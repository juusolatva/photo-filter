use crate::channel_ui::ChannelUi;
use crate::clipping::{self, ClippingOverlay};
use crate::compare::Compare;
use crate::curve_ui::CurveUi;
use crate::export::{ExportFormat, ExportJob, ExportResult};
use crate::eyedropper::Eyedropper;
use crate::grading_ui::GradingUi;
use crate::histogram_ui::HistogramUi;
use crate::history::History;
use crate::lut_ui::{Actions as LutActions, Loaded as LoadedLut, LutUi};
use crate::mixer_ui::MixerUi;
use crate::preset_ui::{Action as PresetAction, PresetUi};
use crate::preview::{PreviewCallback, RegionPreview, needs_regions};
use crate::probe_ui::{ProbeUi, source_pixel};
use crate::resize::ExportResize;
use crate::view::{Placement, ViewInput, ViewState};
use eframe::egui;
use filter_core::{
    ImageBuffer,
    cpu_eval::BasicAdjustments,
    edit::{EditState, ModelError},
    preset::Preset,
};
use filter_gpu::{ExposureFrame, ExposureRenderer, ExposureUpdates, SourceTexture};
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
    LutLoad {
        generation: u64,
        result: Option<Result<Box<LoadedLut>, String>>,
    },
    Export(Box<ExportRequest>),
}

/// Export settings captured when the save dialog opened; no path = cancelled.
struct ExportRequest {
    path: Option<PathBuf>,
    source: Arc<DecodedImage>,
    adjustments: BasicAdjustments,
    format: ExportFormat,
    resize: ExportResize,
}

/// Viewport rendering state for the current proxy and full-resolution tiles.
struct Preview {
    renderer: Arc<ExposureRenderer>,
    texture: Option<SourceTexture>,
    frame: Option<ExposureFrame>,
    updates: ExposureUpdates,
    regions: RegionPreview,
    proxy_scale: f32,
    /// Physical x left of which the original is shown; see `Compare`.
    before_until: f32,
    /// Viewport-only clipping overlay; see `ClippingOverlay`.
    clipping: ClippingOverlay,
}
impl Preview {
    /// Upload a new proxy and drop every frame/tile tied to the old image.
    fn replace_source(
        &mut self,
        rs: &egui_wgpu::RenderState,
        source: &ImageBuffer,
        proxy: &ImageBuffer,
    ) {
        self.regions.reset();
        self.frame = None;
        self.proxy_scale = (proxy.width() as f32 / source.width() as f32)
            .min(proxy.height() as f32 / source.height() as f32);
        self.texture = Some(self.renderer.upload(&rs.device, &rs.queue, proxy));
    }

    /// Paint the proxy, plus refined full-resolution tiles at high zoom.
    fn paint(
        &mut self,
        ui: &egui::Ui,
        device: &wgpu::Device,
        viewport: egui::Rect,
        placement: Placement,
        source: &Arc<DecodedImage>,
        adjustments: &BasicAdjustments,
    ) {
        let Placement {
            image,
            visible,
            physical_scale,
        } = placement;
        let Some(texture) = &self.texture else {
            return;
        };
        if !visible.is_positive() {
            return;
        }
        let dims = egui::vec2(source.buffer.width() as f32, source.buffer.height() as f32);
        let origin = (visible.min - image.min) / image.size();
        let extent = visible.size() / image.size();
        let uv = [origin.x, origin.y, extent.x, extent.y];
        let frame = self.frame.get_or_insert_with(|| {
            self.renderer
                .frame_region_adjusted(device, texture, adjustments, uv)
        });
        self.renderer.update_frame(
            &mut self.updates,
            frame,
            adjustments,
            uv,
            [0.0, 0.0, 1.0, 1.0],
        );
        let mut frames = vec![frame.clone()];
        if needs_regions(self.proxy_scale, physical_scale) {
            frames.extend(self.regions.frames(
                &self.renderer,
                device,
                &mut self.updates,
                source,
                egui::Rect::from_min_size(
                    egui::pos2(origin.x * dims.x, origin.y * dims.y),
                    egui::vec2(extent.x * dims.x, extent.y * dims.y),
                ),
                adjustments,
            ));
        }
        self.updates.flush();
        let callback = PreviewCallback {
            renderer: self.renderer.clone(),
            frames,
            viewport: visible,
            before_until: self.before_until,
            clipping: self.clipping,
            adjustments: adjustments.clone(),
        };
        ui.painter()
            .with_clip_rect(viewport)
            .add(egui_wgpu::Callback::new_paint_callback(visible, callback));
    }
}

