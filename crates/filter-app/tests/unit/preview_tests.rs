use super::{
    Arc, BasicAdjustments, CACHE_BYTES, CACHE_TILES, DecodedImage, ExposureFrame, ExposureRenderer,
    ExposureUpdates, RegionPreview, Request, TILE_EDGE, Tile, egui, mpsc, needs_regions,
    pixel_aligned, visible_tiles, with_border,
};
use filter_core::ImageBuffer;
use filter_gpu::SourceTexture;
use half::f16;
use std::time::{Duration, Instant};

fn rect(x: f32, y: f32, width: f32, height: f32) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
}

#[test]
fn minified_views_keep_the_area_filtered_proxy() {
    for proxy_scale in [0.01, 2048.0 / 6000.0, 0.75] {
        for physical_scale in [proxy_scale, (proxy_scale + 1.0) * 0.5, 0.999] {
            assert!(!needs_regions(proxy_scale, physical_scale));
        }
        assert!(needs_regions(proxy_scale, 1.0));
        assert!(needs_regions(proxy_scale, 1.25));
    }
    assert!(!needs_regions(1.0, 1.0));
    assert!(!needs_regions(1.0, 8.0));
}

#[test]
fn native_image_and_clipped_callback_align_on_hidpi_displays() {
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        let image = pixel_aligned(rect(-120.3, 31.7, 1031.0 / ppp, 601.0 / ppp), ppp);
        assert!((image.width() * ppp - 1031.0).abs() < 0.001);
        assert!((image.height() * ppp - 601.0).abs() < 0.001);
        let visible = pixel_aligned(image.intersect(rect(10.2, 40.5, 500.3, 300.7)), ppp);
        let origin = (visible.min - image.min) * ppp;
        for value in [
            origin.x,
            origin.y,
            visible.width() * ppp,
            visible.height() * ppp,
        ] {
            assert!((value - value.round()).abs() < 0.001);
        }
    }
}

#[test]
fn visible_tiles_cover_fractional_regions_and_clip_image_edges() {
    let visible = rect(510.5, 511.25, 521.0, 100.0);
    let tiles = visible_tiles(1031, 601, visible, TILE_EDGE);
    assert_eq!(tiles.len(), 6);
    for y in 511..601 {
        for x in 510..1031 {
            assert_eq!(
                tiles
                    .iter()
                    .filter(|t| x >= t.x && x < t.x + t.width && y >= t.y && y < t.y + t.height)
                    .count(),
                1
            );
        }
    }
    for tile in tiles {
        let border = with_border(tile, 1031, 601);
        assert!(border.x <= tile.x && border.y <= tile.y);
        assert!(border.x + border.width <= 1031 && border.y + border.height <= 601);
        assert!(border.width <= TILE_EDGE + 2 && border.height <= TILE_EDGE + 2);
    }
    assert!(visible_tiles(10, 10, rect(11.0, 0.0, 2.0, 2.0), 512).is_empty());
    assert!(visible_tiles(10, 10, rect(0.0, 0.0, 0.0, 2.0), 512).is_empty());
    assert_eq!(
        visible_tiles(10, 10, rect(-10.0, -10.0, 30.0, 30.0), 512).len(),
        1
    );
}

#[test]
fn huge_regions_have_a_stable_bounded_center_first_working_set() {
    let visible = rect(0.0, 0.0, 10000.0, 10000.0);
    let tiles = visible_tiles(10000, 10000, visible, TILE_EDGE);
    assert_eq!(tiles.len(), CACHE_TILES);
    assert_eq!(tiles, visible_tiles(10000, 10000, visible, TILE_EDGE));
    assert!(
        tiles
            .iter()
            .map(|t| u64::from(with_border(*t, 10000, 10000).width)
                * u64::from(with_border(*t, 10000, 10000).height)
                * 8)
            .sum::<u64>()
            <= CACHE_BYTES as u64
    );
    assert!((tiles[0].x as f32 - visible.center().x).abs() <= TILE_EDGE as f32);
}

fn source(width: u32, height: u32, reverse: bool) -> Arc<DecodedImage> {
    Arc::new(DecodedImage {
        buffer: ImageBuffer::new(
            width,
            height,
            (0..width * height)
                .map(|i| {
                    let x = (i % width) as f32 / width as f32;
                    let y = (i / width) as f32 / height as f32;
                    [
                        if reverse { 1.0 - x } else { x },
                        y,
                        if i % 2 == 0 { 0.8 } else { 0.01 },
                        [0.0, 0.25, 0.5, 1.0][i as usize % 4],
                    ]
                    .map(f16::from_f32)
                })
                .collect(),
        )
        .unwrap(),
        source_orientation: image::metadata::Orientation::NoTransforms,
        icc_profile: None,
        exif_bytes: None,
    })
}

