use eframe::egui;
use filter_core::cpu_eval::BasicAdjustments;
use filter_gpu::{ExposureFrame, ExposureRenderer, ExposureUpdates, SourceTexture, Tile};
use filter_io::DecodedImage;
use std::sync::{Arc, mpsc};

pub struct PreviewCallback {
    pub renderer: Arc<ExposureRenderer>,
    pub frames: Vec<ExposureFrame>,
}
impl egui_wgpu::CallbackTrait for PreviewCallback {
    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        for frame in &self.frames {
            self.renderer.paint(pass, frame);
        }
    }
}

const TILE_EDGE: u32 = 512;
// Includes the one-pixel filtering border on every side, at 8 bytes/pixel.
const CACHE_BYTES: usize = 128 * 1024 * 1024;
const CACHE_TILES: usize = CACHE_BYTES / ((TILE_EDGE as usize + 2).pow(2) * 8);
const _: () = assert!(
    CACHE_TILES > 0,
    "preview budget must hold at least one bordered tile"
);

/// Single-level source textures must not replace the area-filtered proxy while
/// minifying. A mip/area-filtered region path can lower this threshold later.
pub fn needs_regions(proxy_scale: f32, physical_scale: f32) -> bool {
    proxy_scale < 1.0 && physical_scale >= 1.0
}

/// Align native-resolution image and callback bounds to the physical display grid.
pub fn pixel_aligned(rect: egui::Rect, pixels_per_point: f32) -> egui::Rect {
    let align = |p: egui::Pos2| {
        egui::pos2(
            (p.x * pixels_per_point).round() / pixels_per_point,
            (p.y * pixels_per_point).round() / pixels_per_point,
        )
    };
    egui::Rect::from_min_max(align(rect.min), align(rect.max))
}

struct Request {
    generation: u64,
    source: Arc<DecodedImage>,
    tile: Tile,
    border: Tile,
}
struct CachedTile {
    tile: Tile,
    texture: SourceTexture,
    frame: Option<ExposureFrame>,
    used: u64,
}
type TileResult = (u64, Tile, Result<SourceTexture, String>);

/// Immutable source tiles are cached, rather than rendered edits. Every draw uses
/// the current adjustment snapshot, so slider/preset changes cannot show stale edits.
/// A single background upload bounds CPU work, staging memory and queued requests.
pub struct RegionPreview {
    requests: mpsc::SyncSender<Request>,
    results: mpsc::Receiver<TileResult>,
    cache: Vec<CachedTile>,
    failed: Vec<Tile>,
    worker_available: bool,
    generation: u64,
    pending: bool,
    clock: u64,
    edge: u32,
    pub error: Option<String>,
}

impl RegionPreview {
    pub fn new(
        renderer: Arc<ExposureRenderer>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        ctx: egui::Context,
    ) -> Self {
        let edge = TILE_EDGE.min(device.limits().max_texture_dimension_2d.saturating_sub(2));
        let (requests, jobs) = mpsc::sync_channel::<Request>(1);
        let (results_tx, results) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                let result = renderer
                    .upload_region(&device, &queue, &job.source.buffer, job.border)
                    .map_err(|e| format!("{e:#}"));
                // Flush the upload; later viewport submissions on the same queue
                // see it in order, without a readback or a blocking GPU poll.
                queue.submit([]);
                if results_tx.send((job.generation, job.tile, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            requests,
            results,
            cache: Vec::new(),
            failed: Vec::new(),
            worker_available: true,
            generation: 0,
            pending: false,
            clock: 0,
            edge,
            error: None,
        }
    }

    pub fn reset(&mut self) {
        self.generation += 1;
        self.cache.clear();
        self.failed.clear();
        self.error = None;
        // Keep the outstanding request accounted for until its result arrives.
    }

    pub fn frames(
        &mut self,
        renderer: &ExposureRenderer,
        device: &wgpu::Device,
        updates: &mut ExposureUpdates,
        source: &Arc<DecodedImage>,
        visible: egui::Rect,
        adjustments: &BasicAdjustments,
    ) -> Vec<ExposureFrame> {
        self.clock += 1;
        loop {
            let (generation, tile, result) = match self.results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = false;
                    self.worker_available = false;
                    self.error = Some("Full-resolution preview worker unavailable".into());
                    break;
                }
            };
            self.pending = false;
            if generation != self.generation {
                continue;
            }
            match result {
                Ok(texture) => {
                    if self.cache.len() == CACHE_TILES {
                        let oldest = self
                            .cache
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, t)| t.used)
                            .unwrap()
                            .0;
                        self.cache.swap_remove(oldest);
                    }
                    self.cache.push(CachedTile {
                        tile,
                        texture,
                        frame: None,
                        used: self.clock,
                    });
                }
                Err(error) => {
                    self.failed.push(tile);
                    self.error = Some(format!("Full-resolution tile unavailable: {error}"));
                }
            }
        }
        let width = source.buffer.width();
        let height = source.buffer.height();
        let wanted = visible_tiles(width, height, visible, self.edge);
        let mut frames = Vec::new();
        for tile in &wanted {
            if let Some(cached) = self.cache.iter_mut().find(|c| c.tile == *tile) {
                cached.used = self.clock;
                let border = with_border(*tile, width, height);
                let uv = [
                    (tile.x - border.x) as f32 / border.width as f32,
                    (tile.y - border.y) as f32 / border.height as f32,
                    tile.width as f32 / border.width as f32,
                    tile.height as f32 / border.height as f32,
                ];
                let rect = [
                    (tile.x as f32 - visible.min.x) / visible.width(),
                    (tile.y as f32 - visible.min.y) / visible.height(),
                    tile.width as f32 / visible.width(),
                    tile.height as f32 / visible.height(),
                ];
                let frame = cached.frame.get_or_insert_with(|| {
                    renderer.frame_region_placed(device, &cached.texture, adjustments, uv, rect)
                });
                renderer.update_frame(updates, frame, adjustments, uv, rect);
                frames.push(frame.clone());
            }
        }
        if !self.pending
            && self.worker_available
            && let Some(tile) = wanted
                .iter()
                .find(|t| !self.failed.contains(t) && !self.cache.iter().any(|c| c.tile == **t))
        {
            let request = Request {
                generation: self.generation,
                source: source.clone(),
                tile: *tile,
                border: with_border(*tile, width, height),
            };
            match self.requests.try_send(request) {
                Ok(()) => self.pending = true,
                // A full queue already contains work whose result will wake us.
                Err(mpsc::TrySendError::Full(_)) => self.pending = true,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.worker_available = false;
                    self.error = Some("Full-resolution preview worker unavailable".into());
                }
            }
        }
        frames
    }

    pub fn can_retry(&self) -> bool {
        self.worker_available && !self.failed.is_empty()
    }
}