/// Reserve fixed-height panel space for a readout painted later in the frame.
fn reserve(ui: &mut egui::Ui, height: f32) -> egui::Rect {
    ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    )
    .0
}

fn dither_checkbox(ui: &mut egui::Ui, seed: &mut Option<u64>) {
    let mut dither = seed.is_some();
    if ui.checkbox(&mut dither, "Dither 8-bit output").changed() {
        *seed = dither.then_some(0);
    }
}

fn bit_depth_selector<T: PartialEq>(ui: &mut egui::Ui, depth: &mut T, eight: T, sixteen: T) {
    ui.horizontal(|ui| {
        ui.label("Bit depth");
        ui.selectable_value(depth, eight, "8-bit");
        ui.selectable_value(depth, sixteen, "16-bit");
    });
}

const UNDO: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
const REDO: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::Z,
);
const REDO_ALT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ExportKind {
    #[default]
    Png,
    Jpeg,
    WebP,
    Tiff,
}

pub struct PhotoApp {
    source: Option<Arc<DecodedImage>>,
    preview: Preview,
    histogram: HistogramUi,
    probe: ProbeUi,
    load_tx: mpsc::Sender<(u64, PathBuf)>,
    load_rx: mpsc::Receiver<LoadResult>,
    dialog_tx: mpsc::Sender<DialogResult>,
    dialog_rx: mpsc::Receiver<DialogResult>,
    dialog_open: bool,
    generation: u64,
    loading: bool,
    status: String,
    edit: EditState,
    /// Changes not yet settled into `history`, e.g. during a slider drag.
    edit_dirty: bool,
    history: History,
    adjustments: Result<BasicAdjustments, ModelError>,
    presets: PresetUi,
    curve_ui: CurveUi,
    /// LUT status and file name; the LUT itself is part of the edit.
    lut_ui: LutUi,
    /// Selected mixer tab; not part of the edit.
    mixer_ui: MixerUi,
    /// Selected grading wheel tab; not part of the edit.
    grading_ui: GradingUi,
    /// Channel mixer panel; stateless, kept for symmetry with the other sections.
    channel_ui: ChannelUi,
    view: ViewState,
    compare: Compare,
    /// Transient white-balance picker; not part of the edit.
    eyedropper: Eyedropper,
    /// Persists across image loads for the session; not part of the edit.
    clipping: ClippingOverlay,
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
        Self::with_render_state(&cc.egui_ctx, rs, path)
    }
    /// Construct against an existing context and device; tests use a headless
    /// context with a software adapter.
    fn with_render_state(
        egui_ctx: &egui::Context,
        rs: &egui_wgpu::RenderState,
        path: Option<PathBuf>,
    ) -> Self {
        let renderer = Arc::new(ExposureRenderer::new(&rs.device, rs.target_format));
        let regions = RegionPreview::new(
            renderer.clone(),
            rs.device.clone(),
            rs.queue.clone(),
            egui_ctx.clone(),
        );
        let (load_tx, jobs) = mpsc::channel::<(u64, PathBuf)>();
        let (results, load_rx) = mpsc::channel();
        let (dialog_tx, dialog_rx) = mpsc::channel();
        let ctx = egui_ctx.clone();
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
            source: None,
            preview: Preview {
                renderer,
                texture: None,
                frame: None,
                updates: ExposureUpdates::new(&rs.device, &rs.queue),
                regions,
                proxy_scale: 1.0,
                before_until: 0.0,
                clipping: ClippingOverlay::default(),
            },
            histogram: HistogramUi::new(&rs.device),
            probe: ProbeUi::new(&rs.device),
            load_tx,
            load_rx,
            dialog_tx,
            dialog_rx,
            dialog_open: false,
            generation: 0,
            loading: false,
            status: "Open a JPEG, PNG, WebP or TIFF to begin".into(),
            edit: EditState::default(),
            edit_dirty: false,
            history: History::new(&EditState::default()),
            adjustments: Ok(BasicAdjustments::default()),
            presets: PresetUi::default(),
            curve_ui: CurveUi::default(),
            lut_ui: LutUi::default(),
            mixer_ui: MixerUi::default(),
            grading_ui: GradingUi::default(),
            channel_ui: ChannelUi,
            view: ViewState::default(),
            compare: Compare::default(),
            eyedropper: Eyedropper::default(),
            clipping: ClippingOverlay::default(),
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
    /// Record pending edit changes as one undo step once no pointer button or
    /// key is held, so a slider drag or a held arrow key settles as one step.
    fn settle_history(&mut self, ctx: &egui::Context) {
        let gesture = ctx.input(|i| i.pointer.any_down() || !i.keys_down.is_empty());
        if self.edit_dirty && !gesture {
            self.history.commit(&self.edit);
            self.edit_dirty = false;
        }
    }
    fn can_undo(&self) -> bool {
        self.history.can_undo() || self.edit_dirty
    }
    fn can_redo(&self) -> bool {
        self.history.can_redo() && !self.edit_dirty
    }
    /// Undo or redo, first settling any unrecorded change as its own step.
    fn step_history(&mut self, redo: bool) {
        let edit = if redo {
            self.history.redo(&self.edit)
        } else {
            self.history.undo(&self.edit)
        };
        self.edit_dirty = false;
        if let Some(edit) = edit {
            self.replace_edit(edit);
        }
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
    fn load_lut(&mut self, ctx: &egui::Context) {
        if self.dialog_open || self.loading || self.source.is_none() {
            self.lut_ui.status = "Finish the current file operation before loading a LUT.".into();
            return;
        }
        self.dialog_open = true;
        let generation = self.generation;
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        self.lut_ui.loading();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .add_filter("Cube LUT", &["cube"])
                .pick_file()
                .map(|path| {
                    filter_io::lut::read_cube(&path)
                        .map(|lut| {
                            let file_name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            Box::new(LoadedLut { lut, file_name })
                        })
                        .map_err(|e| format!("{e:#}"))
                });
            let _ = tx.send(DialogResult::LutLoad { generation, result });
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
            let _ = tx.send(DialogResult::Export(Box::new(ExportRequest {
                path,
                source,
                adjustments,
                format,
                resize,
            })));
            ctx.request_repaint();
        });
    }
    fn handle_dialog(
        &mut self,
        result: DialogResult,
        ctx: &egui::Context,
        rs: &egui_wgpu::RenderState,
    ) {
        match result {
            DialogResult::Open(Some(path)) => self.open(path),
            DialogResult::Open(None) => {}
            DialogResult::PresetLoad { generation, result } => {
                self.presets
                    .finish_load(generation == self.generation, result);
            }
            DialogResult::LutLoad { generation, result } => {
                let same_image = generation == self.generation;
                if self
                    .lut_ui
                    .finish_load(same_image, result, &mut self.edit.finishing)
                {
                    self.edit_changed();
                }
            }
            DialogResult::PresetSave(result) => {
                self.presets.status = match result {
                    Some(Ok(path)) => format!("Saved preset {}", path.display()),
                    Some(Err(error)) => format!("Could not save preset: {error}"),
                    None => "Preset saving cancelled".into(),
                };
            }
            DialogResult::Export(request) => self.start_export(*request, ctx, rs),
        }
    }
    fn start_export(
        &mut self,
        request: ExportRequest,
        ctx: &egui::Context,
        rs: &egui_wgpu::RenderState,
    ) {
        let Some(path) = request.path else {
            return;
        };
        match ExportJob::start(
            rs.device.clone(),
            rs.queue.clone(),
            request.source,
            request.adjustments,
            path,
            request.format,
            request.resize,
            ctx.clone(),
        ) {
            Ok(job) => {
                self.export = Some(job);
                self.export_status.clear();
            }
            Err(error) => self.export_status = format!("Could not start export: {error:#}"),
        }
    }
    fn poll_export(&mut self) {
        if let Some(result) = self.export.as_ref().and_then(ExportJob::try_result) {
            self.export = None;
            self.export_status = match result {
                ExportResult::Saved(path) => format!("Exported {}", path.display()),
                ExportResult::Cancelled => "Export cancelled".into(),
                ExportResult::Failed(error) => format!("Export failed: {error}"),
            };
        }
    }
    /// Apply decode results for the current generation; older ones are dropped.
    fn poll_loads(&mut self, rs: &egui_wgpu::RenderState) {
        while let Ok((generation, path, result)) = self.load_rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.loading = false;
            match result {
                Ok((source, proxy)) => self.finish_load(rs, &path, source, &proxy),
                Err(error) => self.status = format!("Could not open {}: {error}", path.display()),
            }
        }
    }
    fn finish_load(
        &mut self,
        rs: &egui_wgpu::RenderState,
        path: &std::path::Path,
        source: Arc<DecodedImage>,
        proxy: &ImageBuffer,
    ) {
        self.preview.replace_source(rs, &source.buffer, proxy);
        self.histogram.reset();
        self.probe.reset();
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
        self.lut_ui.reset();
        self.replace_edit(EditState::default());
        self.history = History::new(&self.edit);
        self.edit_dirty = false;
        self.view = ViewState::default();
        self.eyedropper.reset();
    }
    /// Dropped presets load; any other dropped file opens as an image.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
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
                self.load_preset(ctx, Some(path));
            } else {
                self.open(path);
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            self.dialog(ctx);
        }
        // Holding Space shows the original. Consuming its presses also keeps
        // Space from clicking a focused button, such as Reset adjustments.
        let keys_free = !ctx.text_edit_focused();
        self.compare.held = keys_free
            && self.source.is_some()
            && ctx.input_mut(|i| {
                i.events.retain(|event| {
                    !matches!(
                        event,
                        egui::Event::Key {
                            key: egui::Key::Space,
                            pressed: true,
                            ..
                        }
                    )
                });
                i.key_down(egui::Key::Space)
            });
        if keys_free {
            self.eyedropper.handle_keys(ctx, self.source.is_some());
            if crate::keys::consume_initial_press(ctx, clipping::TOGGLE_KEY) {
                self.clipping.toggle_all();
            }
            // Text fields keep their own undo; match Shift+Z before plain Z.
            if ctx.input_mut(|i| i.consume_shortcut(&REDO) || i.consume_shortcut(&REDO_ALT)) {
                self.step_history(true);
            } else if ctx.input_mut(|i| i.consume_shortcut(&UNDO)) {
                self.step_history(false);
            }
        }
    }
    /// Returns whether 100% was clicked; the canvas applies it after layout.
    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.dialog_open, egui::Button::new("Open image…"))
                .clicked()
            {
                self.dialog(ctx);
            }
            let undo = format!("Undo ({})", ctx.format_shortcut(&UNDO));
            if ui
                .add_enabled(self.can_undo(), egui::Button::new("Undo"))
                .on_hover_text(undo)
                .clicked()
            {
                self.step_history(false);
            }
            let redo = format!(
                "Redo ({} or {})",
                ctx.format_shortcut(&REDO),
                ctx.format_shortcut(&REDO_ALT)
            );
            if ui
                .add_enabled(self.can_redo(), egui::Button::new("Redo"))
                .on_hover_text(redo)
                .clicked()
            {
                self.step_history(true);
            }
            ui.separator();
            if ui.button("Fit").clicked() {
                self.view.fit();
            }
            let native_zoom = ui
                .add_enabled(self.source.is_some(), egui::Button::new("100%"))
                .on_hover_text("One source pixel per physical display pixel")
                .clicked();
            let split = self.compare.split.is_some();
            if ui
                .add_enabled(
                    self.source.is_some(),
                    egui::Button::selectable(split, "Before/after"),
                )
                .on_hover_text("Split view with a draggable divider · hold Space for the original")
                .clicked()
            {
                self.compare.split = (!split).then_some(Compare::DEFAULT_SPLIT);
            }
            ui.label("Scroll to zoom · drag to pan");
            if self.loading {
                ui.spinner();
            }
            native_zoom
        })
        .inner
    }
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.label(&self.status);
        if let Some(error) = &self.preview.regions.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
            if self.preview.regions.can_retry() && ui.button("Retry preview").clicked() {
                // Release cached textures as well, so an allocation failure
                // can recover without reopening and decoding the image.
                self.preview.regions.reset();
            }
        }
    }
    /// Returns the reserved probe area, painted after the canvas computes
    /// this frame's cursor target.
    fn side_panel(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rs: &egui_wgpu::RenderState,
    ) -> egui::Ui {
        let histogram_rect = reserve(ui, HistogramUi::HEIGHT);
        self.clipping.show(ui);
        let probe_rect = reserve(ui, ProbeUi::HEIGHT);
        let probe_panel = ui.new_child(egui::UiBuilder::new().max_rect(probe_rect));
        ui.separator();
        self.adjustment_controls(ui);
        ui.separator();
        self.mixer_controls(ui);
        ui.separator();
        self.grading_controls(ui);
        ui.separator();
        self.channel_controls(ui);
        ui.separator();
        self.curve_controls(ui);
        ui.separator();
        self.lut_controls(ui, ctx);
        ui.separator();
        self.preset_controls(ui, ctx);
        ui.separator();
        self.export_format_controls(ui);
        ui.separator();
        self.export_size_controls(ui);
        self.export_controls(ui, ctx);
        self.histogram.refresh(
            &rs.device,
            &rs.queue,
            self.preview.texture.as_ref(),
            self.adjustments.as_ref().ok(),
        );
        self.histogram.show(ui, histogram_rect);
        probe_panel
    }
    fn adjustment_controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Basic adjustments");
        let mut changed = false;
        ui.add_enabled_ui(self.source.is_some(), |ui| {
            // Typed values apply once on Enter or focus loss, as one undo step.
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.edit.calibration.exposure, -5.0..=5.0)
                        .text("Exposure EV")
                        .update_while_editing(false),
                )
                .changed();
            self.eyedropper.show(ui);
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
                changed |= ui
                    .add(
                        egui::Slider::new(value, -1.0..=1.0)
                            .text(label)
                            .update_while_editing(false),
                    )
                    .changed();
            }
            if ui.button("Reset adjustments").clicked() {
                self.edit.reset_basic();
                changed = true;
            }
        });
        if changed {
            self.edit_changed();
        }
    }
    fn mixer_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::CollapsingHeader::new("Color mixer (HSL)").show(ui, |ui| {
            ui.add_enabled_ui(self.source.is_some(), |ui| {
                changed = self.mixer_ui.show(ui, &mut self.edit.color);
            });
        });
        if changed {
            self.edit_changed();
        }
    }
    fn grading_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::CollapsingHeader::new("Color grading").show(ui, |ui| {
            ui.add_enabled_ui(self.source.is_some(), |ui| {
                changed = self.grading_ui.show(ui, &mut self.edit.color);
            });
        });
        if changed {
            self.edit_changed();
        }
    }
    fn channel_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::CollapsingHeader::new("Channel mixer").show(ui, |ui| {
            ui.add_enabled_ui(self.source.is_some(), |ui| {
                changed = self.channel_ui.show(ui, &mut self.edit.color.scalars);
            });
        });
        if changed {
            self.edit_changed();
        }
    }
    fn lut_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let can_load = !self.dialog_open && !self.loading;
        let mut actions = LutActions::default();
        egui::CollapsingHeader::new("Look (LUT)").show(ui, |ui| {
            ui.add_enabled_ui(self.source.is_some(), |ui| {
                actions = self.lut_ui.show(ui, &mut self.edit.finishing, can_load);
            });
        });
        // Apply a change first so a same-frame Load starts from the refreshed snapshot.
        if actions.changed {
            self.edit_changed();
        }
        if actions.load {
            self.load_lut(ctx);
        }
    }
    fn curve_controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Tone curve");
        let mut changed = false;
        ui.add_enabled_ui(self.source.is_some(), |ui| {
            changed = self.curve_ui.show(ui, &mut self.edit.curves);
        });
        if changed {
            self.edit_changed();
        }
    }
    /// Refresh the renderer snapshot after an in-place edit and mark it for
    /// the next undo step.
    fn edit_changed(&mut self) {
        self.adjustments = BasicAdjustments::new(&self.edit);
        self.edit_dirty = true;
    }
    fn preset_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let enabled = self.source.is_some() && !self.loading && !self.dialog_open;
        match self.presets.show(ui, &self.edit, enabled) {
            Some(PresetAction::Load) => self.load_preset(ctx, None),
            Some(PresetAction::Save(preset)) => self.save_preset(ctx, *preset),
            Some(PresetAction::Apply(edit)) => {
                self.replace_edit(*edit);
                self.edit_dirty = true;
            }
            None => {}
        }
    }
    fn export_format_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Format");
            ui.selectable_value(&mut self.export_kind, ExportKind::Png, "PNG");
            ui.selectable_value(&mut self.export_kind, ExportKind::Jpeg, "JPEG");
            ui.selectable_value(&mut self.export_kind, ExportKind::WebP, "WebP");
            ui.selectable_value(&mut self.export_kind, ExportKind::Tiff, "TIFF");
        });
        match self.export_kind {
            ExportKind::Jpeg => {
                ui.add(egui::Slider::new(&mut self.jpeg_options.quality, 1..=100).text("Quality"));
                ui.horizontal(|ui| {
                    ui.label("Background");
                    ui.color_edit_button_srgb(&mut self.jpeg_options.background);
                });
                dither_checkbox(ui, &mut self.jpeg_options.dither_seed);
                ui.label("8-bit sRGB · flattened alpha");
            }
            ExportKind::WebP => {
                dither_checkbox(ui, &mut self.webp_options.dither_seed);
                ui.label("Lossless 8-bit sRGB · preserves alpha");
            }
            ExportKind::Tiff => {
                use filter_io::TiffBitDepth::{Eight, Sixteen};
                let options = &mut self.tiff_options;
                bit_depth_selector(ui, &mut options.bit_depth, Eight, Sixteen);
                if options.bit_depth == Eight {
                    dither_checkbox(ui, &mut options.dither_seed);
                }
                ui.label("sRGB · preserves alpha");
                ui.label("Uncompressed · camera metadata omitted");
            }
            ExportKind::Png => {
                use filter_io::PngBitDepth::{Eight, Sixteen};
                let options = &mut self.png_options;
                bit_depth_selector(ui, &mut options.bit_depth, Eight, Sixteen);
                if options.bit_depth == Eight {
                    dither_checkbox(ui, &mut options.dither_seed);
                }
                ui.label("sRGB · preserves alpha");
            }
        }
    }
    fn export_size_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Size");
            ui.selectable_value(&mut self.export_resize, ExportResize::Full, "Full");
            let long_edge = matches!(self.export_resize, ExportResize::LongEdge(_));
            if ui.selectable_label(long_edge, "Long edge").clicked() && !long_edge {
                self.export_resize = ExportResize::LongEdge(2048);
            }
            let percentage = matches!(self.export_resize, ExportResize::Percentage(_));
            if ui.selectable_label(percentage, "Percentage").clicked() && !percentage {
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
    }
    fn export_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let can_export = self.source.is_some()
            && self.adjustments.is_ok()
            && !self.dialog_open
            && self.export.is_none();
        if ui
            .add_enabled(can_export, egui::Button::new("Export…"))
            .clicked()
        {
            self.export_dialog(ctx);
        }
        if let Some(export) = &self.export {
            ui.add(egui::ProgressBar::new(export.progress()).show_percentage());
            ui.label("Rendering and encoding captured adjustments");
            if ui
                .add_enabled(!export.is_cancelling(), egui::Button::new("Cancel export"))
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
    }
    /// Paints the image and returns the source pixel under the pointer.
    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        device: &wgpu::Device,
        native_zoom: bool,
    ) -> Option<[u32; 2]> {
        let (viewport, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        ui.painter()
            .rect_filled(viewport, 0.0, egui::Color32::from_gray(24));
        let (Some(source), Ok(adjustments), true) = (
            &self.source,
            &self.adjustments,
            self.preview.texture.is_some(),
        ) else {
            ui.painter().text(
                viewport.center(),
                egui::Align2::CENTER_CENTER,
                if self.source.is_some() {
                    "Adjustments cannot be previewed"
                } else {
                    "Drop an image here"
                },
                egui::FontId::proportional(22.0),
                egui::Color32::GRAY,
            );
            return None;
        };
        // Registered above the canvas, so dragging the divider does not pan.
        self.compare.interact(ui, viewport);
        let size = [source.buffer.width(), source.buffer.height()];
        let input = ViewInput {
            native_zoom,
            drag: response.dragged().then(|| response.drag_delta()),
            scroll: if response.hovered() {
                ctx.input(|i| i.smooth_scroll_delta.y)
            } else {
                0.0
            },
            pointer: response.hover_pos(),
        };
        let placement = self.view.place(
            viewport,
            egui::vec2(size[0] as f32, size[1] as f32),
            ctx.pixels_per_point(),
            input,
        )?;
        self.preview.before_until = self.compare.before_until(viewport, ctx.pixels_per_point());
        self.preview.clipping = self.clipping;
        self.preview
            .paint(ui, device, viewport, placement, source, adjustments);
        self.compare.paint(ui, viewport);
        self.eyedropper_canvas(ui, &response, viewport, placement.image, size);
        source_pixel(response.hover_pos(), viewport, placement.image, size)
    }
    /// While picking: show the hint and let a click (not a drag, which pans) on
    /// an image pixel set the white balance. The divider and the Space hold
    /// take precedence over the image.
    fn eyedropper_canvas(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        viewport: egui::Rect,
        image: egui::Rect,
        size: [u32; 2],
    ) {
        if !self.eyedropper.active {
            return;
        }
        let scale = ui.ctx().pixels_per_point();
        let target = |pointer: Option<egui::Pos2>| {
            pointer
                .filter(|p| !self.compare.held && !self.compare.grabs(viewport, scale, *p))
                .and_then(|p| source_pixel(Some(p), viewport, image, size))
        };
        let hovered = target(response.hover_pos()).is_some();
        let clicked = response
            .clicked()
            .then(|| target(response.interact_pointer_pos()))
            .flatten();
        self.eyedropper.paint_hint(ui, viewport, hovered);
        if let Some(xy) = clicked {
            self.pick_white_balance(xy);
        }
    }
    /// Set Temperature and Tint from the source pixel as one undo step.
    fn pick_white_balance(&mut self, xy: [u32; 2]) {
        let Some(source) = self.source.clone() else {
            return;
        };
        if let Some(balance) = self.eyedropper.pick(&source.buffer, xy) {
            self.edit.calibration.temperature = balance.temperature;
            self.edit.calibration.tint = balance.tint;
            self.edit_changed();
        }
    }
    fn schedule_repaint(&self, ctx: &egui::Context) {
        if self.loading || self.export.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.histogram.is_pending() || self.probe.is_pending() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}
impl eframe::App for PhotoApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let rs = frame.wgpu_render_state().expect("wgpu renderer configured");
        self.show(ui, rs);
    }
}
impl PhotoApp {
    /// One frame of the whole window.
    fn show(&mut self, ui: &mut egui::Ui, rs: &egui_wgpu::RenderState) {
        let ctx = ui.ctx().clone();
        while let Ok(result) = self.dialog_rx.try_recv() {
            self.dialog_open = false;
            self.handle_dialog(result, &ctx, rs);
        }
        self.poll_export();
        self.poll_loads(rs);
        // Settle gestures that ended with this frame's input before the
        // toolbar shows undo state; same-frame changes settle at the end.
        self.settle_history(&ctx);
        self.handle_shortcuts(&ctx);
        let native_zoom = egui::Panel::top("toolbar")
            .show(ui, |ui| self.toolbar(ui, &ctx))
            .inner;
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        let mut probe_panel = egui::Panel::right("adjustments")
            .default_size(220.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .show(ui, |ui| self.side_panel(ui, &ctx, rs))
                    .inner
            })
            .inner;
        let probe_coordinate = egui::CentralPanel::default()
            .show(ui, |ui| self.canvas(ui, &ctx, &rs.device, native_zoom))
            .inner;
        self.probe.refresh(
            &rs.device,
            &rs.queue,
            self.source.as_ref().map(|s| &s.buffer),
            self.adjustments.as_ref().ok(),
            probe_coordinate,
        );
        self.probe.show(&mut probe_panel);
        self.settle_history(&ctx);
        let _ = rs.device.poll(wgpu::PollType::Poll);
        self.schedule_repaint(&ctx);
    }
}

#[cfg(test)]
#[path = "../tests/unit/app_tests.rs"]
mod tests;
