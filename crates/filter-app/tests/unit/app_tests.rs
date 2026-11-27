//! Headless app-shell checks: real egui frames on a software GPU with queued
//! input and dialog results. Native file dialogs are never opened.
use super::{DialogResult, ExportKind, ExportRequest, PhotoApp};
use crate::export::ExportFormat;
use crate::resize::ExportResize;
use eframe::egui;
use filter_core::{
    edit::{EditState, GradingWheel, HslBand},
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
        self.frame_shapes()
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }
    /// Run one frame and return painted text with its bounds.
    fn frame_shapes(&mut self) -> Vec<(String, egui::Rect)> {
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
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_owned(), t.visual_bounding_rect()))
                }
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        text
    }
    /// Bounds of a painted text, from a fresh frame.
    fn find(&mut self, line: &str) -> egui::Rect {
        let shapes = self.frame_shapes();
        shapes
            .iter()
            .find(|(text, _)| text == line)
            .unwrap_or_else(|| panic!("{line:?} is not painted: {shapes:?}"))
            .1
    }
    fn press(&mut self, pos: egui::Pos2, pressed: bool) {
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    fn click_at(&mut self, pos: egui::Pos2, button: egui::PointerButton) {
        self.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.events.push(egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            self.frame();
        }
    }
    fn click(&mut self, line: &str) {
        let center = self.find(line).center();
        self.press(center, true);
        self.frame();
        self.press(center, false);
        self.frame();
    }
    /// Press and release a shortcut over two frames, as a keyboard would.
    fn shortcut(&mut self, modifiers: egui::Modifiers, key: egui::Key) {
        for pressed in [true, false] {
            self.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
            self.frame();
        }
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
    assert_eq!(
        window.app.preview.regions.drawn_tiles(),
        0,
        "Fit minifies, so only the area-filtered proxy is drawn"
    );

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
    assert_eq!(window.app.preview.regions.drawn_tiles(), 0);
    window.app.view.native_pixels = true;
    window.until("full-resolution tiles", |app| {
        app.preview.regions.drawn_tiles() > 0 && !app.preview.regions.is_pending()
    });
    assert!(window.app.view.native_pixels);
    assert!(window.app.preview.regions.error.is_none());
    // Leaving 100% for Fit draws the proxy alone again; cached tiles keep
    // their frames, so check that no new tile is requested.
    let drawn = window.app.preview.regions.drawn_tiles();
    window.app.view.fit();
    for _ in 0..5 {
        window.frame();
    }
    assert_eq!(window.app.preview.regions.drawn_tiles(), drawn);
    assert!(!window.app.preview.regions.is_pending());
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
    window.send(DialogResult::Export(Box::new(job)));
    window.until("export", |app| app.export.is_none());
    assert_eq!(
        window.app.export_status,
        format!("Exported {}", exported.display())
    );
    assert_eq!(image::image_dimensions(&exported).unwrap(), (32, 16));

    let job = request(Some(dir.join("out.txt")), &window.app);
    window.send(DialogResult::Export(Box::new(job)));
    assert!(window.app.export.is_none());
    let status = &window.app.export_status;
    assert!(status.starts_with("Could not start export:"), "{status}");
    // Cancelling the save dialog changes nothing.
    let job = request(None, &window.app);
    window.send(DialogResult::Export(Box::new(job)));
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

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn slider_drags_presets_and_shortcuts_form_undo_steps() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let image = dir.join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image.clone()));
    window.loaded();
    assert!(!window.app.can_undo() && !window.app.can_redo());

    // The exposure rail starts at the panel's content edge, in the label's row.
    let left = window.find("Basic adjustments").min.x;
    let row = window.find("Exposure EV").center().y;
    let start = egui::pos2(left + 30.0, row);
    window.press(start, true);
    window.frame();
    for step in 1..=5 {
        window.events.push(egui::Event::PointerMoved(
            start + egui::vec2(8.0 * step as f32, 0.0),
        ));
        window.frame();
        assert!(
            window.app.edit_dirty,
            "a drag in progress is not yet a step"
        );
        assert!(!window.app.history.can_undo());
    }
    window.press(start + egui::vec2(40.0, 0.0), false);
    window.frame();
    let dragged = window.app.edit.calibration.exposure;
    assert!(dragged != 0.0, "the drag changed exposure");
    assert!(!window.app.edit_dirty && window.app.history.can_undo());

    // A preset application is another single step.
    let mut warm = EditState::default();
    warm.color.scalars.saturation = 0.5;
    let preset = Preset::capture("Saturated", &warm, &Sections::ALL).unwrap();
    window.send(DialogResult::PresetLoad {
        generation: window.app.generation,
        result: Some(Ok(Box::new(preset))),
    });
    window.click("Apply selected sections");
    assert_eq!(window.app.edit.color.scalars.saturation, 0.5);
    assert_eq!(window.app.edit.calibration.exposure, 0.0, "full preset");

    // Ctrl+Z walks back one step at a time and refreshes the renderer snapshot.
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.calibration.exposure, dragged);
    assert_eq!(window.app.edit.color.scalars.saturation, 0.0);
    let expected = filter_core::cpu_eval::BasicAdjustments::new(&window.app.edit).unwrap();
    assert_eq!(
        format!("{:?}", window.app.adjustments),
        format!("{:?}", Ok::<_, filter_core::edit::ModelError>(expected))
    );
    window.click("Undo");
    assert_eq!(window.app.edit, EditState::default());
    assert!(!window.app.can_undo() && window.app.can_redo());
    // Both redo shortcuts and the Redo button step forward again.
    window.shortcut(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::Z,
    );
    assert_eq!(window.app.edit.calibration.exposure, dragged);
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Y);
    assert_eq!(window.app.edit.color.scalars.saturation, 0.5);
    window.click("Undo");
    window.click("Redo");
    assert_eq!(window.app.edit.color.scalars.saturation, 0.5);
    assert!(!window.app.can_redo());

    // Reset is undoable too.
    window.click("Reset adjustments");
    assert_eq!(window.app.edit, EditState::default());
    window.click("Undo");
    assert_eq!(window.app.edit.color.scalars.saturation, 0.5);

    // Opening an image starts a fresh history.
    window.app.open(image);
    window.loaded();
    assert_eq!(window.app.edit, EditState::default());
    assert!(!window.app.can_undo() && !window.app.can_redo());
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit, EditState::default());
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn before_after_split_drags_and_space_shows_the_original() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    assert_eq!(
        window.app.preview.before_until, 0.0,
        "edit shown everywhere"
    );

    window.click("Before/after");
    let text = window.frame();
    assert_eq!(window.app.compare.split, Some(0.5));
    assert!(has(&text, "Before") && has(&text, "After"), "{text:?}");
    // Headless frames use one physical pixel per point.
    let divider = window.app.preview.before_until;
    assert!(divider > 0.0 && divider < SCREEN.x, "{divider}");
    assert_eq!(divider, divider.round(), "on a physical pixel boundary");

    // Dragging the divider moves the split rather than panning the image.
    let pan = window.app.view.pan;
    let start = egui::pos2(divider, CANVAS_POINT.y);
    window.press(start, true);
    window.frame();
    for step in 1..=4 {
        let pos = start - egui::vec2(25.0 * step as f32, 0.0);
        window.events.push(egui::Event::PointerMoved(pos));
        window.frame();
    }
    window.press(start - egui::vec2(100.0, 0.0), false);
    window.frame();
    assert_eq!(window.app.view.pan, pan);
    let moved = window.app.preview.before_until;
    assert!(
        (moved - (divider - 100.0)).abs() <= 1.0,
        "{moved} vs {divider}"
    );
    assert!(window.app.compare.split.unwrap() < 0.5);

    // Holding Space shows the original everywhere, and its presses do not
    // click a keyboard-focused button: Tab to Reset adjustments first.
    let reset = window.find("Reset adjustments").center();
    let reset_focused = |window: &Window| {
        window
            .ctx
            .memory(|m| m.focused())
            .and_then(|id| window.ctx.read_response(id))
            .is_some_and(|response| response.rect.contains(reset))
    };
    for _ in 0..80 {
        if reset_focused(&window) {
            break;
        }
        window.shortcut(egui::Modifiers::NONE, egui::Key::Tab);
    }
    assert!(reset_focused(&window), "Tab reaches Reset adjustments");
    let mut edit = EditState::default();
    edit.calibration.exposure = 1.0;
    window.app.replace_edit(edit.clone());
    let space = |pressed, repeat| egui::Event::Key {
        key: egui::Key::Space,
        physical_key: None,
        pressed,
        repeat,
        modifiers: egui::Modifiers::NONE,
    };
    window.events.push(space(true, false));
    let text = window.frame();
    assert!(window.app.compare.held);
    assert_eq!(window.app.preview.before_until, f32::MAX);
    assert!(has(&text, "Before") && !has(&text, "After"), "{text:?}");
    // Held across frames, including key-repeat presses.
    window.events.push(space(true, true));
    window.frame();
    assert_eq!(window.app.preview.before_until, f32::MAX);
    window.events.push(space(false, false));
    window.frame();
    assert!(!window.app.compare.held);
    assert_eq!(window.app.preview.before_until, moved);
    assert_eq!(window.app.edit, edit, "Space did not click Reset");
    assert!(reset_focused(&window));

    window.click("Before/after");
    let text = window.frame();
    assert_eq!(window.app.compare.split, None);
    assert_eq!(window.app.preview.before_until, 0.0);
    assert!(!has(&text, "After"));
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn clipping_overlay_checkboxes_and_j_toggle_view_state_only() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image.clone()));
    window.loaded();
    let state = |window: &Window| (window.app.clipping.shadows, window.app.clipping.highlights);
    window.frame();
    assert_eq!(state(&window), (false, false));
    assert_eq!(window.app.preview.clipping, Default::default());

    // J turns both on; with either off it turns both on again.
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    assert_eq!(state(&window), (true, true));
    assert!(window.app.preview.clipping.shadows && window.app.preview.clipping.highlights);
    window.click("Shadow clipping");
    assert_eq!(state(&window), (false, true));
    assert!(!window.app.preview.clipping.shadows);
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    assert_eq!(state(&window), (true, true));
    window.click("Highlight clipping");
    assert_eq!(state(&window), (true, false));
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    assert_eq!(state(&window), (false, false), "both on, then both off");

    // View state: no edit, no undo step, and it survives loading another image.
    assert_eq!(window.app.edit, EditState::default());
    assert!(!window.app.can_undo() && !window.app.can_redo());
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    window.app.open(image);
    window.loaded();
    window.frame();
    assert_eq!(state(&window), (true, true));
    assert!(!window.app.can_undo());

    // A focused text field keeps the key.
    for _ in 0..80 {
        if window.ctx.text_edit_focused() {
            break;
        }
        window.shortcut(egui::Modifiers::NONE, egui::Key::Tab);
    }
    assert!(window.ctx.text_edit_focused(), "Tab reaches a text field");
    window.shortcut(egui::Modifiers::NONE, egui::Key::J);
    assert_eq!(state(&window), (true, true), "J ignored while typing");
}

