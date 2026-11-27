//! Shared software-adapter helpers for the LUT GPU tests.
#![allow(dead_code)]
use filter_core::{
    ImageBuffer,
    cpu_eval::BasicAdjustments,
    edit::EditState,
    lut::{CubeLut, LutSlot},
};
use filter_gpu::{TilePlan, TiledExposure};
use half::f16;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use wgpu::util::DeviceExt;

pub fn gpu() -> (wgpu::Device, wgpu::Queue) {
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
        adapter.request_device(&Default::default()).await.unwrap()
    })
}

/// Domain `[lo, hi]` on every channel.
pub type Domain = ([f32; 3], [f32; 3]);
pub const UNIT: Domain = ([0.0; 3], [1.0; 3]);
pub const SHIFTED: Domain = ([-0.2, -0.1, 0.0], [1.5, 1.2, 0.9]);

/// A film-like lattice with slope below 3 and cross-channel terms. Its offsets avoid
/// placing a flat plateau exactly on a histogram bin edge, where one ulp changes the bin.
/// Values are
/// absolute encoded sRGB; node coordinates are mapped through the domain.
pub fn film_lut(n: usize, (lo, hi): Domain) -> CubeLut {
    let mut texels = Vec::with_capacity(3 * n * n * n);
    let s = (n - 1) as f32;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let c = [r, g, b].map(|v| v as f32 / s);
                let x = std::array::from_fn::<f32, 3, _>(|k| lo[k] + c[k] * (hi[k] - lo[k]));
                let out = [
                    0.9 * x[0].clamp(0.0, 1.0).powf(1.15) + 0.08 * x[1] + 0.037,
                    0.05 + 0.85 * x[1] + 0.12 * (x[2] - x[0]),
                    0.1 + 0.7 * x[2] + 0.2 * x[0] * x[1],
                ];
                texels.extend(out.map(|v| f16::from_f32(v).to_bits()));
            }
        }
    }
    CubeLut::new(n, "film", lo, hi, texels).unwrap()
}
pub fn lut_edit(lut: &Arc<CubeLut>, strength: f32) -> EditState {
    let mut edit = EditState::default();
    edit.finishing.lut = LutSlot::Cube(Arc::clone(lut));
    edit.finishing.lut_strength = strength;
    edit
}
pub fn adjustments(lut: &Arc<CubeLut>, strength: f32) -> BasicAdjustments {
    BasicAdjustments::new(&lut_edit(lut, strength)).unwrap()
}
/// Tone and color edits stacked below a LUT.
pub fn graded(lut: &Arc<CubeLut>, strength: f32) -> BasicAdjustments {
    let mut edit = lut_edit(lut, strength);
    edit.calibration.exposure = 0.3;
    edit.tone.contrast = 0.4;
    edit.color.scalars.saturation = 0.2;
    edit.curves.master.0 = vec![[0.0, 0.03], [0.5, 0.55], [1.0, 0.97]];
    BasicAdjustments::new(&edit).unwrap()
}

/// Gradients, HDR, negative and alpha-bearing pixels; no non-finite values.
pub fn pixels(w: u32, h: u32) -> Vec<[f32; 4]> {
    (0..w * h)
        .map(|i| {
            let x = (i % w) as f32 / (w - 1) as f32;
            let rgb = match i / w % 5 {
                0 => [x * x, x, 1.0 - x],
                1 => [x * 4.0 - 1.0, x * 2.0 - 0.5, 0.125],
                2 => [x * 0.001; 3],
                3 => [0.18, x * 1.6, 0.5 - x],
                _ => [x, 1.0 - x, x * x * 2.0],
            };
            let a = [0.0, 0.25, 0.5, 1.0][i as usize % 4];
            [rgb[0], rgb[1], rgb[2], a].map(|v| f16::from_f32(v).to_f32())
        })
        .collect()
}
pub fn image(w: u32, h: u32) -> ImageBuffer {
    ImageBuffer::new(
        w,
        h,
        pixels(w, h)
            .into_iter()
            .map(|p| p.map(f16::from_f32))
            .collect(),
    )
    .unwrap()
}
pub fn special_pixels() -> Vec<[f32; 4]> {
    vec![
        [f32::NAN, 0.5, 0.5, 1.0],
        [0.2, f32::INFINITY, 0.5, 1.0],
        [0.2, 0.3, f32::NEG_INFINITY, 0.5],
        [-0.3, 0.2, 0.05, 1.0],
        [4.0, 0.1, -0.2, 1.0],
        [0.0; 4],
    ]
}

