//! Headless app-shell checks: real egui frames on a software GPU with queued
//! input and dialog results. Native file dialogs are never opened.
use super::{DialogResult, ExportKind, ExportRequest, PhotoApp};
use crate::export::ExportFormat;
use crate::resize::ExportResize;
use eframe::egui;
use filter_core::{
    edit::EditState,
    preset::{Preset, Sections},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tall enough that the scrolling side panel paints every control.
const SCREEN: egui::Vec2 = egui::vec2(1280.0, 2000.0);
/// Inside the canvas: left of the 220-point side panel, between the bars.
const CANVAS_POINT: egui::Pos2 = egui::pos2(500.0, 1000.0);

fn render_state() -> egui_wgpu::RenderState {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY)
                & wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..Default::default()
            })
            .await
            .expect("software adapter required");
        eprintln!("Software GPU: {:?}", adapter.get_info());
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = egui_wgpu::Renderer::new(&device, target_format, Default::default());
        egui_wgpu::RenderState {
            adapter: adapter.clone(),
            available_adapters: vec![adapter],
            instance,
            device,
            queue,
            target_format,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        }
    })
}

#[derive(Debug)]
struct Dropped(PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// A headless window: each frame runs the whole app against a fixed screen,
/// consuming queued input like eframe would.
struct Window {
    ctx: egui::Context,
    rs: egui_wgpu::RenderState,
    app: PhotoApp,
    events: Vec<egui::Event>,
    dropped: Vec<egui::DroppedFileHandle>,
}
impl Window {
    fn new(path: Option<PathBuf>) -> Self {
        let ctx = egui::Context::default();
        let rs = render_state();
        let app = PhotoApp::with_render_state(&ctx, &rs, path);
        Self {
            ctx,
            rs,
            app,
            events: Vec::new(),
            dropped: Vec::new(),
        }
    }
    /// Run one frame and return every painted text.
    fn frame(&mut self) -> Vec<String> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events: std::mem::take(&mut self.events),
            dropped_files: std::mem::take(&mut self.dropped),
            ..Default::default()
        };
        let output = self.ctx.run_ui(input, |ui| self.app.show(ui, &self.rs));
        let text = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        text
    }
    /// Run frames until `done`, giving background decode/export/GPU work time.
    fn until(&mut self, what: &str, mut done: impl FnMut(&PhotoApp) -> bool) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = self.frame();
            if done(&self.app) {
                return text;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn loaded(&mut self) -> Vec<String> {
        self.until("image load", |app| !app.loading && app.source.is_some())
    }
    fn send(&mut self, result: DialogResult) {
        self.app.dialog_open = true;
        self.app.dialog_tx.send(result).unwrap();
        self.frame();
        assert!(!self.app.dialog_open, "a dialog result closes the dialog");
    }
    fn drop_file(&mut self, path: &Path) {
        self.dropped.push(Arc::new(Dropped(path.to_owned())));
    }
}