fn refined(
    regions: &mut RegionPreview,
    renderer: &ExposureRenderer,
    device: &wgpu::Device,
    updates: &mut ExposureUpdates,
    source: &Arc<DecodedImage>,
    visible: egui::Rect,
    settings: &BasicAdjustments,
) -> Vec<ExposureFrame> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let frames = regions.frames(renderer, device, updates, source, visible, settings);
        updates.flush();
        assert!(regions.error.is_none(), "{:?}", regions.error);
        if !regions.pending {
            assert_eq!(
                frames.len(),
                visible_tiles(
                    source.buffer.width(),
                    source.buffer.height(),
                    visible,
                    regions.edge
                )
                .len()
            );
            return frames;
        }
        assert!(Instant::now() < deadline, "region worker timed out");
        device.poll(wgpu::PollType::Poll).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn render(
    renderer: &ExposureRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    size: (u32, u32),
    origin: (u32, u32),
    frames: &[ExposureFrame],
) -> Vec<u8> {
    renderer.set_viewport(
        queue,
        [
            origin.0 as f32,
            origin.1 as f32,
            size.0 as f32,
            size.1 as f32,
        ],
    );
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("region test target"),
        size: wgpu::Extent3d {
            width: size.0 + origin.0,
            height: size.1 + origin.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let stride = ((size.0 + origin.0) * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(stride) * u64::from(size.1 + origin.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = texture.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_viewport(
            origin.0 as f32,
            origin.1 as f32,
            size.0 as f32,
            size.1 as f32,
            0.0,
            1.0,
        );
        for frame in frames {
            renderer.paint(&mut pass, frame);
        }
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(size.1 + origin.1),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let result = mapped
        .chunks_exact(stride as usize)
        .skip(origin.1 as usize)
        .take(size.1 as usize)
        .flat_map(|r| {
            r[origin.0 as usize * 4..(origin.0 + size.0) as usize * 4]
                .iter()
                .copied()
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    result
}

fn check_tile_failure_recovery(
    renderer: &Arc<ExposureRenderer>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    let mut regions = RegionPreview::new(
        renderer.clone(),
        device.clone(),
        queue.clone(),
        egui::Context::default(),
    );
    // Drive the worker protocol explicitly to inject a transient allocation
    // failure without depending on real memory exhaustion or thread timing.
    let (requests, jobs) = mpsc::sync_channel(1);
    let (results_tx, results) = mpsc::sync_channel(1);
    regions.requests = requests;
    regions.results = results;
    regions.edge = 16;
    let image = source(33, 17, false);
    let visible = rect(0.0, 0.0, 33.0, 17.0);
    let settings = BasicAdjustments::default();
    let mut updates = ExposureUpdates::new(device, queue);
    let complete = |job: Request| {
        let texture = renderer
            .upload_region(device, queue, &job.source.buffer, job.border)
            .unwrap();
        results_tx
            .send((job.generation, job.tile, Ok(texture)))
            .unwrap();
    };
    regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    let first = jobs.recv_timeout(Duration::from_secs(1)).unwrap();
    let failed_tile = first.tile;
    results_tx
        .send((
            first.generation,
            first.tile,
            Err("simulated allocation failure".into()),
        ))
        .unwrap();
    regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    assert!(regions.error.is_some());
    assert!(regions.can_retry());
    let second = jobs.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ne!(second.tile, failed_tile, "other tiles must keep refining");
    complete(second);
    let frames = regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    updates.flush();
    assert_eq!(frames.len(), 1);
    let old_pending = jobs.recv_timeout(Duration::from_secs(1)).unwrap();
    // This is the Retry preview action: free the cache, clear failed tiles,
    // and invalidate the outstanding result without decoding the image.
    regions.reset();
    assert!(regions.error.is_none());
    assert!(regions.failed.is_empty());
    complete(old_pending);
    assert!(
        regions
            .frames(renderer, device, &mut updates, &image, visible, &settings)
            .is_empty()
    );
    let retried = jobs.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(retried.tile, failed_tile);
    assert_eq!(retried.generation, regions.generation);
    complete(retried);
    assert_eq!(
        regions
            .frames(renderer, device, &mut updates, &image, visible, &settings)
            .len(),
        1
    );
    updates.flush();
    assert!(!regions.can_retry());
    assert!(regions.error.is_none());

    // A full channel represents queued work, not a failed image. Its eventual
    // result restores scheduling rather than permanently disabling refinement.
    regions.pending = false;
    regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    updates.flush();
    assert!(regions.pending);
    assert!(regions.error.is_none());
    complete(jobs.recv_timeout(Duration::from_secs(1)).unwrap());
    regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    updates.flush();
    assert!(regions.error.is_none());
    assert!(regions.cache.len() >= 2);

    // A disconnected worker is reported as unavailable instead of hanging in
    // the pending state or offering a tile retry that cannot restart it.
    drop(jobs);
    drop(results_tx);
    regions.frames(renderer, device, &mut updates, &image, visible, &settings);
    updates.flush();
    assert!(!regions.pending);
    assert!(regions.error.is_some());
    assert!(!regions.can_retry());
}

/// A software device and renderer for one surface format.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Arc<ExposureRenderer>,
    format: wgpu::TextureFormat,
}
impl Harness {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            renderer: Arc::new(ExposureRenderer::new(device, format)),
            format,
        }
    }
    fn regions(&self) -> RegionPreview {
        RegionPreview::new(
            self.renderer.clone(),
            self.device.clone(),
            self.queue.clone(),
            egui::Context::default(),
        )
    }
    fn updates(&self) -> ExposureUpdates {
        ExposureUpdates::new(&self.device, &self.queue)
    }
    fn refined(
        &self,
        regions: &mut RegionPreview,
        updates: &mut ExposureUpdates,
        source: &Arc<DecodedImage>,
        visible: egui::Rect,
        settings: &BasicAdjustments,
    ) -> Vec<ExposureFrame> {
        refined(
            regions,
            &self.renderer,
            &self.device,
            updates,
            source,
            visible,
            settings,
        )
    }
    fn render(&self, size: (u32, u32), origin: (u32, u32), frames: &[ExposureFrame]) -> Vec<u8> {
        render(
            &self.renderer,
            &self.device,
            &self.queue,
            self.format,
            size,
            origin,
            frames,
        )
    }
    /// Untiled proxy-path frame showing the `visible` source pixels.
    fn untiled(
        &self,
        texture: &SourceTexture,
        source: &DecodedImage,
        visible: egui::Rect,
        settings: &BasicAdjustments,
    ) -> ExposureFrame {
        let (w, h) = (source.buffer.width() as f32, source.buffer.height() as f32);
        self.renderer.frame_region_adjusted(
            &self.device,
            texture,
            settings,
            [
                visible.min.x / w,
                visible.min.y / h,
                visible.width() / w,
                visible.height() / h,
            ],
        )
    }
}

fn assert_bytes_close(actual: &[u8], expected: &[u8], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}");
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (i32::from(*a) - i32::from(*b)).abs() <= 1,
            "{context}, byte {i}: tiled {a}, untiled {b}"
        );
    }
}

