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
    let settings = *BasicAdjustments::default().uniform_rows();
    s.set_target(Some(([1, 2], settings)));
    let old = s.generation;
    s.submitted = Some(old);
    s.accept(old, Ok(ProbeSample::Transparent));
    s.set_target(Some(([2, 2], settings)));
    assert!(s.display.is_none() && s.submitted.is_none());
    s.accept(old, Err("old failure".into()));
    assert!(s.error.is_none());
    s.set_target(Some((
        [2, 2],
        *BasicAdjustments::exposure(1.0).unwrap().uniform_rows(),
    )));
    let edit = s.generation;
    s.reset(); // New image, even at identical pointer/settings.
    s.set_target(Some(([2, 2], settings)));
    s.accept(edit, Ok(ProbeSample::Transparent));
    assert!(s.display.is_none());
    let current = s.generation;
    s.set_target(s.target);
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
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn probe_ui_samples_original_pixels_coalesces_and_paints_reserved_space() {
    use half::f16;
    use std::time::{Duration, Instant};
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
    });
}