/// Stage-by-stage tolerances of the existing f32 and f16 tile comparisons.
pub fn compare(expected: [f32; 4], actual: [f32; 4], what: &str) {
    for c in 0..3 {
        if !expected[c].is_finite() {
            assert_eq!(
                actual[c].to_bits() & 0x7f80_0000,
                0x7f80_0000,
                "{what}: {actual:?} != {expected:?}"
            );
            continue;
        }
        assert!(
            actual[c].is_finite()
                && (actual[c] - expected[c]).abs() <= 2e-5 * (1.0 + expected[c].abs()),
            "{what}: {actual:?} != {expected:?}"
        );
    }
    assert_eq!(actual[3].to_bits(), expected[3].to_bits(), "{what}");
    if expected[..3].iter().all(|v| v.is_finite()) {
        compare_display(expected, actual, what);
    }
}
fn compare_display(expected: [f32; 4], actual: [f32; 4], what: &str) {
    let a = filter_core::color::rec2020_to_linear_srgb(actual[..3].try_into().unwrap());
    let b = filter_core::color::rec2020_to_linear_srgb(expected[..3].try_into().unwrap());
    for c in 0..3 {
        let encoded =
            |x: f32| (filter_core::color::linear_to_srgb(x).clamp(0.0, 1.0) * 255.0).round();
        assert!(
            (encoded(a[c]) - encoded(b[c])).abs() < 2.0,
            "display {what}: {a:?} != {b:?}"
        );
    }
}
/// f16 tile output: the f16-rounded reference plus one f16 step and the f32 tolerance.
pub fn compare_stored(expected: [f32; 4], actual: [f32; 4], what: &str) {
    for c in 0..3 {
        let stored = f16::from_f32(expected[c]);
        if !expected[c].is_finite() {
            assert!(!actual[c].is_finite(), "{what}: {actual:?} {expected:?}");
            continue;
        }
        let step = (f16::from_bits(stored.to_bits() + 1).to_f32() - stored.to_f32()).abs();
        let bound = step + 2e-5 * (1.0 + expected[c].abs());
        assert!(
            actual[c].is_finite() && (actual[c] - stored.to_f32()).abs() <= bound,
            "stored {what}, channel {c}: {actual:?} != {expected:?} (bound {bound})"
        );
    }
    assert_eq!(actual[3], expected[3], "{what}");
}

/// Compute harness running `apply_basic` over f32 pixels with the LUT bound
/// exactly as the renderers bind it (texture of the LUT's size, 48-byte params).
pub struct Runner {
    pipeline: wgpu::ComputePipeline,
}
impl Runner {
    pub fn new(device: &wgpu::Device) -> Self {
        let code = format!(
            "{}\n{}\n{}\n{}",
            include_str!("../../src/working.wgsl"),
            include_str!("../../src/lut.wgsl"),
            include_str!("../../src/output.wgsl"),
            r#"
            @group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
            @group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
            @group(0) @binding(2) var<uniform> params: BasicParams;
            @group(0) @binding(3) var<uniform> curves: ToneCurves;
            @group(0) @binding(4) var lut_tex: texture_3d<f32>;
            @group(0) @binding(5) var<uniform> lutp: LutParams;
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
        Self { pipeline }
    }
    pub fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &[[f32; 4]],
        settings: &BasicAdjustments,
    ) -> Vec<[f32; 4]> {
        let buffer = |contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents,
                usage,
            })
        };
        let size = (input.len() * 16) as u64;
        let source = buffer(bytemuck::cast_slice(input), wgpu::BufferUsages::STORAGE);
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
        let params = buffer(
            bytemuck::cast_slice(settings.uniform_rows()),
            wgpu::BufferUsages::UNIFORM,
        );
        let tables = settings.curves().map_or_else(
            || vec![0; filter_core::curve::UNIFORM_BYTES],
            |lut| bytemuck::cast_slice(lut.uniform_rows()).to_vec(),
        );
        let curves = buffer(&tables, wgpu::BufferUsages::UNIFORM);
        let lut_params = buffer(
            bytemuck::cast_slice(&settings.lut_rows()),
            wgpu::BufferUsages::UNIFORM,
        );
        let lut_view = exact_lut_view(device, queue, settings);
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, source.as_entire_binding()),
                entry(1, output.as_entire_binding()),
                entry(2, params.as_entire_binding()),
                entry(3, curves.as_entire_binding()),
                entry(4, wgpu::BindingResource::TextureView(&lut_view)),
                entry(5, lut_params.as_entire_binding()),
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups((input.len() as u32).div_ceil(64), 1, 1);
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
        let result = bytemuck::cast_slice::<u8, [f32; 4]>(&mapped).to_vec();
        drop(mapped);
        readback.unmap();
        result
    }
}
fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}
/// A texture of exactly the active LUT's size (1^3 when inactive).
fn exact_lut_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    settings: &BasicAdjustments,
) -> wgpu::TextureView {
    let n = settings.lut().map_or(1, |stage| stage.lut.size() as u32);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    if let Some(stage) = settings.lut() {
        let rgba: Vec<u16> = stage
            .lut
            .texels()
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], f16::ONE.to_bits()])
            .collect();
        queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&rgba),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8 * n),
                rows_per_image: Some(n),
            },
            texture.size(),
        );
    }
    texture.create_view(&Default::default())
}

pub fn read_tile(device: &wgpu::Device, pending: &mut filter_gpu::PendingTile) -> Vec<[f16; 4]> {
    let started = Instant::now();
    loop {
        device.poll(wgpu::PollType::Poll).unwrap();
        if let Some(pixels) = pending.try_read().unwrap() {
            return pixels;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "tile timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
/// Render every tile of a plan with one `TiledExposure` and assemble the image.
pub fn render_tiles(
    renderer: &TiledExposure,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &ImageBuffer,
    edge: u32,
    settings: &BasicAdjustments,
) -> Vec<[f16; 4]> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let plan = TilePlan::new(image.width(), image.height(), &device.limits(), edge).unwrap();
    let mut out = vec![[f16::ZERO; 4]; w * h];
    for tile in plan.tiles() {
        let mut pending = renderer
            .submit_adjusted(device, queue, image, tile, settings)
            .unwrap();
        let data = read_tile(device, &mut pending);
        for (row, data) in data.chunks_exact(tile.width as usize).enumerate() {
            let start = (tile.y as usize + row) * w + tile.x as usize;
            out[start..start + data.len()].copy_from_slice(data);
        }
    }
    out
}