/// Press, OS key-repeat events (one in the same frame, more in later frames)
/// and release of `key`, as a held key produces.
fn hold(window: &mut Window, key: egui::Key) {
    let event = |pressed, repeat| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat,
        modifiers: egui::Modifiers::NONE,
    };
    window.events.push(event(true, false));
    window.events.push(event(true, true));
    window.frame();
    for _ in 0..3 {
        window.events.push(event(true, true));
        window.frame();
    }
    window.events.push(event(false, false));
    window.frame();
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn holding_w_or_j_toggles_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    window.frame();
    let clipping = |window: &Window| (window.app.clipping.shadows, window.app.clipping.highlights);
    for expected in [(true, true), (false, false), (true, true)] {
        hold(&mut window, egui::Key::J);
        assert_eq!(clipping(&window), expected, "one toggle per held J");
    }
    for expected in [true, false, true] {
        hold(&mut window, egui::Key::W);
        assert_eq!(
            window.app.eyedropper.active, expected,
            "one toggle per held W"
        );
    }
}

/// Warm orange everywhere except fully transparent columns left of x = 16.
fn write_tinted_image(path: &Path) {
    image::RgbaImage::from_fn(64, 32, |x, _| {
        image::Rgba([200, 150, 100, if x < 16 { 0 } else { 255 }])
    })
    .save(path)
    .unwrap();
}
const WB_BUTTON: &str = "White balance picker (W)";
/// Left of the canvas image's transparent columns, and in the letterbox above it.
const TRANSPARENT_POINT: egui::Pos2 = egui::pos2(50.0, 1000.0);
const LETTERBOX_POINT: egui::Pos2 = egui::pos2(500.0, 200.0);

