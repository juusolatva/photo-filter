//! Explicit software-adapter parity for canonical WGSL, f32 and tiled f16 outputs.
use filter_core::{
    ImageBuffer,
    cpu_eval::BasicAdjustments,
    curve,
    edit::{Curve, EditState, GradingWheel, HslBand},
};
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
    edits.extend(curve_edits());
    edits.extend(hsl_edits());
    edits.extend(grading_edits());
    edits.extend(mixer_edits());
    edits
        .iter()
        .map(|e| BasicAdjustments::new(e).unwrap())
        .collect()
}
/// Tone curves alone (S, per-channel, HDR/negative ends) and between tone and color.
fn curve_edits() -> Vec<EditState> {
    let curve = |points: &[[f32; 2]]| Curve(points.to_vec());
    let mut s_curve = EditState::default();
    s_curve.curves.master = curve(&[[0.0, 0.0], [0.25, 0.18], [0.75, 0.85], [1.0, 1.0]]);
    let mut channels = EditState::default();
    channels.curves.red = curve(&[[0.0, 0.05], [1.0, 1.0]]);
    channels.curves.green = curve(&[[0.0, 0.1], [0.5, 0.45], [0.8, 0.85], [1.0, 0.9]]);
    channels.curves.blue = curve(&[[0.0, 0.0], [0.4, 0.3], [1.0, 0.8]]);
    let mut extended = EditState::default();
    extended.curves.master = curve(&[[0.0, -0.1], [0.6, 0.7], [1.0, 1.4]]);
    let mut combined = channels.clone();
    combined.curves.master = s_curve.curves.master.clone();
    combined.calibration.exposure = 0.4;
    combined.calibration.temperature = 0.3;
    combined.tone.contrast = 0.5;
    combined.tone.shadows = 0.4;
    combined.color.scalars.vibrance = 0.6;
    combined.color.scalars.saturation = -0.4;
    let mut color_only = extended.clone();
    color_only.color.scalars.saturation = 0.5;
    vec![s_curve, channels, extended, combined, color_only]
}
/// Deterministic values in [-1, 1].
fn lcg(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 8) as f32 / (1 << 23) as f32 - 1.0
}
/// HSL mixer alone, per band, all bands random, and stacked with tone, curves,
/// vibrance and saturation.
fn hsl_edits() -> Vec<EditState> {
    let mut edits = Vec::new();
    for band in 0..8 {
        let mut e = EditState::default();
        e.color.hsl[band] = HslBand {
            hue: 0.9,
            saturation: if band % 2 == 0 { 0.6 } else { -1.0 },
            lightness: if band % 3 == 0 { -0.7 } else { 1.0 },
        };
        edits.push(e);
    }
    for seed in 1..=3 {
        let mut state = seed * 7919;
        let mut e = EditState::default();
        for band in &mut e.color.hsl {
            *band = HslBand {
                hue: lcg(&mut state),
                saturation: lcg(&mut state),
                lightness: lcg(&mut state),
            };
        }
        edits.push(e);
    }
    let mut stacked = edits[8].clone();
    stacked.calibration.exposure = 0.5;
    stacked.calibration.temperature = 0.3;
    stacked.tone.contrast = 0.5;
    stacked.tone.shadows = 0.4;
    stacked.color.scalars.vibrance = 0.5;
    stacked.color.scalars.saturation = -0.3;
    let mut curved = stacked.clone();
    curved.curves = curve_edits().remove(3).curves;
    // HSL on its own with curves, without tone or vibrance/saturation.
    let mut curved_only = edits[9].clone();
    curved_only.curves = curved.curves.clone();
    edits.extend([stacked, curved, curved_only]);
    edits
}
/// Grading per wheel at balance -1/0/1, all wheels, and stacked with every other
/// color and tone stage, with and without curves.
fn grading_edits() -> Vec<EditState> {
    let wheel = |hue, chroma, lightness| GradingWheel {
        hue,
        chroma,
        lightness,
    };
    let mut edits = Vec::new();
    for balance in [-1.0, 0.0, 1.0] {
        for k in 0..3 {
            let mut e = EditState::default();
            e.color.grading[k] = wheel(0.1 + 0.3 * k as f32, 0.8, if k == 1 { -1.0 } else { 1.0 });
            e.color.scalars.grading_balance = balance;
            edits.push(e);
        }
        let mut all = EditState::default();
        all.color.grading = [
            wheel(0.72, 0.6, 0.5),
            wheel(0.0, 0.0, -0.4),
            wheel(0.17, 1.0, 1.0),
        ];
        all.color.scalars.grading_balance = balance;
        edits.push(all);
    }
    let mut stacked = edits[3].clone();
    stacked.calibration.exposure = 0.5;
    stacked.calibration.temperature = 0.3;
    stacked.tone.contrast = 0.5;
    stacked.tone.shadows = 0.4;
    stacked.color.scalars.vibrance = 0.5;
    stacked.color.scalars.saturation = -1.0;
    stacked.color.hsl = hsl_edits().remove(8).color.hsl;
    let mut curved = stacked.clone();
    curved.curves = curve_edits().remove(3).curves;
    let mut curved_only = edits[3].clone();
    curved_only.curves = curved.curves.clone();
    edits.extend([stacked, curved, curved_only]);
    edits
}
/// Channel mixer and monochrome: a general matrix, each B&W preset, and stacks
/// with white balance, tone, curves, HSL, vibrance/saturation and grading.
fn mixer_edits() -> Vec<EditState> {
    use filter_core::grading::BW_PRESETS;
    let matrix = [[0.9, 0.35, -0.2], [-0.15, 1.3, -0.1], [0.05, -0.4, 1.6]];
    let mut edits = Vec::new();
    let mut mixer = EditState::default();
    mixer.color.scalars.channel_mixer = matrix;
    edits.push(mixer.clone());
    let mut mono = EditState::default();
    mono.color.scalars.monochrome = true;
    edits.push(mono);
    for (_, weights) in BW_PRESETS {
        let mut e = EditState::default();
        e.apply_bw_response(weights);
        edits.push(e);
    }
    let mut stacked = mixer.clone();
    stacked.calibration.exposure = 0.5;
    stacked.calibration.temperature = 0.4;
    stacked.calibration.tint = -0.2;
    stacked.tone.contrast = 0.4;
    stacked.tone.blacks = -0.3;
    stacked.color.scalars.vibrance = 0.4;
    stacked.color.scalars.saturation = -0.3;
    stacked.color.hsl = hsl_edits().remove(8).color.hsl;
    stacked.color.grading = grading_edits().remove(3).color.grading;
    let mut curved = stacked.clone();
    curved.curves = curve_edits().remove(3).curves;
    // Monochrome with per-channel curves (toned B&W), then the perceptual stages.
    let mut toned = curved.clone();
    toned.apply_bw_response(BW_PRESETS[4].1);
    let mut toned_no_curves = stacked.clone();
    toned_no_curves.apply_bw_response(BW_PRESETS[2].1);
    edits.extend([stacked, curved, toned, toned_no_curves]);
    edits
}
/// Hue sweeps across chroma around the HSL gate, and special values.
fn color_rows(row: u32, x: f32, i: u32) -> [f32; 3] {
    use filter_core::color::oklab_to_rec2020;
    let lab = |degrees: f32, chroma: f32, l: f32| {
        let (sin, cos) = degrees.to_radians().sin_cos();
        oklab_to_rec2020([l, chroma * cos, chroma * sin])
    };
    match row {
        3 => lab(x * 360.0, 0.02 + 0.2 * x, 0.6),
        4 => lab(
            x * 360.0 + 3.0,
            [0.003, 0.006, 0.02, 0.045, 0.2][(i % 5) as usize],
            0.2 + 0.6 * x,
        ),
        _ => {
            const SPECIAL: [[f32; 3]; 8] = [
                [0.0, 0.0, 0.0],
                [0.18, 0.18, 0.18],
                [-0.3, 0.2, 0.05],
                [4.0, 0.1, -0.2],
                [0.001, 0.001, 0.001],
                [0.5, 2.5, 0.1],
                [-0.01, -0.01, -0.01],
                [0.25, 0.0, 3.0],
            ];
            SPECIAL[(i % 8) as usize]
        }
    }
}
/// Rows of gradients, ramps, color sweeps and special values with mixed alpha.
fn test_image(w: u32, h: u32) -> ImageBuffer {
    ImageBuffer::new(
        w,
        h,
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 / (w - 1) as f32;
                let rgb = match i / w {
                    0 => [x * x, x, 1.0 - x],
                    1 => [x * 4.0 - 1.0, x * 2.0 - 0.5, 0.125],
                    2 => [x * 0.001; 3],
                    row => color_rows(row, x, i),
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
    .unwrap()
}
/// Render every tile of the plan and assemble the half-float image.
fn render_tiles(
    renderer: &TiledExposure,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &ImageBuffer,
    plan: &TilePlan,
    settings: &BasicAdjustments,
) -> Vec<[f16; 4]> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut actual = vec![[f16::ZERO; 4]; w * h];
    for tile in plan.tiles() {
        let mut pending = renderer
            .submit_adjusted(device, queue, image, tile, settings)
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
            let start = (tile.y as usize + row) * w + tile.x as usize;
            actual[start..start + data.len()].copy_from_slice(data);
        }
    }
    actual
}
#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn basic_stages_match_cpu_in_f32_and_across_half_float_tiles() {
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
        let (w, h) = (67, 6);
        let image = test_image(w, h);
        let pixels: Vec<[f32; 4]> = image.pixels().iter().map(|p| p.map(f16::to_f32)).collect();
        let code = format!(
            "{}\n{}\n{}",
            include_str!("../src/working.wgsl"),
            include_str!("../src/output.wgsl"),
            r#"
            @group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
            @group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
            @group(0) @binding(2) var<uniform> params: BasicParams;
            @group(0) @binding(3) var<uniform> curves: ToneCurves;
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
            let flags = settings.uniform_rows()[15];
            let rows = settings.uniform_rows();
            let stage_active = flags[0] != 0.0
                || flags[1] != 0.0
                || rows[..3] != BasicAdjustments::default().uniform_rows()[..3];
            assert!(
                !stage_active || expected != pixels,
                "case {case} is a no-op"
            );
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(settings.uniform_rows()),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let tables = settings.curves().map_or_else(
                || vec![0; curve::UNIFORM_BYTES],
                |lut| bytemuck::cast_slice(lut.uniform_rows()).to_vec(),
            );
            let curves = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: &tables,
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: curves.as_entire_binding(),
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
                let actual = render_tiles(&renderer, &device, &queue, &image, &plan, settings);
                for (p, q) in expected.iter().zip(&actual) {
                    compare_stored(*p, q.map(f16::to_f32), case);
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
/// Half-float tiles store the f32 shader result rounded to f16. GPU and CPU f32
/// results within the shader tolerance can round to adjacent f16 values, so
/// allow the f16-rounded reference plus one f16 step and the f32 tolerance.
/// The 8-bit display check is left to the f32 comparison: near-black sRGB
/// channels of bright saturated pixels come from cancelling Rec.2020 components,
/// where one f16 step (about 0.001 near 1.3) can move the encoded value by 2/255.
fn compare_stored(expected: [f32; 4], actual: [f32; 4], case: usize) {
    for c in 0..3 {
        let stored = f16::from_f32(expected[c]);
        let step = (f16::from_bits(stored.to_bits() + 1).to_f32() - stored.to_f32()).abs();
        let bound = step + 2e-5 * (1.0 + expected[c].abs());
        assert!(
            actual[c].is_finite() && (actual[c] - stored.to_f32()).abs() <= bound,
            "stored case {case}, channel {c}: {actual:?} != {expected:?} (bound {bound})"
        );
    }
    assert_eq!(actual[3], expected[3]);
}
fn compare(expected: [f32; 4], actual: [f32; 4], case: usize, tolerance: f32) {
    compare_working(expected, actual, case, tolerance);
    compare_display(expected, actual, case);
}
fn compare_working(expected: [f32; 4], actual: [f32; 4], case: usize, tolerance: f32) {
    for c in 0..3 {
        assert!(
            actual[c].is_finite()
                && (actual[c] - expected[c]).abs() <= tolerance * (1.0 + expected[c].abs()),
            "case {case}: {actual:?} != {expected:?}"
        );
    }
    assert_eq!(actual[3], expected[3]);
}
fn compare_display(expected: [f32; 4], actual: [f32; 4], case: usize) {
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
