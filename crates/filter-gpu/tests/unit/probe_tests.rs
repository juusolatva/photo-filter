use super::*;
use filter_core::edit::EditState;
use half::f16;
use std::time::{Duration, Instant};

fn collect(gpu: &mut GpuProbe, device: &wgpu::Device, count: usize) -> Vec<(u64, ProbeSample)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = Vec::new();
    while output.len() < count {
        device.poll(wgpu::PollType::Poll).unwrap();
        output.extend(gpu.collect().into_iter().map(|(g, r)| (g, r.unwrap())));
        assert!(Instant::now() < deadline, "color probe timed out");
        if output.len() < count {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    output.sort_by_key(|(g, _)| *g);
    output
}
fn compare(actual: ProbeSample, expected: ProbeSample) {
    match (actual, expected) {
        (
            ProbeSample::Color {
                rgb: a,
                oklab: la,
                alpha: aa,
            },
            ProbeSample::Color {
                rgb: b,
                oklab: lb,
                alpha: ab,
            },
        ) => {
            for (a, b) in a.into_iter().zip(b) {
                assert!((a - b).abs() < 0.0001, "RGB: {a} != {b}");
            }
            for (a, b) in la.into_iter().zip(lb) {
                assert!((a - b).abs() < 0.0001, "Oklab: {a} != {b}");
            }
            assert_eq!(aa, ab);
            assert_eq!(actual.rgb8(), expected.rgb8());
        }
        _ => assert_eq!(actual, expected),
    }
}
#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn source_probe_matches_cpu_bounds_jobs_and_recovers_failed_maps() {
    let (adapter, device, queue) = pollster::block_on(async {
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
        (adapter, device, queue)
    });
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut gpu = GpuProbe::new(&device).unwrap();
    let neutral = BasicAdjustments::default();
    let mut edit = EditState::default();
    edit.calibration.exposure = 0.7;
    edit.calibration.temperature = 0.35;
    edit.calibration.tint = -0.2;
    edit.tone.brightness = 0.25;
    edit.tone.contrast = -0.3;
    edit.tone.highlights = -0.2;
    edit.tone.shadows = 0.15;
    edit.tone.whites = 0.2;
    edit.tone.blacks = -0.15;
    edit.color.scalars.vibrance = 0.3;
    edit.color.scalars.saturation = -0.2;
    let mixed = BasicAdjustments::new(&edit).unwrap();
    edit.curves.master.0 = vec![[0.0, 0.1], [0.5, 0.7], [1.0, 0.95]];
    edit.curves.green.0 = vec![[0.0, 0.0], [1.0, 0.6]];
    edit.color.hsl[0].saturation = -0.6;
    edit.color.hsl[3].hue = 0.8;
    edit.color.hsl[5].lightness = -0.5;
    edit.color.grading[1].chroma = 0.6;
    edit.color.grading[1].hue = 0.6;
    edit.color.grading[0].lightness = 0.5;
    edit.color.scalars.channel_mixer = [[1.2, -0.1, -0.1], [0.1, 0.9, 0.0], [-0.1, 0.2, 0.9]];
    let curved = BasicAdjustments::new(&edit).unwrap();
    let settings = [
        neutral.clone(),
        BasicAdjustments::exposure(1.25).unwrap(),
        mixed,
        curved,
    ];
    let pixels = [
        [0.0, 0.0, 0.0, 1.0],
        [1.0; 4],
        [0.125, 0.25, 0.5, 1.0],
        [-0.2, 0.15, 1.5, 0.25],
        [0.8, 0.1, 0.3, 0.5],
        [0.15, 0.7, 0.1, 0.00001], // no premultiplied half-float underflow
        [f32::NAN, f32::INFINITY, 1.0, 0.0],
        [f32::NAN, 1.0, 1.0, 1.0],
        [1.0, f32::INFINITY, 1.0, 1.0],
        [1.0, 1.0, 1.0, f32::NAN],
    ]
    .map(|p| p.map(|v| f16::from_f32(v).to_f32()));
    for (i, pixel) in pixels.into_iter().enumerate() {
        let a = &settings[i % settings.len()];
        let b = &settings[(i + 1) % settings.len()];
        assert!(gpu.try_submit(&device, &queue, pixel, a, 1));
        // Different source pixel/edit with the first job still pending.
        let replacement = [0.125, 0.25, 0.5, 0.5];
        assert!(gpu.try_submit(&device, &queue, replacement, b, 2));
        assert!(!gpu.try_submit(&device, &queue, pixel, a, 3));
        let results = collect(&mut gpu, &device, 2);
        assert_eq!((results[0].0, results[1].0), (1, 2));
        compare(results[0].1, ProbeSample::reference(pixel, a));
        compare(results[1].1, ProbeSample::reference(replacement, b));
        assert!(!gpu.is_pending());
        assert!(gpu.collect().is_empty());
    }
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "probe must not produce GPU validation errors"
    );
    for disconnected in [false, true] {
        let old = gpu.readback.inject_failed_map(10, disconnected);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let failed = gpu.collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, 10);
        assert!(failed[0].1.is_err());
        assert!(
            pollster::block_on(scope.pop()).is_none(),
            "failed readback must not unmap an idle buffer"
        );
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        assert!(gpu.try_submit(&device, &queue, [0.25; 4], &neutral, 11));
        assert_ne!(old, *gpu.readback.first_buffer());
        let results = collect(&mut gpu, &device, 1);
        assert_eq!(results[0].0, 11);
        compare(results[0].1, ProbeSample::reference([0.25; 4], &neutral));
        assert!(
            pollster::block_on(scope.pop()).is_none(),
            "retry must not produce validation errors"
        );
    }
    let (limited, _) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: 16,
            ..Default::default()
        },
        ..Default::default()
    }))
    .unwrap();
    assert!(GpuProbe::new(&limited).is_err());
}