fn white_balance(app: &PhotoApp) -> [f32; 2] {
    [app.edit.calibration.temperature, app.edit.calibration.tint]
}
fn key(window: &mut Window, key: egui::Key, pressed: bool) {
    window.events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    window.frame();
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn white_balance_picker_sets_temperature_and_tint_as_one_undo_step() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("tinted.png");
    write_tinted_image(&image);
    let mut window = Window::new(Some(image));
    window.loaded();
    window.frame();
    // Earlier edits give the pick a previous state to restore.
    window.app.edit.calibration.temperature = 0.25;
    window.app.edit_changed();
    window.frame();
    let before = white_balance(&window.app);
    let expected = {
        let balance = filter_core::white_balance::pick_white_balance(
            &window.app.source.as_ref().unwrap().buffer,
            [40, 16],
        )
        .unwrap();
        [balance.temperature, balance.tint]
    };
    assert!(
        expected[0] < -0.1,
        "warm orange asks for cooling: {expected:?}"
    );

    // Active picker: a drag pans and samples nothing; the letterbox, the
    // divider and a held Space are not image clicks.
    window.click(WB_BUTTON);
    assert!(window.app.eyedropper.active);
    let pan = window.app.view.pan;
    window.events.push(egui::Event::PointerMoved(CANVAS_POINT));
    window.press(CANVAS_POINT, true);
    window.frame();
    for step in 1..=4 {
        let pos = CANVAS_POINT + egui::vec2(10.0 * step as f32, 0.0);
        window.events.push(egui::Event::PointerMoved(pos));
        window.frame();
    }
    window.press(CANVAS_POINT + egui::vec2(40.0, 0.0), false);
    window.frame();
    assert_ne!(window.app.view.pan, pan, "drag still pans");
    assert!(window.app.eyedropper.active);
    assert_eq!(white_balance(&window.app), before);
    window.click_at(LETTERBOX_POINT, egui::PointerButton::Primary);
    assert!(window.app.eyedropper.active);
    assert_eq!(white_balance(&window.app), before);
    window.app.compare.split = Some(0.5);
    window.frame();
    let divider = egui::pos2(window.app.preview.before_until, 1000.0);
    window.click_at(divider, egui::PointerButton::Primary);
    assert!(window.app.eyedropper.active, "divider click is not a pick");
    assert_eq!(white_balance(&window.app), before);
    window.app.compare.split = None;
    window.frame();
    key(&mut window, egui::Key::Space, true);
    window.click_at(CANVAS_POINT, egui::PointerButton::Primary);
    key(&mut window, egui::Key::Space, false);
    assert!(
        window.app.eyedropper.active,
        "Space hold shows the original"
    );
    assert_eq!(white_balance(&window.app), before);

    // A transparent area reports an error, keeps the picker and changes nothing.
    let undoable = window.app.can_undo();
    window.click_at(TRANSPARENT_POINT, egui::PointerButton::Primary);
    let text = window.frame();
    assert!(
        text.iter().any(|s| s.ends_with("transparent pixel")),
        "{text:?}"
    );
    assert!(window.app.eyedropper.active);
    assert_eq!(white_balance(&window.app), before);
    assert_eq!(window.app.can_undo(), undoable);

    // Clicking the image sets both controls, once, and deactivates the picker.
    window.click_at(CANVAS_POINT, egui::PointerButton::Primary);
    assert!(!window.app.eyedropper.active);
    let picked = white_balance(&window.app);
    for i in 0..2 {
        assert!((picked[i] - expected[i]).abs() < 1e-6, "{picked:?}");
    }
    let text = window.frame();
    assert!(
        text.iter()
            .any(|s| s.starts_with("Pixel (") && s.contains("Temperature")),
        "{text:?}"
    );
    let compiled = filter_core::cpu_eval::BasicAdjustments::new(&window.app.edit).unwrap();
    assert_eq!(
        window.app.adjustments.as_ref().unwrap().uniform_rows(),
        compiled.uniform_rows(),
        "the renderer snapshot follows the pick"
    );
    window.app.step_history(false);
    assert_eq!(white_balance(&window.app), before, "one step restores both");
    window.app.step_history(true);
    assert_eq!(white_balance(&window.app), picked);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn white_balance_picker_cancels_and_ignores_the_shortcut_while_typing() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("tinted.png");
    write_tinted_image(&image);
    let mut window = Window::new(None);
    // Without an image the button is disabled and W does nothing.
    window.shortcut(egui::Modifiers::NONE, egui::Key::W);
    assert!(!window.app.eyedropper.active);
    window.app.open(image);
    window.loaded();
    window.frame();

    window.shortcut(egui::Modifiers::NONE, egui::Key::W);
    assert!(window.app.eyedropper.active);
    window.shortcut(egui::Modifiers::NONE, egui::Key::Escape);
    assert!(!window.app.eyedropper.active);
    window.click(WB_BUTTON);
    assert!(window.app.eyedropper.active);
    window.click(WB_BUTTON);
    assert!(!window.app.eyedropper.active, "the button toggles");
    assert_eq!(white_balance(&window.app), [0.0, 0.0]);
    assert!(!window.app.can_undo());

    // Loading another image cancels a pending pick.
    window.shortcut(egui::Modifiers::NONE, egui::Key::W);
    assert!(window.app.eyedropper.active);
    window.app.open(directory.path().join("tinted.png"));
    window.loaded();
    window.frame();
    assert!(!window.app.eyedropper.active);

    for _ in 0..80 {
        if window.ctx.text_edit_focused() {
            break;
        }
        window.shortcut(egui::Modifiers::NONE, egui::Key::Tab);
    }
    assert!(window.ctx.text_edit_focused(), "Tab reaches a text field");
    window.shortcut(egui::Modifiers::NONE, egui::Key::W);
    assert!(!window.app.eyedropper.active, "W ignored while typing");
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn tone_curve_editor_adds_moves_and_removes_points_as_undo_steps() {
    use filter_core::edit::Curve;
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    assert!(has(&window.frame(), "Tone curve"));
    let plot = window.app.curve_ui.plot.expect("the curve plot is painted");
    let at = |x: f32, y: f32| {
        egui::pos2(
            plot.left() + x * plot.width(),
            plot.bottom() - y * plot.height(),
        )
    };

    // Dragging from the diagonal adds a point and moves it as one step.
    window.press(at(0.5, 0.5), true);
    window.frame();
    for step in 1..=5 {
        window
            .events
            .push(egui::Event::PointerMoved(at(0.5, 0.5 + 0.05 * step as f32)));
        window.frame();
    }
    window.press(at(0.5, 0.75), false);
    window.frame();
    let master = window.app.edit.curves.master.0.clone();
    assert_eq!(master.len(), 3, "{master:?}");
    assert!((master[1][0] - 0.5).abs() < 0.01 && (master[1][1] - 0.75).abs() < 0.01);
    let adjustments = window.app.adjustments.as_ref().unwrap();
    assert!(adjustments.curves().is_some(), "curves reach the renderer");
    // Hovering the dragged point shows its input → output readout.
    let readout = format!(
        "Input {:.0} → output {:.0}",
        master[1][0] * 255.0,
        master[1][1] * 255.0
    );
    let text = window.frame();
    assert!(has(&text, &readout), "{readout}: {text:?}");

    // A click adds a point; a right-click removes it.
    window.click_at(at(0.25, 0.1), egui::PointerButton::Primary);
    assert_eq!(window.app.edit.curves.master.0.len(), 4);
    window.click_at(at(0.25, 0.1), egui::PointerButton::Secondary);
    assert_eq!(window.app.edit.curves.master.0, master);

    // Channel tabs edit their own curve.
    window.click("Red");
    window.click_at(at(0.5, 0.3), egui::PointerButton::Primary);
    assert_eq!(window.app.edit.curves.red.0.len(), 3);
    assert_eq!(window.app.edit.curves.master.0, master);
    window.click("Reset curve");
    assert_eq!(window.app.edit.curves.red, Curve::default());

    // Each gesture was one undo step.
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.curves.red.0.len(), 3, "reset undone");
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(
        window.app.edit.curves.red,
        Curve::default(),
        "red click undone"
    );
    for _ in 0..2 {
        window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    }
    assert_eq!(window.app.edit.curves.master.0, master, "add/remove undone");
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit, EditState::default(), "the drag undone");
    assert!(window.app.adjustments.as_ref().unwrap().curves().is_none());
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn color_mixer_drags_and_resets_are_single_undo_steps() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    assert!(has(&window.frame(), "Color mixer (HSL)"));
    window.click("Color mixer (HSL)");
    // The first "Red" painted is the mixer's band row; its slider is one row below.
    let red = window.find("Red");
    let start = egui::pos2(red.min.x + 40.0, red.center().y + 22.0);
    window.press(start, true);
    window.frame();
    for step in 1..=5 {
        window.events.push(egui::Event::PointerMoved(
            start + egui::vec2(6.0 * step as f32, 0.0),
        ));
        window.frame();
        assert!(window.app.edit_dirty && !window.app.history.can_undo());
    }
    window.press(start + egui::vec2(30.0, 0.0), false);
    window.frame();
    let dragged = window.app.edit.color.hsl[0].hue;
    assert!(dragged != 0.0, "the drag changed the red hue");
    assert_eq!(window.app.edit.color.hsl[1], HslBand::default());
    assert!(!window.app.edit_dirty && window.app.history.can_undo());
    let rows = window.app.adjustments.as_ref().unwrap().uniform_rows();
    assert_eq!(rows[15][0], 1.0, "the HSL flag reaches the renderer");

    // Per-channel reset, then undo.
    window.click("Reset hue");
    assert_eq!(window.app.edit, EditState::default());
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.color.hsl[0].hue, dragged);
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit, EditState::default());

    // Reset HSL clears every band in one step; other color controls stay.
    window.app.edit.color.hsl[3].saturation = -0.5;
    window.app.edit.color.hsl[6].lightness = 0.4;
    window.app.edit.color.scalars.vibrance = 0.3;
    window.app.edit_changed();
    window.frame();
    window.click("Reset HSL");
    assert_eq!(window.app.edit.color.hsl, <[HslBand; 8]>::default());
    assert_eq!(window.app.edit.color.scalars.vibrance, 0.3);
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.color.hsl[3].saturation, -0.5);
    assert_eq!(window.app.edit.color.hsl[6].lightness, 0.4);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn color_grading_drags_and_resets_are_single_undo_steps() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    assert!(has(&window.frame(), "Color grading"));
    window.click("Color grading");
    // "Highlights" is also a tone slider; the wheel tab is painted later.
    let tab = window
        .frame_shapes()
        .into_iter()
        .rev()
        .find(|(text, _)| text == "Highlights")
        .unwrap()
        .1;
    window.press(tab.center(), true);
    window.frame();
    window.press(tab.center(), false);
    window.frame();
    // The slider sits left of its "Chroma" label and value box.
    let chroma = window.find("Chroma");
    let start = egui::pos2(chroma.min.x - 150.0, chroma.center().y);
    window.press(start, true);
    window.frame();
    for step in 1..=5 {
        window.events.push(egui::Event::PointerMoved(
            start + egui::vec2(6.0 * step as f32, 0.0),
        ));
        window.frame();
        assert!(window.app.edit_dirty && !window.app.history.can_undo());
    }
    // Let the slider catch up with the pointer before releasing it.
    window.frame();
    window.press(start + egui::vec2(30.0, 0.0), false);
    window.frame();
    let dragged = window.app.edit.color.grading[2].chroma;
    assert!(dragged != 0.0, "the drag changed highlights chroma");
    assert_eq!(window.app.edit.color.grading[0], GradingWheel::default());
    assert!(!window.app.edit_dirty && window.app.history.can_undo());
    let rows = window.app.adjustments.as_ref().unwrap().uniform_rows();
    assert_eq!(rows[15][1], 1.0, "the grading flag reaches the renderer");

    // Per-wheel reset, then undo.
    window.click("Reset highlights");
    assert_eq!(window.app.edit, EditState::default());
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.color.grading[2].chroma, dragged);
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit, EditState::default());

    // Reset grading clears every wheel and the balance in one step only.
    window.app.edit.color.grading[0].chroma = 0.5;
    window.app.edit.color.grading[1].lightness = -0.4;
    window.app.edit.color.scalars.grading_balance = 0.3;
    window.app.edit.color.scalars.vibrance = 0.3;
    window.app.edit_changed();
    window.frame();
    window.click("Reset grading");
    assert_eq!(
        window.app.edit.color.grading,
        <[GradingWheel; 3]>::default()
    );
    assert_eq!(window.app.edit.color.scalars.grading_balance, 0.0);
    assert_eq!(window.app.edit.color.scalars.vibrance, 0.3);
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(window.app.edit.color.grading[0].chroma, 0.5);
    assert_eq!(window.app.edit.color.scalars.grading_balance, 0.3);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn channel_mixer_monochrome_presets_and_uncheck_are_single_undo_steps() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("photo.png");
    write_image(&image, 64, 32);
    let mut window = Window::new(Some(image));
    window.loaded();
    assert!(has(&window.frame(), "Channel mixer"));
    window.click("Channel mixer");
    window.click("Monochrome");
    assert!(window.app.edit.color.scalars.monochrome);
    // Switching on keeps the identity matrix: mono is exact luminance.
    assert_eq!(
        window.app.edit.color.scalars.channel_mixer,
        filter_core::color::IDENTITY
    );
    let rows = *window.app.adjustments.as_ref().unwrap().uniform_rows();
    assert_eq!(rows[0], rows[1]);
    assert_eq!(rows[1], rows[2]);

    window.click("Panchromatic");
    let weights = [0.30, 0.58, 0.12];
    assert_eq!(window.app.edit.color.scalars.channel_mixer, [weights; 3]);
    assert!(window.app.history.can_undo());

    // Unchecking with equal rows resets the matrix, as one step.
    window.click("Monochrome");
    assert!(!window.app.edit.color.scalars.monochrome);
    assert_eq!(
        window.app.edit.color.scalars.channel_mixer,
        filter_core::color::IDENTITY
    );
    assert_eq!(
        window.app.adjustments.as_ref().unwrap().uniform_rows()[..3],
        {
            let neutral = *filter_core::cpu_eval::BasicAdjustments::default().uniform_rows();
            [neutral[0], neutral[1], neutral[2]]
        }
    );
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert!(window.app.edit.color.scalars.monochrome);
    assert_eq!(window.app.edit.color.scalars.channel_mixer, [weights; 3]);

    // Reset channel mixer returns to the neutral edit in one step.
    window.click("Reset channel mixer");
    assert_eq!(window.app.edit, EditState::default());
    window.shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
    assert!(window.app.edit.color.scalars.monochrome);
}
