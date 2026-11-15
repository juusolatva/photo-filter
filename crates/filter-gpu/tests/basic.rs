//! Explicit software-adapter parity for canonical WGSL, f32 and tiled f16 outputs.
use filter_core::{ImageBuffer, cpu_eval::BasicAdjustments, edit::EditState};
use filter_gpu::{TilePlan, TiledExposure};
use half::f16;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

fn snapshots() -> Vec<BasicAdjustments> {
    let mut edits = vec![EditState::default()];
    for i in 0..11 {
        for amount in [-1.0, 1.0] {
            let mut e = EditState::default();
            match i {
                0 => e.calibration.exposure = amount * 5.0,
                1 => e.calibration.temperature = amount,
                2 => e.calibration.tint = amount,
                3 => e.tone.brightness = amount,
                4 => e.tone.contrast = amount,
                5 => e.tone.highlights = amount,
                6 => e.tone.shadows = amount,
                7 => e.tone.whites = amount,
                8 => e.tone.blacks = amount,
                9 => e.color.scalars.vibrance = amount,
                _ => e.color.scalars.saturation = amount,
            }
            edits.push(e);
        }
    }
    for amount in [-1.0, 0.65, 1.0] {
        let mut e = EditState::default();
        e.calibration.exposure = 0.7;
        e.calibration.temperature = amount;
        e.calibration.tint = -amount;
        e.tone.brightness = amount;
        e.tone.contrast = amount;
        e.tone.highlights = -amount;
        e.tone.shadows = amount;
        e.tone.whites = amount;
        e.tone.blacks = -amount;
        e.color.scalars.vibrance = amount;
        e.color.scalars.saturation = amount;
        edits.push(e);
    }
    edits
        .iter()
        .map(|e| BasicAdjustments::new(e).unwrap())
        .collect()
}
#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn basic_stages_match_cpu_in_f32_and_across_half_float_tiles() {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..Default::default()
            })
            .await
            .expect("software adapter required");
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let (w, h) = (67, 3);
        let image = ImageBuffer::new(
            w,
            h,
            (0..w * h)
                .map(|i| {
                    let x = (i % w) as f32 / (w - 1) as f32;
                    let rgb = match i / w {
                        0 => [x * x, x, 1.0 - x],
                        1 => [x * 4.0 - 1.0, x * 2.0 - 0.5, 0.125],
                        _ => [x * 0.001; 3],
                    };
                    [
                        rgb[0],
                        rgb[1],
                        rgb[2],
                        [0.0, 0.25, 0.5, 1.0][i as usize % 4],
                    ]
                    .map(f16::from_f32)
                })
                .collect(),
        )
        .unwrap();
        let pixels: Vec<[f32; 4]> = image.pixels().iter().map(|p| p.map(f16::to_f32)).collect();
        let code = format!(
            "{}\n{}",
            include_str!("../src/working.wgsl"),
            r#"
            @group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
            @group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
            @group(0) @binding(2) var<uniform> params: BasicParams;
            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id:vec3<u32>) {
                if id.x < arrayLength(&input) { output[id.x]=apply_basic(input[id.x],params); }
            }
        "#
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&pixels),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (pixels.len() * 16) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let renderer = TiledExposure::new(&device);
        for (case, settings) in snapshots().iter().enumerate() {
            let mut expected = pixels.clone();
            settings.apply(&mut expected);
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(settings.uniform_rows()),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: input.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: output.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bindings, &[]);
                pass.dispatch_workgroups((pixels.len() as u32).div_ceil(64), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                tx.send(r).unwrap();
            });
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(30)),
                })
                .unwrap();
            rx.recv().unwrap().unwrap();
            let mapped = readback.slice(..).get_mapped_range().unwrap();
            let actual: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
            for (p, q) in expected.iter().zip(actual) {
                compare(*p, *q, case, 2e-5);
            }
            drop(mapped);
            readback.unmap();
            let mut reference = None;
            for edge in [w, 17] {
                let plan = TilePlan::new(w, h, &device.limits(), edge).unwrap();
                let mut actual = vec![[f16::ZERO; 4]; pixels.len()];
                for tile in plan.tiles() {
                    let mut pending = renderer
                        .submit_adjusted(&device, &queue, &image, tile, settings)
                        .unwrap();
                    let started = Instant::now();
                    let data = loop {
                        device.poll(wgpu::PollType::Poll).unwrap();
                        if let Some(data) = pending.try_read().unwrap() {
                            break data;
                        }
                        assert!(started.elapsed() < Duration::from_secs(30));
                        std::thread::sleep(Duration::from_millis(1));
                    };
                    for (row, data) in data.chunks_exact(tile.width as usize).enumerate() {
                        let start = (tile.y as usize + row) * w as usize + tile.x as usize;
                        actual[start..start + data.len()].copy_from_slice(data);
                    }
                }
                for (p, q) in expected.iter().zip(&actual) {
                    compare(*p, q.map(f16::to_f32), case, 0.001);
                }
                if let Some(reference) = &reference {
                    assert_eq!(&actual, reference, "tile seam case {case}");
                } else {
                    reference = Some(actual);
                }
            }
        }
    });
}
fn compare(expected: [f32; 4], actual: [f32; 4], case: usize, tolerance: f32) {
    for c in 0..3 {
        assert!(
            actual[c].is_finite()
                && (actual[c] - expected[c]).abs() <= tolerance * (1.0 + expected[c].abs()),
            "case {case}: {actual:?} != {expected:?}"
        );
    }
    assert_eq!(actual[3], expected[3]);
    let a = filter_core::color::rec2020_to_linear_srgb(actual[..3].try_into().unwrap());
    let b = filter_core::color::rec2020_to_linear_srgb(expected[..3].try_into().unwrap());
    for c in 0..3 {
        let encoded =
            |x: f32| (filter_core::color::linear_to_srgb(x).clamp(0.0, 1.0) * 255.0).round();
        assert!(
            (encoded(a[c]) - encoded(b[c])).abs() < 2.0,
            "display case {case}: {a:?} != {b:?}"
        );
    }
}
