//! Explicit software-adapter checks for histogram counts and readback lifecycle.
use filter_core::{
    ImageBuffer,
    cpu_eval::BasicAdjustments,
    edit::EditState,
    histogram::{BINS, Histogram},
};
use filter_gpu::{ExposureRenderer, GpuHistogram};
use half::f16;
use std::time::{Duration, Instant};

fn collect(gpu: &mut GpuHistogram, device: &wgpu::Device, count: usize) -> Vec<(u64, Histogram)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut results = Vec::new();
    while results.len() < count {
        device.poll(wgpu::PollType::Poll).unwrap();
        results.extend(
            gpu.collect()
                .into_iter()
                .map(|(generation, result)| (generation, result.unwrap())),
        );
        assert!(Instant::now() < deadline, "histogram readback timed out");
        if results.len() < count {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    results.sort_by_key(|(generation, _)| *generation);
    results
}

fn fixture() -> ImageBuffer {
    let colors = [
        [0.0; 3],
        [1.0; 3],
        [0.125; 3],
        [0.25; 3],
        [0.5; 3],
        [0.75; 3],
        [-0.2, 0.15, 1.5],
        [0.8, 0.1, 0.3],
        [0.15, 0.7, 0.1],
        [0.1, 0.3, 0.9],
    ];
    ImageBuffer::new(
        67,
        37,
        (0..67 * 37)
            .map(|i| {
                let rgb = colors[i % colors.len()];
                [rgb[0], rgb[1], rgb[2], [0.0, 0.25, 0.5, 1.0][i % 4]].map(f16::from_f32)
            })
            .collect(),
    )
    .unwrap()
}

fn compare(actual: &Histogram, expected: &Histogram) {
    assert_eq!(actual.sampled, expected.sampled);
    assert_eq!(actual.transparent, expected.transparent);
    assert_eq!(actual.invalid, expected.invalid);
    assert_eq!(actual.low_pixels, expected.low_pixels);
    assert_eq!(actual.high_pixels, expected.high_pixels);
    assert_eq!(actual.clipped_low, expected.clipped_low);
    assert_eq!(actual.clipped_high, expected.clipped_high);
    for channel in 0..4 {
        assert_eq!(
            actual.bins[channel], expected.bins[channel],
            "channel {channel}"
        );
    }
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn histogram_matches_cpu_and_double_buffered_jobs_remain_independent() {
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
        let image = fixture();
        let source = renderer.upload(&device, &queue, &image);
        let mut gpu = GpuHistogram::new(&device).unwrap();
        let neutral = BasicAdjustments::default();
        let bright = BasicAdjustments::exposure(1.25).unwrap();
        assert!(
            gpu.try_submit(&device, &queue, &source, &neutral, 1)
                .unwrap()
        );
        assert!(
            gpu.try_submit(&device, &queue, &source, &bright, 2)
                .unwrap()
        );
        // Slots remain occupied even if GPU work finishes until collect unmaps them.
        assert!(
            !gpu.try_submit(&device, &queue, &source, &neutral, 3)
                .unwrap()
        );
        assert!(gpu.is_pending());
        let results = collect(&mut gpu, &device, 2);
        assert_eq!((results[0].0, results[1].0), (1, 2));
        compare(
            &results[0].1,
            &Histogram::reference_preview(&image, &neutral).unwrap(),
        );
        compare(
            &results[1].1,
            &Histogram::reference_preview(&image, &bright).unwrap(),
        );
        assert!(!gpu.is_pending());
        assert!(gpu.collect().is_empty());
        // Change source while old work is pending; its binding and generation
        // must survive, and recycled slots must start from zero counters.
        assert!(
            gpu.try_submit(&device, &queue, &source, &neutral, 4)
                .unwrap()
        );
        gpu.clear_source();
        let transparent = ImageBuffer::new(
            17,
            3,
            vec![[f16::NAN, f16::INFINITY, f16::ONE, f16::ZERO]; 51],
        )
        .unwrap();
        let clear_source = renderer.upload(&device, &queue, &transparent);
        assert!(
            gpu.try_submit(&device, &queue, &clear_source, &neutral, 5)
                .unwrap()
        );
        let results = collect(&mut gpu, &device, 2);
        compare(
            &results[0].1,
            &Histogram::reference_preview(&image, &neutral).unwrap(),
        );
        compare(
            &results[1].1,
            &Histogram::reference_preview(&transparent, &neutral).unwrap(),
        );
        assert_eq!(results[1].1.sampled, 0);
        assert_eq!(results[1].1.transparent, 51);
        assert!(results[1].1.bins.iter().all(|bins| *bins == [0; BINS]));

        let mut mixed = EditState::default();
        mixed.calibration.temperature = 0.35;
        mixed.calibration.tint = -0.2;
        mixed.tone.brightness = 0.25;
        mixed.tone.contrast = -0.3;
        mixed.tone.highlights = -0.2;
        mixed.tone.shadows = 0.15;
        mixed.color.scalars.vibrance = 0.3;
        mixed.color.scalars.saturation = -0.2;
        let mixed = BasicAdjustments::new(&mixed).unwrap();
        assert!(gpu.try_submit(&device, &queue, &source, &mixed, 6).unwrap());
        compare(
            &collect(&mut gpu, &device, 1)[0].1,
            &Histogram::reference_preview(&image, &mixed).unwrap(),
        );
        // Active nonfinite samples are excluded and accounted for, even on edge
        // workgroups whose unused invocations still need to reach both barriers.
        let invalid = ImageBuffer::new(
            3,
            1,
            vec![
                [f16::NAN, f16::ZERO, f16::ZERO, f16::ONE],
                [f16::INFINITY, f16::ONE, f16::ONE, f16::ONE],
                [f16::ONE; 4],
            ],
        )
        .unwrap();
        let invalid_source = renderer.upload(&device, &queue, &invalid);
        assert!(
            gpu.try_submit(&device, &queue, &invalid_source, &neutral, 7)
                .unwrap()
        );
        compare(
            &collect(&mut gpu, &device, 1)[0].1,
            &Histogram::reference_preview(&invalid, &neutral).unwrap(),
        );
        if device.limits().max_texture_dimension_2d > 2048 {
            let too_wide = ImageBuffer::new(2049, 1, vec![[f16::ONE; 4]; 2049]).unwrap();
            let too_wide = renderer.upload(&device, &queue, &too_wide);
            assert!(
                gpu.try_submit(&device, &queue, &too_wide, &neutral, 8)
                    .is_err()
            );
            assert!(!gpu.is_pending());
        }
        let (limited_device, _) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_compute_workgroup_storage_size: 0,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(GpuHistogram::new(&limited_device).is_err());
    });
}