fn has(text: &[String], line: &str) -> bool {
    text.iter().any(|s| s == line)
}
fn write_image(path: &Path, width: u32, height: u32) {
    image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            (x % 256) as u8,
            (y * 6 % 256) as u8,
            128,
            if x % 4 == 0 { 0 } else { 255 },
        ])
    })
    .save(path)
    .unwrap();
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn empty_window_invites_an_image() {
    let mut window = Window::new(None);
    let text = window.frame();
    assert!(has(&text, "Drop an image here"));
    assert!(has(&text, "Open a JPEG, PNG, WebP or TIFF to begin"));
    assert!(has(&text, "Basic adjustments"));
    assert!(has(&text, "Hover over the image"));
    assert!(window.app.source.is_none());
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn loaded_image_supports_zoom_pan_native_pixels_and_probe() {
    let directory = tempfile::tempdir().unwrap();
    // Wider than the 2048px proxy, so 100% refines full-resolution tiles.
    let path = directory.path().join("wide.png");
    write_image(&path, 2100, 1050);
    let mut window = Window::new(Some(path));
    let text = window.loaded();
    let status = &window.app.status;
    assert!(
        status.ends_with("2100 × 1050 · preview 2048 × 1024"),
        "{status}"
    );
    assert!(has(&text, "Cursor color · source pixel"));
    assert!(window.app.preview.proxy_scale < 1.0);

    // Hovering samples the source pixel under the pointer.
    window.events.push(egui::Event::PointerMoved(CANVAS_POINT));
    window.until("probe and histogram", |app| {
        !app.probe.is_pending() && !app.histogram.is_pending()
    });
    let text = window.frame();
    assert!(
        text.iter().any(|s| s.starts_with("Pixel (")),
        "probe readout: {text:?}"
    );

    // Scrolling zooms around the pointer; dragging pans.
    window.events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 200.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..30 {
        window.events.push(egui::Event::PointerMoved(CANVAS_POINT));
        window.frame();
    }
    assert!(window.app.view.zoom > 1.0, "{:?}", window.app.view);
    let pan = window.app.view.pan;
    let press = |pressed| egui::Event::PointerButton {
        pos: CANVAS_POINT,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    window.events.push(press(true));
    window.frame();
    for step in 1..=4 {
        let pos = CANVAS_POINT + egui::vec2(10.0 * step as f32, 0.0);
        window.events.push(egui::Event::PointerMoved(pos));
        window.frame();
    }
    window.events.push(press(false));
    window.frame();
    assert!(window.app.view.pan.x > pan.x, "{:?}", window.app.view);

    // 100% refines full-resolution tiles over the proxy.
    window.app.view.native_pixels = true;
    for _ in 0..30 {
        window.frame();
        let _ = window.rs.device.poll(wgpu::PollType::Poll);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(window.app.view.native_pixels);
    assert!(window.app.preview.regions.error.is_none());
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn dialog_results_save_presets_export_and_open_images() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let first = dir.join("first.png");
    let second = dir.join("second.png");
    write_image(&first, 64, 32);
    write_image(&second, 48, 24);
    let mut window = Window::new(Some(first));
    window.loaded();

    let saved = dir.join("look.pfpreset.json");
    window.send(DialogResult::PresetSave(Some(Ok(saved.clone()))));
    assert_eq!(
        window.app.presets.status,
        format!("Saved preset {}", saved.display())
    );
    window.send(DialogResult::PresetSave(Some(Err("disk full".into()))));
    assert_eq!(
        window.app.presets.status,
        "Could not save preset: disk full"
    );
    window.send(DialogResult::PresetSave(None));
    assert_eq!(window.app.presets.status, "Preset saving cancelled");

    let request = |path: Option<PathBuf>, app: &PhotoApp| ExportRequest {
        path,
        source: app.source.clone().unwrap(),
        adjustments: app.adjustments.clone().unwrap(),
        format: ExportFormat::Png(Default::default()),
        resize: ExportResize::Percentage(50),
    };
    let exported = dir.join("out.png");
    let job = request(Some(exported.clone()), &window.app);
    window.send(DialogResult::Export(job));
    window.until("export", |app| app.export.is_none());
    assert_eq!(
        window.app.export_status,
        format!("Exported {}", exported.display())
    );
    assert_eq!(image::image_dimensions(&exported).unwrap(), (32, 16));

    let job = request(Some(dir.join("out.txt")), &window.app);
    window.send(DialogResult::Export(job));
    assert!(window.app.export.is_none());
    let status = &window.app.export_status;
    assert!(status.starts_with("Could not start export:"), "{status}");
    // Cancelling the save dialog changes nothing.
    let job = request(None, &window.app);
    window.send(DialogResult::Export(job));
    assert!(
        window
            .app
            .export_status
            .starts_with("Could not start export:")
    );

    window.send(DialogResult::Open(None));
    assert!(!window.app.loading);
    window.send(DialogResult::Open(Some(second.clone())));
    assert!(window.app.loading);
    window.loaded();
    assert!(
        window.app.status.contains("48 × 24"),
        "{}",
        window.app.status
    );

    // Only the latest of several queued opens is applied.
    let broken = dir.join("broken.png");
    std::fs::write(&broken, b"not a PNG").unwrap();
    window.app.open(dir.join("missing.png"));
    window.app.open(broken.clone());
    window.until("failed load", |app| !app.loading);
    let status = &window.app.status;
    assert!(
        status.starts_with(&format!("Could not open {}:", broken.display())),
        "{status}"
    );
    assert!(window.app.source.is_some(), "a failed load keeps the image");
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn dropped_files_and_side_panel_states_render() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let image = dir.join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(None);
    window.drop_file(&image);
    window.loaded();

    // A dropped preset loads without a file dialog.
    let mut edit = EditState::default();
    edit.calibration.exposure = 0.5;
    let preset = Preset::capture("Warm", &edit, &Sections::ALL).unwrap();
    let preset_path = dir.join("warm.pfpreset.json");
    filter_io::preset::write(&preset_path, &preset).unwrap();
    window.drop_file(&preset_path);
    let text = window.until("preset load", |app| {
        !app.dialog_open && app.presets.status.starts_with("Choose sections")
    });
    let text = if has(&text, "Loaded: Warm") {
        text
    } else {
        window.frame()
    };
    assert!(has(&text, "Loaded: Warm"), "{text:?}");

    for (kind, line) in [
        (ExportKind::Png, "sRGB · preserves alpha"),
        (ExportKind::Jpeg, "8-bit sRGB · flattened alpha"),
        (ExportKind::WebP, "Lossless 8-bit sRGB · preserves alpha"),
        (ExportKind::Tiff, "Uncompressed · camera metadata omitted"),
    ] {
        window.app.export_kind = kind;
        let text = window.frame();
        assert!(has(&text, line), "{line}: {text:?}");
    }
    window.app.png_options.bit_depth = filter_io::PngBitDepth::Eight;
    window.app.tiff_options.bit_depth = filter_io::TiffBitDepth::Eight;
    for kind in [ExportKind::Png, ExportKind::Tiff] {
        window.app.export_kind = kind;
        assert!(has(&window.frame(), "Dither 8-bit output"));
    }

    window.app.export_resize = ExportResize::LongEdge(16);
    let text = window.frame();
    assert!(has(&text, "Reduce only; smaller images retain their size."));
    assert!(has(&text, "Output: 16 × 8 px"));
    window.app.export_resize = ExportResize::Percentage(50);
    assert!(has(&window.frame(), "Output: 32 × 16 px"));

    // Unsupported adjustments block preview and export with an explanation.
    let mut invalid = EditState::default();
    invalid.calibration.exposure = f32::NAN;
    window.app.replace_edit(invalid);
    assert!(window.app.adjustments.is_err());
    let text = window.frame();
    assert!(has(&text, "Adjustments cannot be previewed"));
    assert!(
        text.iter()
            .any(|s| s.starts_with("Cannot preview these adjustments:"))
    );

    // Dropping another image replaces the current one.
    let other = dir.join("other.png");
    write_image(&other, 20, 10);
    window.drop_file(&other);
    window.frame();
    assert!(window.app.loading);
    window.loaded();
    assert!(window.app.status.contains("20 × 10"));
    assert!(
        window.app.adjustments.is_ok(),
        "a new image resets the edit"
    );
}