/// At native scale each physical pixel must reproduce its source texel over
/// the checkerboard, even beside a high-contrast neighbor. `origin` is the
/// viewport origin used by the checkerboard.
fn assert_native_pixels(
    source: &DecodedImage,
    settings: &BasicAdjustments,
    shown: &[u8],
    origin: (usize, usize),
) {
    let width = source.buffer.width() as usize;
    for (i, (pixel, shown)) in source
        .buffer
        .pixels()
        .iter()
        .zip(shown.as_chunks::<4>().0)
        .enumerate()
    {
        let [r, g, b, alpha] = settings.pixel(pixel.map(f16::to_f32));
        let rgb = filter_core::color::rec2020_to_linear_srgb([r, g, b]);
        let (x, y) = (i % width + origin.0, i / width + origin.1);
        let checker = if (x / 12 + y / 12) % 2 == 0 {
            0.28
        } else {
            0.18
        };
        for (channel, linear) in rgb.iter().enumerate() {
            let composite = checker * (1.0 - alpha) + linear.clamp(0.0, 1.0) * alpha;
            let expected = (filter_core::color::linear_to_srgb(composite) * 255.0).round() as i32;
            assert!(
                (i32::from(shown[channel]) - expected).abs() <= 1,
                "native pixel {i}, channel {channel}: {} vs {expected}",
                shown[channel]
            );
        }
    }
}

/// A result requested for an old source is discarded by reset.
fn check_stale_source_is_discarded(
    gpu: &Harness,
    regions: &mut RegionPreview,
    updates: &mut ExposureUpdates,
) {
    let old = source(67, 37, false);
    let frames = regions.frames(
        &gpu.renderer,
        &gpu.device,
        updates,
        &old,
        rect(0.0, 0.0, 67.0, 37.0),
        &BasicAdjustments::default(),
    );
    assert!(frames.is_empty());
    assert!(regions.pending);
    regions.reset();
}

