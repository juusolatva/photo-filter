//! Software-adapter parity for the Finishing LUT stage: cargo test -p filter-gpu --test lut -- --ignored
mod common;
use common::*;
use filter_core::{
    ImageBuffer, cpu_eval::BasicAdjustments, histogram::Histogram, probe::ProbeSample,
};
use filter_gpu::{ExposureRenderer, GpuHistogram, GpuProbe, TiledExposure};
use half::f16;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SIZES: [usize; 3] = [2, 17, 65];
const STRENGTHS: [f32; 3] = [0.0, 0.5, 1.0];

/// Every snapshot under test: sizes, strengths, domains and stacked edits.
fn cases() -> Vec<(String, BasicAdjustments)> {
    let mut cases = Vec::new();
    for n in SIZES {
        for (name, domain) in [("unit", UNIT), ("shifted", SHIFTED)] {
            let lut = Arc::new(film_lut(n, domain));
            for strength in STRENGTHS {
                cases.push((
                    format!("N={n} {name} s={strength}"),
                    adjustments(&lut, strength),
                ));
            }
            cases.push((format!("N={n} {name} graded"), graded(&lut, 0.75)));
        }
    }
    cases
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn f32_shader_matches_cpu_for_sizes_strengths_domains_and_special_pixels() {
    let (device, queue) = gpu();
    let runner = Runner::new(&device);
    let mut input = pixels(67, 10);
    input.extend(special_pixels());
    for (name, settings) in cases() {
        let actual = runner.run(&device, &queue, &input, &settings);
        for (p, q) in input.iter().zip(&actual) {
            compare(settings.pixel(*p), *q, &name);
        }
        if settings.lut().is_some() && !name.contains("graded") {
            assert!(
                input.iter().zip(&actual).any(|(p, q)| p[..3] != q[..3]),
                "{name}: the LUT stage changed nothing"
            );
        }
    }
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn half_float_tiles_match_cpu_and_are_seamless_across_tile_sizes() {
    let (device, queue) = gpu();
    let image = image(67, 12);
    let renderer = TiledExposure::new(&device);
    for (name, settings) in cases() {
        let mut reference: Option<Vec<[f16; 4]>> = None;
        for edge in [67, 17, 13, 5] {
            let actual = render_tiles(&renderer, &device, &queue, &image, edge, &settings);
            for (p, q) in image.pixels().iter().zip(&actual) {
                let expected = settings.pixel(p.map(f16::to_f32));
                compare_stored(expected, q.map(f16::to_f32), &name);
            }
            match &reference {
                Some(reference) => assert_eq!(&actual, reference, "tile seam {name} edge {edge}"),
                None => reference = Some(actual),
            }
        }
    }
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn inactive_lut_is_bit_exact_with_no_lut_and_neutral_is_identity() {
    let (device, queue) = gpu();
    let runner = Runner::new(&device);
    let input = pixels(67, 10);
    let lut = Arc::new(film_lut(17, UNIT));
    let off = adjustments(&lut, 0.0);
    let none = BasicAdjustments::default();
    assert!(off.lut().is_none());
    let bits =
        |v: &[[f32; 4]]| -> Vec<[u32; 4]> { v.iter().map(|p| p.map(f32::to_bits)).collect() };
    let a = runner.run(&device, &queue, &input, &off);
    let b = runner.run(&device, &queue, &input, &none);
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(bits(&b), bits(&input), "neutral edit must be the identity");
    let image = image(67, 10);
    let renderer = TiledExposure::new(&device);
    let tiled_off = render_tiles(&renderer, &device, &queue, &image, 17, &off);
    assert_eq!(tiled_off, image.pixels());
    assert_eq!(renderer.lut_uploads(), 0);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn tiled_slot_uploads_once_per_lut_not_per_tile() {
    let (device, queue) = gpu();
    let image = image(40, 24);
    let renderer = TiledExposure::new(&device);
    let lut = Arc::new(film_lut(17, UNIT));
    // 40x24 with edge 8 is 15 tiles.
    render_tiles(
        &renderer,
        &device,
        &queue,
        &image,
        8,
        &adjustments(&lut, 1.0),
    );
    assert_eq!(renderer.lut_uploads(), 1);
    // Strength change and an equal lattice behind a different Arc reuse the texture.
    render_tiles(
        &renderer,
        &device,
        &queue,
        &image,
        8,
        &adjustments(&lut, 0.4),
    );
    let copy = Arc::new(film_lut(17, UNIT));
    render_tiles(
        &renderer,
        &device,
        &queue,
        &image,
        8,
        &adjustments(&copy, 1.0),
    );
    assert_eq!(renderer.lut_uploads(), 1);
    // Removing then restoring rebinds without breaking the render.
    let none = render_tiles(
        &renderer,
        &device,
        &queue,
        &image,
        8,
        &adjustments(&lut, 0.0),
    );
    assert_eq!(none, image.pixels());
    let back = adjustments(&lut, 1.0);
    let actual = render_tiles(&renderer, &device, &queue, &image, 8, &back);
    assert_eq!(renderer.lut_uploads(), 1);
    for (p, q) in image.pixels().iter().zip(&actual) {
        compare_stored(back.pixel(p.map(f16::to_f32)), q.map(f16::to_f32), "rebind");
    }
    // A different lattice uploads again.
    let other = Arc::new(film_lut(17, SHIFTED));
    render_tiles(
        &renderer,
        &device,
        &queue,
        &image,
        8,
        &adjustments(&other, 1.0),
    );
    assert_eq!(renderer.lut_uploads(), 2);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn export_snapshot_keeps_its_own_lut() {
    let (device, queue) = gpu();
    let image: ImageBuffer = image(30, 9);
    let renderer = TiledExposure::new(&device);
    let first = Arc::new(film_lut(17, UNIT));
    let snapshot = adjustments(&first, 1.0);
    // Later edits create other snapshots with other LUTs; the first is unaffected.
    let second = Arc::new(film_lut(33, SHIFTED));
    let _later = adjustments(&second, 1.0);
    let actual = render_tiles(&renderer, &device, &queue, &image, 11, &snapshot);
    for (p, q) in image.pixels().iter().zip(&actual) {
        compare_stored(
            snapshot.pixel(p.map(f16::to_f32)),
            q.map(f16::to_f32),
            "snapshot",
        );
    }
}

fn render_viewport(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &ExposureRenderer,
    image: &ImageBuffer,
    settings: &BasicAdjustments,
) -> Vec<u8> {
    let (w, h) = (image.width(), image.height());
    let source = renderer.upload(device, queue, image);
    renderer.set_viewport(queue, [0.0, 0.0, w as f32, h as f32]);
    renderer.set_curves(queue, settings);
    renderer.set_lut(queue, settings).unwrap();
    let frame = renderer.frame_region_adjusted(device, &source, settings, [0.0, 0.0, 1.0, 1.0]);
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let stride = (w * 4).next_multiple_of(256);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(stride * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    let view = output.create_view(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        renderer.paint(&mut pass, &frame);
    }
    encoder.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(h),
            },
        },
        output.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = staging.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::new();
    for row in mapped.chunks_exact(stride as usize) {
        out.extend_from_slice(&row[..(w * 4) as usize]);
    }
    drop(mapped);
    staging.unmap();
    out
}

/// The checkerboard-composited 8-bit output the viewport must produce.
fn expected_viewport(
    settings: &BasicAdjustments,
    image: &ImageBuffer,
    x: usize,
    y: usize,
) -> [i32; 3] {
    let p = image.pixels()[y * image.width() as usize + x];
    let alpha = p[3].to_f32();
    let working = settings.pixel(p.map(f16::to_f32));
    let rgb = filter_core::color::rec2020_to_linear_srgb(working[..3].try_into().unwrap());
    let checker = if (x / 12 + y / 12).is_multiple_of(2) {
        0.28
    } else {
        0.18
    };
    rgb.map(|v| {
        let mixed = checker * (1.0 - alpha) + v.clamp(0.0, 1.0) * alpha;
        (filter_core::color::linear_to_srgb(mixed) * 255.0).round() as i32
    })
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn viewport_renders_lut_with_premultiplied_alpha_and_rebinds_after_remove() {
    let (device, queue) = gpu();
    let renderer = ExposureRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
    let image = image(24, 6);
    let lut = Arc::new(film_lut(65, SHIFTED));
    let mut changed = false;
    for (round, strength) in [1.0, 0.5, 0.0, 1.0].into_iter().enumerate() {
        let settings = graded(&lut, strength);
        let actual = render_viewport(&device, &queue, &renderer, &image, &settings);
        for y in 0..6 {
            for x in 0..24 {
                let expected = expected_viewport(&settings, &image, x, y);
                for c in 0..3 {
                    let got = i32::from(actual[(y * 24 + x) * 4 + c]);
                    assert!(
                        (got - expected[c]).abs() <= 1,
                        "round {round} ({x},{y}): {got} vs {expected:?}"
                    );
                }
            }
        }
        changed |= round == 0
            && actual != render_viewport(&device, &queue, &renderer, &image, &graded(&lut, 0.0));
    }
    assert!(changed, "the viewport LUT changed nothing");
    // One upload covers all rounds: strength changes never re-upload the lattice.
    assert_eq!(renderer.lut_uploads(), 1);
    renderer
        .set_lut(&queue, &adjustments(&Arc::new(film_lut(65, SHIFTED)), 1.0))
        .unwrap();
    assert_eq!(
        renderer.lut_uploads(),
        1,
        "an equal lattice behind a new Arc"
    );
    renderer
        .set_lut(&queue, &adjustments(&Arc::new(film_lut(17, UNIT)), 1.0))
        .unwrap();
    assert_eq!(renderer.lut_uploads(), 2);
}

/// Total absolute difference between two histograms' bins.
fn bin_distance(a: &Histogram, b: &Histogram) -> u32 {
    a.bins
        .iter()
        .flatten()
        .zip(b.bins.iter().flatten())
        .map(|(x, y)| x.abs_diff(*y))
        .sum()
}
fn collect_histogram(gpu: &mut GpuHistogram, device: &wgpu::Device) -> Histogram {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        device.poll(wgpu::PollType::Poll).unwrap();
        if let Some((_, result)) = gpu.collect().pop() {
            return result.unwrap();
        }
        assert!(Instant::now() < deadline, "histogram timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn histogram_with_lut_matches_cpu_and_caches_the_lattice() {
    let (device, queue) = gpu();
    let renderer = ExposureRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
    let image = image(67, 12);
    let source = renderer.upload(&device, &queue, &image);
    let mut gpu = GpuHistogram::new(&device).unwrap();
    let lut = Arc::new(film_lut(33, SHIFTED));
    let neutral = Histogram::reference_preview(&image, &BasicAdjustments::default()).unwrap();
    let mut generation = 0;
    for settings in [
        adjustments(&lut, 1.0),
        graded(&lut, 0.5),
        adjustments(&lut, 0.0),
        adjustments(&lut, 1.0),
    ] {
        generation += 1;
        assert!(
            gpu.try_submit(&device, &queue, &source, &settings, generation)
                .unwrap()
        );
        let actual = collect_histogram(&mut gpu, &device);
        let expected = Histogram::reference_preview(&image, &settings).unwrap();
        assert_eq!(actual.sampled, expected.sampled);
        assert_eq!(actual.invalid, expected.invalid);
        let moved = bin_distance(&actual, &expected);
        assert_eq!(moved, 0, "histogram bins differ");
        if settings.lut().is_none() {
            assert_eq!(
                actual.bins, neutral.bins,
                "strength 0 is the no-LUT histogram"
            );
        }
    }
    assert_eq!(gpu.lut_uploads(), 1);
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn probe_with_lut_matches_cpu_and_caches_the_lattice() {
    let (device, queue) = gpu();
    let mut probe = GpuProbe::new(&device).unwrap();
    let lut = Arc::new(film_lut(17, SHIFTED));
    let pixel = [0.2, 0.5, 1.3, 1.0];
    let mut generation = 0;
    for settings in [
        adjustments(&lut, 1.0),
        graded(&lut, 0.5),
        adjustments(&lut, 0.0),
    ] {
        generation += 1;
        assert!(probe.try_submit(&device, &queue, pixel, &settings, generation));
        let deadline = Instant::now() + Duration::from_secs(30);
        let actual = loop {
            device.poll(wgpu::PollType::Poll).unwrap();
            if let Some((_, result)) = probe.collect().pop() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline, "probe timed out");
            std::thread::sleep(Duration::from_millis(1));
        };
        let expected = ProbeSample::reference(pixel, &settings);
        match (actual, expected) {
            (ProbeSample::Color { rgb: a, .. }, ProbeSample::Color { rgb: b, .. }) => {
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} != {b:?}");
                }
                assert_eq!(actual.rgb8(), expected.rgb8());
            }
            (a, b) => assert_eq!(a, b),
        }
    }
    assert_eq!(probe.lut_uploads(), 1);
}
