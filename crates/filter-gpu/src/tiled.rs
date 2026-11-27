//! One-tile-per-submit working-space basic adjustments and asynchronous, row-padded readback.
use anyhow::{Context, Result, bail, ensure};
use filter_core::{ImageBuffer, cpu_eval::BasicAdjustments};
use half::f16;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use wgpu::util::DeviceExt;

const PIXEL_BYTES: u64 = 8;
const BAND_BUDGET: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Tiles respect granted texture/buffer limits and bound a CPU row band to 32 MiB
/// (or one scanline for exceptionally wide images). No full-image GPU allocation.
#[derive(Clone, Debug)]
pub struct TilePlan {
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
}
impl TilePlan {
    pub fn new(width: u32, height: u32, limits: &wgpu::Limits, max_edge: u32) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && max_edge > 0,
            "Invalid tile dimensions"
        );
        // Narrow the tile too when even a single preferred-width row will not fit.
        let buffer_width = (limits.max_buffer_size / 256 * 256 / PIXEL_BYTES)
            .min(u64::from(u32::MAX / 256 * 256 / 8)) as u32;
        let dispatch_edge = (u64::from(limits.max_compute_workgroups_per_dimension) * 8)
            .min(u64::from(u32::MAX)) as u32;
        let edge = max_edge
            .min(limits.max_texture_dimension_2d)
            .min(dispatch_edge);
        let tile_width = width.min(edge).min(buffer_width);
        ensure!(tile_width > 0, "Device cannot allocate 2D textures");
        let stride = padded_stride(tile_width);
        let buffer_rows = limits.max_buffer_size / stride;
        ensure!(
            buffer_rows > 0,
            "Device buffer limit cannot hold one padded tile row"
        );
        let band_rows = (BAND_BUDGET / (u64::from(width) * PIXEL_BYTES)).max(1);
        let tile_height = u64::from(height.min(edge)).min(buffer_rows).min(band_rows) as u32;
        Ok(Self {
            width,
            height,
            tile_width,
            tile_height,
        })
    }

    pub fn tiles(&self) -> impl Iterator<Item = Tile> + '_ {
        (0..self.height)
            .step_by(self.tile_height as usize)
            .flat_map(move |y| {
                (0..self.width)
                    .step_by(self.tile_width as usize)
                    .map(move |x| Tile {
                        x,
                        y,
                        width: self.tile_width.min(self.width - x),
                        height: self.tile_height.min(self.height - y),
                    })
            })
    }

    pub fn tile_count(&self) -> u64 {
        u64::from(self.width.div_ceil(self.tile_width))
            * u64::from(self.height.div_ceil(self.tile_height))
    }
}

fn padded_stride(width: u32) -> u64 {
    (u64::from(width) * PIXEL_BYTES).div_ceil(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
        * u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

pub struct TiledExposure {
    pipeline: wgpu::ComputePipeline,
}
impl TiledExposure {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = super::exposure_shader(device, include_str!("tiled.wgsl"));
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("working-space tiled exposure"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline }
    }

    /// Uploads only this tile, submits one pass and starts map_async. Does not poll or wait.
    /// Keep at most one pending tile per export to bound GPU/staging memory.
    /// Returns an error for non-finite EV or values outside [-5, 5], matching EditState.
    pub fn submit(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &ImageBuffer,
        tile: Tile,
        ev: f32,
    ) -> Result<PendingTile> {
        self.submit_adjusted(
            device,
            queue,
            source,
            tile,
            &BasicAdjustments::exposure(ev)?,
        )
    }
    /// Apply the same validated snapshot as the viewport, without waiting for readback.
    pub fn submit_adjusted(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &ImageBuffer,
        tile: Tile,
        adjustments: &BasicAdjustments,
    ) -> Result<PendingTile> {
        ensure!(
            tile.width > 0
                && tile.height > 0
                && tile
                    .x
                    .checked_add(tile.width)
                    .is_some_and(|x| x <= source.width())
                && tile
                    .y
                    .checked_add(tile.height)
                    .is_some_and(|y| y <= source.height()),
            "Tile lies outside the source image"
        );
        let limits = device.limits();
        let stride = padded_stride(tile.width);
        let size = stride
            .checked_mul(u64::from(tile.height))
            .context("Tile buffer size overflow")?;
        ensure!(
            tile.width.max(tile.height) <= limits.max_texture_dimension_2d
                && size <= limits.max_buffer_size
                && stride <= u64::from(u32::MAX),
            "Tile exceeds granted device limits"
        );
        ensure!(
            tile.width.div_ceil(8) <= limits.max_compute_workgroups_per_dimension
                && tile.height.div_ceil(8) <= limits.max_compute_workgroups_per_dimension,
            "Tile exceeds granted compute dispatch limits"
        );
        let extent = wgpu::Extent3d {
            width: tile.width,
            height: tile.height,
            depth_or_array_layers: 1,
        };
        let texture = |label, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage,
                view_formats: &[],
            })
        };
        let input = texture(
            "source tile",
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let output = texture(
            "exposed tile",
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let mut pixels = Vec::with_capacity(tile.width as usize * tile.height as usize);
        for y in tile.y..tile.y + tile.height {
            let start = y as usize * source.width() as usize + tile.x as usize;
            pixels.extend_from_slice(&source.pixels()[start..start + tile.width as usize]);
        }
        queue.write_texture(
            input.as_image_copy(),
            bytemuck::cast_slice(&pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(tile.width * 8),
                rows_per_image: Some(tile.height),
            },
            extent,
        );
        let mut params = Vec::from(bytemuck::cast_slice(adjustments.uniform_rows()));
        params.extend_from_slice(bytemuck::cast_slice(&[
            tile.x,
            tile.y,
            source.width(),
            source.height(),
        ]));
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tile exposure and global region"),
            contents: &params,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let curves = super::curve_buffer(device, "tile tone curves");
        super::write_curves(queue, &curves, adjustments);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &input.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &output.create_view(&Default::default()),
                    ),
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
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(tile.width.div_ceil(8), tile.height.div_ceil(8), 1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(tile.height),
                },
            },
            extent,
        );
        queue.submit([encoder.finish()]);
        let (tx, ready) = mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        Ok(PendingTile {
            tile,
            buffer,
            ready,
            stride: stride as usize,
        })
    }
}

/// Poll the device with PollType::Poll, then call try_read until ready. No blocking calls.
pub struct PendingTile {
    tile: Tile,
    buffer: wgpu::Buffer,
    ready: Receiver<Result<(), wgpu::BufferAsyncError>>,
    stride: usize,
}
impl PendingTile {
    pub fn try_read(&mut self) -> Result<Option<Vec<[f16; 4]>>> {
        match self.ready.try_recv() {
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => {
                bail!("Tile readback callback disconnected or result already consumed")
            }
            Ok(result) => result.context("GPU tile mapping failed")?,
        }
        let mapped = self.buffer.slice(..).get_mapped_range()?;
        let mut pixels = Vec::with_capacity(self.tile.width as usize * self.tile.height as usize);
        for row in mapped.chunks_exact(self.stride) {
            // Explicit little-endian decoding avoids depending on mapped pointer alignment.
            for pixel in row[..self.tile.width as usize * 8].as_chunks::<8>().0 {
                pixels.push(std::array::from_fn(|c| {
                    f16::from_le_bytes([pixel[c * 2], pixel[c * 2 + 1]])
                }));
            }
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(Some(pixels))
    }
}

#[cfg(test)]
#[path = "../tests/unit/tiled_tests.rs"]
mod tests;
