use crate::export::{ExportFormat, ExportJob, ExportResult};
use crate::preset_ui::{Action as PresetAction, PresetUi};
use crate::preview::PreviewCallback;
use crate::resize::ExportResize;
use eframe::egui;
use filter_core::{
    ImageBuffer,
    cpu_eval::BasicAdjustments,
    edit::{EditState, ModelError},
    preset::Preset,
};
use filter_gpu::{ExposureRenderer, SourceTexture};
use filter_io::DecodedImage;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

type LoadResult = (
    u64,
    PathBuf,
    Result<(Arc<DecodedImage>, ImageBuffer), String>,
);

enum DialogResult {
    Open(Option<PathBuf>),
    PresetLoad {
        generation: u64,
        result: Option<Result<Box<Preset>, String>>,
    },
    PresetSave(Option<Result<PathBuf, String>>),
    Export {
        path: Option<PathBuf>,
        source: Arc<DecodedImage>,
        adjustments: BasicAdjustments,
        format: ExportFormat,
        resize: ExportResize,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ExportKind {
    #[default]
    Png,
    Jpeg,
    WebP,
    Tiff,
}

pub struct PhotoApp {
    renderer: Arc<ExposureRenderer>,
    source: Option<Arc<DecodedImage>>,
    texture: Option<SourceTexture>,
    load_tx: mpsc::Sender<(u64, PathBuf)>,
    load_rx: mpsc::Receiver<LoadResult>,
    dialog_tx: mpsc::Sender<DialogResult>,
    dialog_rx: mpsc::Receiver<DialogResult>,
    dialog_open: bool,
    generation: u64,
    loading: bool,
    status: String,
    edit: EditState,
    adjustments: Result<BasicAdjustments, ModelError>,
    presets: PresetUi,
    zoom: f32,
    pan: egui::Vec2,
    export: Option<ExportJob>,
    export_status: String,
    export_kind: ExportKind,
    jpeg_options: filter_io::JpegOptions,
    png_options: filter_io::PngOptions,
    webp_options: filter_io::WebPOptions,
    tiff_options: filter_io::TiffOptions,
    export_resize: ExportResize,
}
impl PhotoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("wgpu renderer configured");
        let renderer = Arc::new(ExposureRenderer::new(&rs.device, rs.target_format));
        let (load_tx, jobs) = mpsc::channel::<(u64, PathBuf)>();
        let (results, load_rx) = mpsc::channel();
        let (dialog_tx, dialog_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let max_edge = 2048.min(rs.device.limits().max_texture_dimension_2d);
        std::thread::spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                // Bounded active work: coalesce requests queued during the previous decode.
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                let (generation, path) = job;
                let result = filter_io::decode(&path)
                    .map(|image| {
                        let proxy = image.buffer.proxy(max_edge);
                        (Arc::new(image), proxy)
                    })
                    .map_err(|e| format!("{e:#}"));
                if results.send((generation, path, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let mut app = Self {
            renderer,
            source: None,
            texture: None,
            load_tx,
            load_rx,
            dialog_tx,
            dialog_rx,
            dialog_open: false,
            generation: 0,
            loading: false,
            status: "Open a JPEG, PNG, WebP or TIFF to begin".into(),
            edit: EditState::default(),
            adjustments: Ok(BasicAdjustments::default()),
            presets: PresetUi::default(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            export: None,
            export_status: String::new(),
            export_kind: ExportKind::default(),
            jpeg_options: filter_io::JpegOptions::default(),
            png_options: filter_io::PngOptions::default(),
            webp_options: filter_io::WebPOptions::default(),
            tiff_options: filter_io::TiffOptions::default(),
            export_resize: ExportResize::default(),
        };
        if let Some(path) = path {
            app.open(path);
        }
        app
    }
    /// Edit replacement refreshes the immutable renderer snapshot.
    fn replace_edit(&mut self, edit: EditState) {
        self.adjustments = BasicAdjustments::new(&edit);
        self.edit = edit;
    }
    fn open(&mut self, path: PathBuf) {
        self.generation += 1;
        self.loading = true;
        self.status = format!("Loading {}…", path.display());
        let _ = self.load_tx.send((self.generation, path));
    }
    fn load_preset(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        if self.dialog_open {
            self.presets.status =
                "Finish the current file operation before loading a preset.".into();
            return;
        }
        if self.loading {
            self.presets.status = "Wait for the image to finish loading.".into();
            return;
        }
        if self.source.is_none() {
            self.presets.status = "Open an image before loading a preset.".into();
            return;
        }
        self.dialog_open = true;
        let generation = self.generation;
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        self.presets.status = "Loading preset…".into();
        std::thread::spawn(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("Photo Filter preset", &["json"])
                    .pick_file()
            });
            let result = path.map(|path| {
                filter_io::preset::read(&path)
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}"))
            });
            let _ = tx.send(DialogResult::PresetLoad { generation, result });
            ctx.request_repaint();
        });
    }
    fn save_preset(&mut self, ctx: &egui::Context, preset: Preset) {
        if self.dialog_open {
            return;
        }
        self.dialog_open = true;
        self.presets.status = "Saving captured preset…".into();
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .add_filter("Photo Filter preset", &["json"])
                .set_file_name("edited.pfpreset.json")
                .save_file()
                .map(|path| {
                    filter_io::preset::write(&path, &preset)
                        .map(|()| path)
                        .map_err(|e| format!("{e:#}"))
                });
            let _ = tx.send(DialogResult::PresetSave(result));
            ctx.request_repaint();
        });
    }
    fn dialog(&mut self, ctx: &egui::Context) {
        if self.dialog_open {
            return;
        }
        self.dialog_open = true;
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("Images", &["jpg", "jpeg", "png", "webp", "tif", "tiff"])
                .pick_file();
            let _ = tx.send(DialogResult::Open(path));
            ctx.request_repaint();
        });
    }
    fn export_dialog(&mut self, ctx: &egui::Context) {
        if self.dialog_open || self.export.is_some() {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        let adjustments = match &self.adjustments {
            Ok(settings) => settings.clone(),
            Err(error) => {
                self.export_status = format!("Cannot export these adjustments: {error}");
                return;
            }
        };
        let format = match self.export_kind {
            ExportKind::Jpeg => ExportFormat::Jpeg(self.jpeg_options),
            ExportKind::Png => ExportFormat::Png(self.png_options),
            ExportKind::WebP => ExportFormat::WebP(self.webp_options),
            ExportKind::Tiff => ExportFormat::Tiff(self.tiff_options),
        };
        let resize = self.export_resize;
        self.dialog_open = true;
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let extensions: &[&str] = match format {
                ExportFormat::Png(_) => &["png"],
                ExportFormat::Jpeg(_) => &["jpg", "jpeg"],
                ExportFormat::WebP(_) => &["webp"],
                ExportFormat::Tiff(_) => &["tif", "tiff"],
            };
            let path = rfd::FileDialog::new()
                .add_filter(format.label(), extensions)
                .set_file_name(format!("edited.{}", format.extension()))
                .save_file();
            let _ = tx.send(DialogResult::Export {
                path,
                source,
                adjustments,
                format,
                resize,
            });
            ctx.request_repaint();
        });
    }
}
impl eframe::App for PhotoApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let rs = frame.wgpu_render_state().expect("wgpu renderer configured");
        while let Ok(result) = self.dialog_rx.try_recv() {
            self.dialog_open = false;
            match result {
                DialogResult::Open(Some(path)) => self.open(path),
                DialogResult::PresetLoad { generation, result } => {
                    self.presets
                        .finish_load(generation == self.generation, result);
                }
                DialogResult::PresetSave(result) => {
                    self.presets.status = match result {
                        Some(Ok(path)) => format!("Saved preset {}", path.display()),
                        Some(Err(error)) => format!("Could not save preset: {error}"),
                        None => "Preset saving cancelled".into(),
                    };
                }
                DialogResult::Export {
                    path: Some(path),
                    source,
                    adjustments,
                    format,
                    resize,
                } => {
                    match ExportJob::start(
                        rs.device.clone(),
                        rs.queue.clone(),
                        source,
                        adjustments,
                        path,
                        format,
                        resize,
                        ctx.clone(),
                    ) {
                        Ok(job) => {
                            self.export = Some(job);
                            self.export_status.clear();
                        }
                        Err(error) => {
                            self.export_status = format!("Could not start export: {error:#}")
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(result) = self.export.as_ref().and_then(ExportJob::try_result) {
            self.export = None;
            self.export_status = match result {
                ExportResult::Saved(path) => format!("Exported {}", path.display()),
                ExportResult::Cancelled => "Export cancelled".into(),
                ExportResult::Failed(error) => format!("Export failed: {error}"),
            };
        }
        while let Ok((generation, path, result)) = self.load_rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.loading = false;
            match result {
                Ok((source, proxy)) => {
                    self.texture = Some(self.renderer.upload(&rs.device, &rs.queue, &proxy));
                    self.status = format!(
                        "{} · {} × {} · preview {} × {}",
                        path.display(),
                        source.buffer.width(),
                        source.buffer.height(),
                        proxy.width(),
                        proxy.height()
                    );
                    self.source = Some(source);
                    self.presets = PresetUi::default();
                    self.replace_edit(EditState::default());
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                }
                Err(error) => self.status = format!("Could not open {}: {error}", path.display()),
            }
        }
        if let Some(path) = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .next()
        }) {
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                self.load_preset(&ctx, Some(path));
            } else {
                self.open(path);
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            self.dialog(&ctx);
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.dialog_open, egui::Button::new("Open image…"))
                    .clicked()
                {
                    self.dialog(&ctx);
                }
                if ui.button("Fit").clicked() {
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                }
                ui.label("Scroll to zoom · drag to pan");
                if self.loading {
                    ui.spinner();
                }
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.status);
        });
        let mut adjustments_changed = false;
        egui::Panel::right("adjustments")
            .default_size(220.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Basic adjustments");
                    ui.add_enabled_ui(self.source.is_some(), |ui| {
                        adjustments_changed |= ui
                            .add(
                                egui::Slider::new(&mut self.edit.calibration.exposure, -5.0..=5.0)
                                    .text("Exposure EV"),
                            )
                            .changed();
                        for (label, value) in [
                            ("Temperature", &mut self.edit.calibration.temperature),
                            ("Tint", &mut self.edit.calibration.tint),
                            ("Brightness", &mut self.edit.tone.brightness),
                            ("Contrast", &mut self.edit.tone.contrast),
                            ("Highlights", &mut self.edit.tone.highlights),
                            ("Shadows", &mut self.edit.tone.shadows),
                            ("Whites", &mut self.edit.tone.whites),
                            ("Blacks", &mut self.edit.tone.blacks),
                            ("Vibrance", &mut self.edit.color.scalars.vibrance),
                            ("Saturation", &mut self.edit.color.scalars.saturation),
                        ] {
                            adjustments_changed |= ui
                                .add(egui::Slider::new(value, -1.0..=1.0).text(label))
                                .changed();
                        }
                        if ui.button("Reset adjustments").clicked() {
                            self.edit.reset_basic();
                            adjustments_changed = true;
                        }
                    });
                    if adjustments_changed {
                        self.adjustments = BasicAdjustments::new(&self.edit);
                    }
                    ui.separator();
                    if let Some(action) = self.presets.show(
                        ui,
                        &self.edit,
                        self.source.is_some() && !self.loading && !self.dialog_open,
                    ) {
                        match action {
                            PresetAction::Load => self.load_preset(&ctx, None),
                            PresetAction::Save(preset) => self.save_preset(&ctx, *preset),
                            PresetAction::Apply(edit) => self.replace_edit(*edit),
                        }
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Format");
                        ui.selectable_value(&mut self.export_kind, ExportKind::Png, "PNG");
                        ui.selectable_value(&mut self.export_kind, ExportKind::Jpeg, "JPEG");
                        ui.selectable_value(&mut self.export_kind, ExportKind::WebP, "WebP");
                        ui.selectable_value(&mut self.export_kind, ExportKind::Tiff, "TIFF");
                    });
                    if self.export_kind == ExportKind::Jpeg {
                        ui.add(
                            egui::Slider::new(&mut self.jpeg_options.quality, 1..=100)
                                .text("Quality"),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Background");
                            ui.color_edit_button_srgb(&mut self.jpeg_options.background);
                        });
                        let mut dither = self.jpeg_options.dither_seed.is_some();
                        if ui.checkbox(&mut dither, "Dither 8-bit output").changed() {
                            self.jpeg_options.dither_seed = dither.then_some(0);
                        }
                        ui.label("8-bit sRGB · flattened alpha");
                    } else if self.export_kind == ExportKind::WebP {
                        let mut dither = self.webp_options.dither_seed.is_some();
                        if ui.checkbox(&mut dither, "Dither 8-bit output").changed() {
                            self.webp_options.dither_seed = dither.then_some(0);
                        }
                        ui.label("Lossless 8-bit sRGB · preserves alpha");
                    } else if self.export_kind == ExportKind::Tiff {
                        ui.horizontal(|ui| {
                            ui.label("Bit depth");
                            ui.selectable_value(
                                &mut self.tiff_options.bit_depth,
                                filter_io::TiffBitDepth::Eight,
                                "8-bit",
                            );
                            ui.selectable_value(
                                &mut self.tiff_options.bit_depth,
                                filter_io::TiffBitDepth::Sixteen,
                                "16-bit",
                            );
                        });
                        if self.tiff_options.bit_depth == filter_io::TiffBitDepth::Eight {
                            let mut dither = self.tiff_options.dither_seed.is_some();
                            if ui.checkbox(&mut dither, "Dither 8-bit output").changed() {
                                self.tiff_options.dither_seed = dither.then_some(0);
                            }
                        }
                        ui.label("sRGB · preserves alpha");
                        ui.label("Uncompressed · camera metadata omitted");
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Bit depth");
                            ui.selectable_value(
                                &mut self.png_options.bit_depth,
                                filter_io::PngBitDepth::Eight,
                                "8-bit",
                            );
                            ui.selectable_value(
                                &mut self.png_options.bit_depth,
                                filter_io::PngBitDepth::Sixteen,
                                "16-bit",
                            );
                        });
                        if self.png_options.bit_depth == filter_io::PngBitDepth::Eight {
                            let mut dither = self.png_options.dither_seed.is_some();
                            if ui.checkbox(&mut dither, "Dither 8-bit output").changed() {
                                self.png_options.dither_seed = dither.then_some(0);
                            }
                        }
                        ui.label("sRGB · preserves alpha");
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Size");
                        ui.selectable_value(&mut self.export_resize, ExportResize::Full, "Full");
                        if ui
                            .selectable_label(
                                matches!(self.export_resize, ExportResize::LongEdge(_)),
                                "Long edge",
                            )
                            .clicked()
                            && !matches!(self.export_resize, ExportResize::LongEdge(_))
                        {
                            self.export_resize = ExportResize::LongEdge(2048);
                        }
                        if ui
                            .selectable_label(
                                matches!(self.export_resize, ExportResize::Percentage(_)),
                                "Percentage",
                            )
                            .clicked()
                            && !matches!(self.export_resize, ExportResize::Percentage(_))
                        {
                            self.export_resize = ExportResize::Percentage(50);
                        }
                    });
                    match &mut self.export_resize {
                        ExportResize::Full => {}
                        ExportResize::LongEdge(edge) => {
                            ui.add(egui::DragValue::new(edge).range(1..=u32::MAX).suffix(" px"));
                            ui.label("Reduce only; smaller images retain their size.");
                        }
                        ExportResize::Percentage(percent) => {
                            ui.add(egui::Slider::new(percent, 1..=100).suffix("%"));
                        }
                    }
                    if let Some(source) = &self.source
                        && let Ok((width, height)) = self
                            .export_resize
                            .dimensions(source.buffer.width(), source.buffer.height())
                    {
                        ui.label(format!("Output: {width} × {height} px"));
                    }
                    if ui
                        .add_enabled(
                            self.source.is_some()
                                && self.adjustments.is_ok()
                                && !self.dialog_open
                                && self.export.is_none(),
                            egui::Button::new("Export…"),
                        )
                        .clicked()
                    {
                        self.export_dialog(&ctx);
                    }
                    if let Some(export) = &self.export {
                        ui.add(egui::ProgressBar::new(export.progress()).show_percentage());
                        ui.label("Rendering and encoding captured adjustments");
                        if ui
                            .add_enabled(
                                !export.is_cancelling(),
                                egui::Button::new("Cancel export"),
                            )
                            .clicked()
                        {
                            export.cancel();
                        }
                        if export.is_cancelling() {
                            ui.label("Cancelling…");
                        }
                    }
                    if let Err(error) = &self.adjustments {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("Cannot preview these adjustments: {error}"),
                        );
                    }
                    if !self.export_status.is_empty() {
                        ui.label(&self.export_status);
                    }
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_gray(24));
            if let (Some(source), Some(texture), Ok(adjustments)) =
                (&self.source, &self.texture, &self.adjustments)
            {
                let dims = egui::vec2(source.buffer.width() as f32, source.buffer.height() as f32);
                let fit = (rect.width() / dims.x).min(rect.height() / dims.y);
                if response.dragged() {
                    self.pan += response.drag_delta();
                }
                if response.hovered() {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        let old = self.zoom;
                        self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.1, 32.0);
                        if let Some(pointer) = response.hover_pos() {
                            let offset = pointer - rect.center();
                            self.pan = offset - (offset - self.pan) * (self.zoom / old);
                        }
                    }
                }
                let image_rect =
                    egui::Rect::from_center_size(rect.center() + self.pan, dims * fit * self.zoom);
                let visible = image_rect.intersect(rect);
                if visible.is_positive() {
                    let origin = (visible.min - image_rect.min) / image_rect.size();
                    let extent = visible.size() / image_rect.size();
                    let callback = PreviewCallback {
                        renderer: self.renderer.clone(),
                        frame: self.renderer.frame_region_adjusted(
                            &rs.device,
                            texture,
                            adjustments,
                            [origin.x, origin.y, extent.x, extent.y],
                        ),
                    };
                    ui.painter()
                        .with_clip_rect(rect)
                        .add(egui_wgpu::Callback::new_paint_callback(visible, callback));
                }
            } else {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if self.source.is_some() {
                        "Adjustments cannot be previewed"
                    } else {
                        "Drop an image here"
                    },
                    egui::FontId::proportional(22.0),
                    egui::Color32::GRAY,
                );
            }
        });
        let _ = rs.device.poll(wgpu::PollType::Poll);
        if self.loading || self.export.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
