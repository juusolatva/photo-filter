use super::*;
#[test]
fn pointer_mapping_handles_letterbox_pan_zoom_and_display_scale() {
    for dpi in [1.0, 1.25, 1.5, 2.0] {
        // Same physical image/viewport at several point-to-pixel scales.
        let viewport =
            egui::Rect::from_min_max(egui::pos2(40.0, 60.0) / dpi, egui::pos2(640.0, 460.0) / dpi);
        for (min, extent) in [
            (egui::pos2(140.0, 160.0), egui::vec2(400.0, 200.0)),
            (egui::pos2(-160.0, -40.0), egui::vec2(1200.0, 600.0)),
        ] {
            let image = egui::Rect::from_min_size(min / dpi, extent / dpi);
            // Test pixel centers: exact pixel boundaries can move by one
            // f32 ULP when logical points are scaled on fractional-DPI screens.
            let p = (min + extent * egui::vec2(400.5 / 800.0, 200.5 / 400.0)) / dpi;
            assert_eq!(
                source_pixel(Some(p), viewport, image, [800, 400]),
                Some([400, 200])
            );
            assert_eq!(
                source_pixel(Some(image.max), viewport, image, [800, 400]),
                None
            );
        }
        let image = egui::Rect::from_min_size(
            egui::pos2(140.0, 160.0) / dpi,
            egui::vec2(400.0, 200.0) / dpi,
        );
        assert_eq!(
            source_pixel(Some(image.min), viewport, image, [800, 400]),
            Some([0, 0])
        );
        assert_eq!(
            source_pixel(
                Some(image.max - egui::vec2(0.1, 0.1) / dpi),
                viewport,
                image,
                [800, 400]
            ),
            Some([799, 399])
        );
        assert_eq!(
            source_pixel(Some(viewport.min), viewport, image, [800, 400]),
            None
        );
        assert_eq!(source_pixel(None, viewport, image, [800, 400]), None);
    }
}
#[test]
fn rapid_pointer_edits_image_changes_and_leaving_reject_old_results() {
    let mut s = ProbeState::default();
    let settings = RenderSnapshot::new(&BasicAdjustments::default());
    s.set_target(Some(([1, 2], settings.clone())));
    let old = s.generation;
    s.submitted = Some(old);
    s.accept(old, Ok(ProbeSample::Transparent));
    s.set_target(Some(([2, 2], settings.clone())));
    assert!(s.display.is_none() && s.submitted.is_none());
    s.accept(old, Err("old failure".into()));
    assert!(s.error.is_none());
    s.set_target(Some((
        [2, 2],
        RenderSnapshot::new(&BasicAdjustments::exposure(1.0).unwrap()),
    )));
    let edit = s.generation;
    s.reset(); // New image, even at identical pointer/settings.
    s.set_target(Some(([2, 2], settings.clone())));
    s.accept(edit, Ok(ProbeSample::Transparent));
    assert!(s.display.is_none());
    let current = s.generation;
    s.set_target(s.target.clone());
    assert_eq!(s.generation, current); // Idle and movement inside same pixel.
    s.accept(current, Err("map failure".into()));
    s.retry();
    assert!(s.error.is_none() && s.submitted.is_none());
    s.accept(current, Ok(ProbeSample::Transparent));
    assert!(s.display.is_some());
    s.set_target(None);
    s.accept(current, Ok(ProbeSample::Transparent));
    assert!(s.display.is_none());
}
#[test]
fn a_curve_point_change_with_unchanged_rows_invalidates_the_target() {
    use filter_core::edit::{Curve, EditState};
    let snapshot = |y: f32| {
        let mut edit = EditState::default();
        edit.curves.master = Curve(vec![[0.0, 0.0], [0.5, y], [1.0, 1.0]]);
        RenderSnapshot::new(&BasicAdjustments::new(&edit).unwrap())
    };
    let mut s = ProbeState::default();
    s.set_target(Some(([1, 2], snapshot(0.6))));
    let old = s.generation;
    s.submitted = Some(old);
    s.accept(old, Ok(ProbeSample::Transparent));
    assert!(s.display.is_some());
    s.set_target(Some(([1, 2], snapshot(0.4))));
    assert_eq!(s.generation, old + 1);
    assert!(s.display.is_none() && s.submitted.is_none());
}
/// Paint the readout into its reserved panel space and return its text,
/// checking that every state fits above the following controls.
fn rendered_text(probe: &mut ProbeUi) -> Vec<String> {
    let ctx = egui::Context::default();
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let rect = ui
                .allocate_exact_size(egui::vec2(220.0, ProbeUi::HEIGHT), egui::Sense::hover())
                .0;
            let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            let next = ui.label("Basic adjustments").rect;
            probe.show(&mut panel);
            assert!(
                panel.min_rect().bottom() <= rect.bottom(),
                "readout must fit reserved height"
            );
            assert!(
                panel.min_rect().right() <= rect.right(),
                "readout must fit panel width"
            );
            assert!(
                panel.min_rect().bottom() < next.top(),
                "readout must not overlap controls"
            );
        });
    });
    let text: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|s| {
            if let egui::Shape::Text(t) = &s.shape {
                Some(t.galley.text().to_owned())
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    text
}
fn has(text: &[String], line: &str) -> bool {
    text.iter().any(|s| s == line)
}
#[test]
fn unavailable_gpu_explains_without_offering_retry() {
    let mut probe = ProbeUi {
        gpu: Err("Device limits cannot support the color probe".into()),
        state: ProbeState::default(),
    };
    probe.state.target = Some(([1, 2], RenderSnapshot::new(&BasicAdjustments::default())));
    let text = rendered_text(&mut probe);
    assert!(has(&text, "Color probe unavailable"));
    // Retrying cannot recover a device that lacks the required limits.
    assert!(!has(&text, "Retry color probe"));
    assert!(!probe.is_pending());
}
#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn probe_ui_samples_original_pixels_coalesces_and_paints_reserved_space() {
    use half::f16;
    use std::time::{Duration, Instant};
    let (device, queue) = pollster::block_on(async {
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
        adapter.request_device(&Default::default()).await.unwrap()
    });
    let mut pixels = vec![[f16::ZERO; 4]; 3001];
    pixels[3000] = [0.125, 0.25, 0.5, 0.25].map(f16::from_f32);
    let source = ImageBuffer::new(3001, 1, pixels).unwrap();
    let neutral = BasicAdjustments::default();
    let bright = BasicAdjustments::exposure(1.0).unwrap();
    let mut probe = ProbeUi::new(&device);
    for (pixel, settings) in [
        ([0, 0], &neutral),
        ([3000, 0], &bright),
        ([3000, 0], &neutral),
    ] {
        probe.refresh(&device, &queue, Some(&source), Some(settings), Some(pixel));
    }
    assert!(probe.is_pending());
    let deadline = Instant::now() + Duration::from_secs(30);
    while probe.state.display.is_none() || probe.is_pending() {
        device.poll(wgpu::PollType::Poll).unwrap();
        probe.refresh(
            &device,
            &queue,
            Some(&source),
            Some(&neutral),
            Some([3000, 0]),
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let expected = ProbeSample::reference(source.pixels()[3000].map(f16::to_f32), &neutral);
    let actual = probe.state.display.unwrap();
    assert_eq!(actual.rgb8(), expected.rgb8());
    let generation = probe.state.generation;
    probe.refresh(
        &device,
        &queue,
        Some(&source),
        Some(&neutral),
        Some([3000, 0]),
    );
    assert_eq!(generation, probe.state.generation);
    assert!(!probe.is_pending(), "idle cursor must not submit GPU work");

    let text = rendered_text(&mut probe);
    assert!(text.iter().any(|s| s == "Pixel (3000, 0)"));
    assert!(
        text.iter()
            .any(|s| s.starts_with("RGB ") && s.contains('#'))
    );
    assert!(text.iter().any(|s| s.starts_with("Oklab ")));
    assert!(text.iter().any(|s| s == "Alpha 25.0% · sRGB"));

    // A new image at identical coordinates/settings invalidates old work.
    probe.refresh(
        &device,
        &queue,
        Some(&source),
        Some(&bright),
        Some([3000, 0]),
    );
    probe.reset();
    let replacement = ImageBuffer::new(3001, 1, vec![[f16::ZERO; 4]; 3001]).unwrap();
    probe.refresh(
        &device,
        &queue,
        Some(&replacement),
        Some(&neutral),
        Some([3000, 0]),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while probe.state.display.is_none() || probe.is_pending() {
        device.poll(wgpu::PollType::Poll).unwrap();
        probe.refresh(
            &device,
            &queue,
            Some(&replacement),
            Some(&neutral),
            Some([3000, 0]),
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(probe.state.display, Some(ProbeSample::Transparent));
    probe.refresh(&device, &queue, Some(&replacement), Some(&neutral), None);
    assert!(probe.state.display.is_none());
    assert!(!probe.is_pending());
    // Unsupported settings and out-of-bounds targets never submit.
    probe.refresh(&device, &queue, Some(&replacement), None, Some([3000, 0]));
    probe.refresh(
        &device,
        &queue,
        Some(&replacement),
        Some(&neutral),
        Some([3001, 0]),
    );
    assert!(probe.state.target.is_none() && !probe.is_pending());

    let text = rendered_text(&mut probe);
    assert!(has(&text, "Hover over the image"));

    // Every remaining readout state explains itself within the reserved space.
    probe.state.target = Some(([7, 0], RenderSnapshot::new(&neutral)));
    for (display, line) in [
        (Some(ProbeSample::Transparent), "Transparent · alpha 0%"),
        (
            Some(ProbeSample::Invalid),
            "Invalid pixel · nonfinite color",
        ),
        (None, "Updating…"),
    ] {
        probe.state.display = display;
        let text = rendered_text(&mut probe);
        assert!(has(&text, "Pixel (7, 0)") && has(&text, line), "{text:?}");
    }
    probe.state.error = Some("Color probe mapping failed".into());
    let text = rendered_text(&mut probe);
    assert!(has(&text, "Color probe unavailable"));
    assert!(has(&text, "Retry color probe"));
    assert!(
        !has(&text, "Pixel (7, 0)"),
        "a failure must not show stale values"
    );
}