fn with_border(tile: Tile, width: u32, height: u32) -> Tile {
    let x = tile.x.saturating_sub(1);
    let y = tile.y.saturating_sub(1);
    Tile {
        x,
        y,
        width: (tile.x + tile.width).saturating_add(1).min(width) - x,
        height: (tile.y + tile.height).saturating_add(1).min(height) - y,
    }
}

fn visible_tiles(width: u32, height: u32, visible: egui::Rect, edge: u32) -> Vec<Tile> {
    let visible = visible.intersect(egui::Rect::from_min_max(
        egui::Pos2::ZERO,
        egui::pos2(width as f32, height as f32),
    ));
    if !visible.is_positive() || edge == 0 {
        return Vec::new();
    }
    let mut tiles = Vec::new();
    for y in (visible.min.y as u32 / edge * edge..(visible.max.y.ceil() as u32).min(height))
        .step_by(edge as usize)
    {
        for x in (visible.min.x as u32 / edge * edge..(visible.max.x.ceil() as u32).min(width))
            .step_by(edge as usize)
        {
            tiles.push(Tile {
                x,
                y,
                width: edge.min(width - x),
                height: edge.min(height - y),
            });
        }
    }
    // Refine the center first. If an unusually large visible source region exceeds
    // the cache budget, keep a stable central subset and use the proxy elsewhere.
    let center = visible.center();
    tiles.sort_by(|a, b| {
        let distance = |t: &Tile| {
            (egui::pos2(
                t.x as f32 + t.width as f32 * 0.5,
                t.y as f32 + t.height as f32 * 0.5,
            ) - center)
                .length_sq()
        };
        distance(a).total_cmp(&distance(b))
    });
    tiles.truncate(CACHE_TILES);
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use filter_core::ImageBuffer;
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
        frames: &[ExposureFrame],
    ) -> Vec<u8> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("region test target"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let stride = (size.0 * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride) * u64::from(size.1),
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
                    rows_per_image: Some(size.1),
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
            .flat_map(|r| r[..size.0 as usize * 4].iter().copied())
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

    #[test]
    #[ignore = "requires a Vulkan/DX12/Metal software adapter"]
    fn full_resolution_regions_match_untiled_and_reject_stale_uploads() {
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
            for format in [
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ] {
                let renderer = Arc::new(ExposureRenderer::new(&device, format));
                check_tile_failure_recovery(&renderer, &device, &queue);
                let mut updates = ExposureUpdates::new(&device, &queue);
                let mut regions = RegionPreview::new(
                    renderer.clone(),
                    device.clone(),
                    queue.clone(),
                    egui::Context::default(),
                );
                regions.edge = 16;
                let old = source(67, 37, false);
                let image = source(67, 37, true);
                let full = rect(0.0, 0.0, 67.0, 37.0);
                assert!(
                    regions
                        .frames(
                            &renderer,
                            &device,
                            &mut updates,
                            &old,
                            full,
                            &BasicAdjustments::default()
                        )
                        .is_empty()
                );
                assert!(regions.pending);
                regions.reset();
                let texture = renderer.upload(&device, &queue, &image.buffer);
                let mut bindings = Vec::new();
                for settings in [
                    BasicAdjustments::default(),
                    BasicAdjustments::exposure(1.25).unwrap(),
                ] {
                    for (visible, size) in
                        [(full, (67, 37)), (rect(14.25, 14.5, 51.5, 21.25), (97, 69))]
                    {
                        let frames = refined(
                            &mut regions,
                            &renderer,
                            &device,
                            &mut updates,
                            &image,
                            visible,
                            &settings,
                        );
                        let expected = render(
                            &renderer,
                            &device,
                            &queue,
                            format,
                            size,
                            &[renderer.frame_region_adjusted(
                                &device,
                                &texture,
                                &settings,
                                [
                                    visible.min.x / 67.0,
                                    visible.min.y / 37.0,
                                    visible.width() / 67.0,
                                    visible.height() / 37.0,
                                ],
                            )],
                        );
                        let actual = render(&renderer, &device, &queue, format, size, &frames);
                        for (i, (a, b)) in actual.iter().zip(&expected).enumerate() {
                            assert!(
                                (i32::from(*a) - i32::from(*b)).abs() <= 1,
                                "{format:?} {visible:?}, byte {i}: tiled {a}, untiled {b}"
                            );
                        }
                        if visible == full {
                            // At native scale each physical pixel must reproduce
                            // its source texel, even beside a high-contrast neighbor.
                            for (i, (pixel, shown)) in image
                                .buffer
                                .pixels()
                                .iter()
                                .zip(actual.as_chunks::<4>().0)
                                .enumerate()
                            {
                                let pixel = settings.pixel(pixel.map(f16::to_f32));
                                let rgb = filter_core::color::rec2020_to_linear_srgb(
                                    pixel[..3].try_into().unwrap(),
                                );
                                let checker = if (i % 67 / 12 + i / 67 / 12) % 2 == 0 {
                                    0.28
                                } else {
                                    0.18
                                };
                                for (channel, linear) in rgb.iter().enumerate() {
                                    let composite = checker * (1.0 - pixel[3])
                                        + linear.clamp(0.0, 1.0) * pixel[3];
                                    let expected =
                                        (filter_core::color::linear_to_srgb(composite) * 255.0)
                                            .round() as i32;
                                    assert!(
                                        (i32::from(shown[channel]) - expected).abs() <= 1,
                                        "native pixel {i}, channel {channel}: {} vs {expected}",
                                        shown[channel]
                                    );
                                }
                            }
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
                // Exercise the eviction path and stable behavior when a view exceeds capacity.
                regions.reset();
                let large = source(257, 257, false);
                let view = rect(0.0, 0.0, 257.0, 257.0);
                refined(
                    &mut regions,
                    &renderer,
                    &device,
                    &mut updates,
                    &large,
                    view,
                    &BasicAdjustments::default(),
                );
                assert_eq!(regions.cache.len(), CACHE_TILES);
                let shifted = rect(180.0, 180.0, 77.0, 77.0);
                refined(
                    &mut regions,
                    &renderer,
                    &device,
                    &mut updates,
                    &large,
                    shifted,
                    &BasicAdjustments::default(),
                );
                assert_eq!(regions.cache.len(), CACHE_TILES);
                assert!(!regions.pending);
                // The source exceeds the granted texture limit on both axes.
                // Compare against an untiled reference on the unrestricted device.
                let limited_renderer = Arc::new(ExposureRenderer::new(&limited_device, format));
                let mut limited_regions = RegionPreview::new(
                    limited_renderer.clone(),
                    limited_device.clone(),
                    limited_queue.clone(),
                    egui::Context::default(),
                );
                assert_eq!(limited_regions.edge, 62);
                let mut limited_updates = ExposureUpdates::new(&limited_device, &limited_queue);
                let visible = rect(60.25, 60.5, 128.5, 125.25);
                let settings = BasicAdjustments::exposure(-0.5).unwrap();
                let frames = refined(
                    &mut limited_regions,
                    &limited_renderer,
                    &limited_device,
                    &mut limited_updates,
                    &large,
                    visible,
                    &settings,
                );
                let large_texture = renderer.upload(&device, &queue, &large.buffer);
                let reference = renderer.frame_region_adjusted(
                    &device,
                    &large_texture,
                    &settings,
                    [
                        visible.min.x / 257.0,
                        visible.min.y / 257.0,
                        visible.width() / 257.0,
                        visible.height() / 257.0,
                    ],
                );
                let expected = render(&renderer, &device, &queue, format, (63, 61), &[reference]);
                let actual = render(
                    &limited_renderer,
                    &limited_device,
                    &limited_queue,
                    format,
                    (63, 61),
                    &frames,
                );
                assert!(
                    actual
                        .iter()
                        .zip(&expected)
                        .all(|(a, b)| (i32::from(*a) - i32::from(*b)).abs() <= 1)
                );
                assert!(
                    limited_renderer
                        .upload_region(
                            &limited_device,
                            &limited_queue,
                            &large.buffer,
                            Tile {
                                x: 0,
                                y: 0,
                                width: 65,
                                height: 1
                            }
                        )
                        .is_err()
                );
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
                        renderer
                            .upload_region(&device, &queue, &image.buffer, tile)
                            .is_err()
                    );
                }
            }
        });
    }
}