/// Tiled output matches the untiled path at native and fractional scales,
/// and panning or edits reuse cached tiles and their bind groups.
fn check_tiles_match_untiled_across_pans_and_edits(
    gpu: &Harness,
    regions: &mut RegionPreview,
    updates: &mut ExposureUpdates,
    image: &Arc<DecodedImage>,
) {
    let texture = gpu.renderer.upload(&gpu.device, &gpu.queue, &image.buffer);
    let full = rect(0.0, 0.0, 67.0, 37.0);
    let mut bindings = Vec::new();
    for settings in [
        BasicAdjustments::default(),
        BasicAdjustments::exposure(1.25).unwrap(),
    ] {
        for (visible, size) in [(full, (67, 37)), (rect(14.25, 14.5, 51.5, 21.25), (97, 69))] {
            let frames = gpu.refined(regions, updates, image, visible, &settings);
            let reference = gpu.untiled(&texture, image, visible, &settings);
            let expected = gpu.render(size, (7, 9), &[reference]);
            let actual = gpu.render(size, (7, 9), &frames);
            assert_bytes_close(&actual, &expected, &format!("{:?} {visible:?}", gpu.format));
            if visible == full {
                assert_native_pixels(image, &settings, &actual, (7, 9));
            }
        }
        let current: Vec<_> = regions
            .cache
            .iter()
            .map(|c| (c.tile, c.frame.as_ref().unwrap().bind_group.clone()))
            .collect();
        if bindings.is_empty() {
            bindings = current;
        } else {
            assert_eq!(
                bindings, current,
                "panning and edits must reuse bind groups and their uniform buffers"
            );
        }
    }
    // Offscreen tiles survive panning and adjustment changes without uploads.
    assert_eq!(regions.cache.len(), 15);
    assert!(!regions.pending);
}

/// Views larger than the cache evict tiles and keep a stable working set.
fn check_cache_stays_bounded(
    gpu: &Harness,
    regions: &mut RegionPreview,
    updates: &mut ExposureUpdates,
    large: &Arc<DecodedImage>,
) {
    regions.reset();
    let settings = BasicAdjustments::default();
    for view in [rect(0.0, 0.0, 257.0, 257.0), rect(180.0, 180.0, 77.0, 77.0)] {
        gpu.refined(regions, updates, large, view, &settings);
        assert_eq!(regions.cache.len(), CACHE_TILES);
    }
    assert!(!regions.pending);
}

/// The source exceeds the granted texture limit on both axes. Compare against
/// an untiled reference on the unrestricted device.
fn check_texture_limited_tiles(gpu: &Harness, limited: &Harness, large: &Arc<DecodedImage>) {
    let mut regions = limited.regions();
    assert_eq!(regions.edge, 62);
    let mut updates = limited.updates();
    let visible = rect(60.25, 60.5, 128.5, 125.25);
    let settings = BasicAdjustments::exposure(-0.5).unwrap();
    let frames = limited.refined(&mut regions, &mut updates, large, visible, &settings);
    let texture = gpu.renderer.upload(&gpu.device, &gpu.queue, &large.buffer);
    let reference = gpu.untiled(&texture, large, visible, &settings);
    let expected = gpu.render((63, 61), (0, 0), &[reference]);
    let actual = limited.render((63, 61), (0, 0), &frames);
    assert_bytes_close(&actual, &expected, &format!("{:?} limited", gpu.format));
    let too_wide = Tile {
        x: 0,
        y: 0,
        width: 65,
        height: 1,
    };
    assert!(
        limited
            .renderer
            .upload_region(&limited.device, &limited.queue, &large.buffer, too_wide)
            .is_err()
    );
}

fn check_invalid_tile_uploads_fail(gpu: &Harness, image: &DecodedImage) {
    for tile in [
        Tile {
            x: 0,
            y: 0,
            width: 0,
            height: 1,
        },
        Tile {
            x: u32::MAX,
            y: 0,
            width: 2,
            height: 1,
        },
    ] {
        assert!(
            gpu.renderer
                .upload_region(&gpu.device, &gpu.queue, &image.buffer, tile)
                .is_err()
        );
    }
}

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn full_resolution_regions_match_untiled_and_reject_stale_uploads() {
    let (device, queue, limited_device, limited_queue) = pollster::block_on(async {
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
        let (limited_device, limited_queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: 64,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap();
        (device, queue, limited_device, limited_queue)
    });
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let gpu = Harness::new(&device, &queue, format);
        let limited = Harness::new(&limited_device, &limited_queue, format);
        check_tile_failure_recovery(&gpu.renderer, &device, &queue);
        let mut regions = gpu.regions();
        regions.edge = 16;
        let mut updates = gpu.updates();
        check_stale_source_is_discarded(&gpu, &mut regions, &mut updates);
        let image = source(67, 37, true);
        check_tiles_match_untiled_across_pans_and_edits(&gpu, &mut regions, &mut updates, &image);
        let large = source(257, 257, false);
        check_cache_stays_bounded(&gpu, &mut regions, &mut updates, &large);
        check_texture_limited_tiles(&gpu, &limited, &large);
        check_invalid_tile_uploads_fail(&gpu, &image);
    }
}
