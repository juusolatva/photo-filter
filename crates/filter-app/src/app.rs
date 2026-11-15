use crate::preview::PreviewCallback;
use eframe::egui;
use filter_core::ImageBuffer;
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

pub struct PhotoApp {
    renderer: Arc<ExposureRenderer>,
    source: Option<Arc<DecodedImage>>,
    texture: Option<SourceTexture>,
    load_tx: mpsc::Sender<(u64, PathBuf)>,
    load_rx: mpsc::Receiver<LoadResult>,
    dialog_tx: mpsc::Sender<Option<PathBuf>>,
    dialog_rx: mpsc::Receiver<Option<PathBuf>>,
    dialog_open: bool,
    generation: u64,
    loading: bool,
    status: String,
    ev: f32,
    zoom: f32,
    pan: egui::Vec2,
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
            ev: 0.0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };
        if let Some(path) = path {
            app.open(path);
        }
        app
    }
    fn open(&mut self, path: PathBuf) {
        self.generation += 1;
        self.loading = true;
        self.status = format!("Loading {}…", path.display());
        let _ = self.load_tx.send((self.generation, path));
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
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }
}
impl eframe::App for PhotoApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(path) = self.dialog_rx.try_recv() {
            self.dialog_open = false;
            if let Some(path) = path {
                self.open(path);
            }
        }
        let rs = frame.wgpu_render_state().expect("wgpu renderer configured");
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
                    self.ev = 0.0;
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
            self.open(path);
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
        egui::Panel::right("adjustments")
            .default_size(220.0)
            .show(ui, |ui| {
                ui.heading("Basic adjustments");
                ui.add_enabled(
                    self.source.is_some(),
                    egui::Slider::new(&mut self.ev, -5.0..=5.0).text("Exposure EV"),
                );
                if ui.button("Reset exposure").clicked() {
                    self.ev = 0.0;
                }
                ui.separator();
                ui.label("Phase 0 · GPU exposure preview");
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_gray(24));
            if let (Some(source), Some(texture)) = (&self.source, &self.texture) {
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
                        frame: self.renderer.frame_region(
                            &rs.device,
                            texture,
                            self.ev,
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
                    "Drop an image here",
                    egui::FontId::proportional(22.0),
                    egui::Color32::GRAY,
                );
            }
        });
        let _ = rs.device.poll(wgpu::PollType::Poll);
        if self.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
