use eframe::egui;
use filter_core::cpu_eval::BasicAdjustments;
use filter_gpu::{ExposureFrame, ExposureRenderer, ExposureUpdates, SourceTexture, Tile};
use filter_io::DecodedImage;
use std::sync::{Arc, mpsc};

pub struct PreviewCallback {
    pub renderer: Arc<ExposureRenderer>,
    pub frames: Vec<ExposureFrame>,
    pub viewport: egui::Rect,
}
impl egui_wgpu::CallbackTrait for PreviewCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        _resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let bounds = egui::PaintCallbackInfo {
            viewport: self.viewport,
            clip_rect: self.viewport,
            pixels_per_point: screen.pixels_per_point,
            screen_size_px: screen.size_in_pixels,
        }
        .viewport_in_pixels();
        if bounds.width_px > 0 && bounds.height_px > 0 {
            self.renderer.set_viewport(
                queue,
                [
                    bounds.left_px as f32,
                    bounds.top_px as f32,
                    bounds.width_px as f32,
                    bounds.height_px as f32,
                ],
            );
        }
        Vec::new()
    }
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

#[cfg(test)]
impl RegionPreview {
    /// Full-resolution tiles drawn at least once since the last reset; only
    /// tiles returned to the viewport for painting have frames.
    pub fn drawn_tiles(&self) -> usize {
        self.cache.iter().filter(|c| c.frame.is_some()).count()
    }
    pub fn is_pending(&self) -> bool {
        self.pending
    }
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
#[path = "../tests/unit/preview_tests.rs"]
mod tests;
