use super::*;
fn histogram(n: u32) -> Histogram {
    Histogram {
        transparent: n,
        ..Default::default()
    }
}
#[test]
fn image_changes_reject_old_results_even_when_adjustments_match() {
    let mut state = HistogramState::default();
    let settings = Some(*BasicAdjustments::default().uniform_rows());
    state.set_settings(settings);
    let old = state.generation;
    state.reset();
    state.set_settings(settings);
    state.accept(old, Ok(histogram(1)));
    assert!(state.display.is_none());
    state.accept(state.generation, Ok(histogram(2)));
    assert!(state.current());
    assert_eq!(state.display.unwrap().1.transparent, 2);
}
#[test]
fn edits_coalesce_and_stale_errors_cannot_replace_current_data() {
    let mut state = HistogramState::default();
    state.set_settings(Some(*BasicAdjustments::default().uniform_rows()));
    let old = state.generation;
    state.accept(old, Ok(histogram(1)));
    state.submitted = Some(old);
    state.set_settings(Some(
        *BasicAdjustments::exposure(1.0).unwrap().uniform_rows(),
    ));
    let latest = state.generation;
    assert!(!state.current());
    assert_eq!(state.display.as_ref().unwrap().1.transparent, 1);
    assert!(state.submitted.is_none());
    state.set_settings(Some(
        *BasicAdjustments::exposure(1.0).unwrap().uniform_rows(),
    ));
    assert_eq!(state.generation, latest); // Idle repaint or pan/zoom changes nothing.
    state.accept(latest, Ok(histogram(3)));
    state.accept(old, Err("old failure".into()));
    state.accept(old, Ok(histogram(9)));
    assert!(state.error.is_none());
    assert!(state.current());
    assert_eq!(state.display.unwrap().1.transparent, 3);
}
#[test]
fn unsupported_edits_clear_display_and_failures_can_retry_same_source() {
    let mut state = HistogramState::default();
    state.set_settings(Some(*BasicAdjustments::default().uniform_rows()));
    let generation = state.generation;
    state.submitted = Some(generation);
    state.accept(generation, Err("mapping failure".into()));
    state.retry();
    assert!(state.error.is_none());
    assert!(state.submitted.is_none());
    assert_eq!(state.generation, generation);
    state.accept(generation, Ok(histogram(1)));
    state.set_settings(None);
    state.accept(generation, Ok(histogram(4)));
    assert!(state.display.is_none());
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn histogram_ui_coalesces_edits_and_refreshes_the_reserved_panel() {
    use filter_core::ImageBuffer;
    use filter_gpu::ExposureRenderer;
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
        let renderer = ExposureRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let image = ImageBuffer::new(
            3,
            2,
            vec![
                [
                    f16::from_f32(0.125),
                    f16::from_f32(0.25),
                    f16::from_f32(0.5),
                    f16::ONE
                ];
                6
            ],
        )
        .unwrap();
        let source = renderer.upload(&device, &queue, &image);
        let mut histogram = HistogramUi::new(&device);
        let neutral = BasicAdjustments::default();
        let bright = BasicAdjustments::exposure(1.0).unwrap();
        let latest = BasicAdjustments::exposure(-1.0).unwrap();
        histogram.refresh(&device, &queue, Some(&source), Some(&neutral));
        histogram.refresh(&device, &queue, Some(&source), Some(&bright));
        histogram.refresh(&device, &queue, Some(&source), Some(&latest));
        assert!(histogram.is_pending());
        assert_eq!(histogram.state.settings, Some(*latest.uniform_rows()));
        // A queue submission may already have completed an older map. The
        // latest edit either uses that free slot or waits for one to open.
        assert!(
            histogram
                .state
                .submitted
                .is_none_or(|g| g == histogram.state.generation)
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !histogram.state.current() || histogram.is_pending() {
            device.poll(wgpu::PollType::Poll).unwrap();
            histogram.refresh(&device, &queue, Some(&source), Some(&latest));
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            histogram.state.display.as_ref().unwrap().1,
            Histogram::reference_preview(&image, &latest).unwrap()
        );
        let generation = histogram.state.generation;
        histogram.refresh(&device, &queue, Some(&source), Some(&latest));
        assert_eq!(histogram.state.generation, generation);
        assert!(
            !histogram.is_pending(),
            "idle UI must not keep submitting GPU jobs"
        );

        // Paint the same reserved space used above the adjustment controls.
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let reserved = ui
                    .allocate_exact_size(
                        egui::vec2(220.0, HistogramUi::HEIGHT),
                        egui::Sense::hover(),
                    )
                    .0;
                ui.label("Basic adjustments");
                histogram.show(ui, reserved);
            });
        });
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| {
                if let egui::Shape::Text(text) = &s.shape {
                    Some(text.galley.text().to_owned())
                } else {
                    None
                }
            })
            .collect();
        output.drop_without_applying_deltas();
        assert!(text.iter().any(|s| s == "Histogram"));
        assert!(text.iter().any(|s| s.contains("6 visible pixels")));
        assert!(text.iter().any(|s| s.starts_with("RGB clips:")));
    });
}
