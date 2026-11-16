//! Bounded, double-buffered asynchronous histogram readback on the caller's device.
use crate::SourceTexture;
use anyhow::{Context, Result, ensure};
use filter_core::{
    cpu_eval::BasicAdjustments,
    histogram::{Histogram, WORDS},
};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use wgpu::util::DeviceExt;

const BYTES: u64 = (WORDS * 4) as u64;
pub const MAX_HISTOGRAM_EDGE: u32 = 2048;

struct Pending {
    generation: u64,
    pixels: u64,
    ready: Receiver<Result<(), wgpu::BufferAsyncError>>,
}
struct Slot {
    buffer: wgpu::Buffer,
    pending: Option<Pending>,
}

pub struct GpuHistogram {
    pipeline: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    counters: wgpu::Buffer,
    bindings: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    slots: [Slot; 2],
}
impl GpuHistogram {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let limits = device.limits();
        ensure!(
            limits.max_buffer_size >= BYTES
                && limits.max_storage_buffer_binding_size >= BYTES
                && limits.max_compute_workgroup_storage_size >= BYTES as u32
                && limits.max_compute_invocations_per_workgroup >= 256
                && limits.max_compute_workgroup_size_x >= 16
                && limits.max_compute_workgroup_size_y >= 16,
            "Device limits cannot support the preview histogram"
        );
        let shader = super::exposure_shader(device, include_str!("histogram.wgsl"));
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("edited preview histogram"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("histogram adjustments"),
            contents: bytemuck::cast_slice(BasicAdjustments::default().uniform_rows()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let counters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram counters"),
            size: BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let slots = std::array::from_fn(|_| Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("histogram readback"),
                size: BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            pending: None,
        });
        Ok(Self {
            pipeline,
            params,
            counters,
            bindings: None,
            slots,
        })
    }

    /// Release the cached source binding; already submitted jobs remain readable.
    pub fn clear_source(&mut self) {
        self.bindings = None;
    }
    pub fn is_pending(&self) -> bool {
        self.slots.iter().any(|s| s.pending.is_some())
    }

    /// Returns false when both readback slots are busy, leaving the caller free
    /// to coalesce to its latest desired generation. Never polls or waits.
    pub fn try_submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &SourceTexture,
        adjustments: &BasicAdjustments,
        generation: u64,
    ) -> Result<bool> {
        ensure!(
            source.width.max(source.height) <= MAX_HISTOGRAM_EDGE,
            "Histogram source must be a preview of at most 2048px per edge"
        );
        let (groups_x, groups_y) = (source.width.div_ceil(16), source.height.div_ceil(16));
        ensure!(
            groups_x.max(groups_y) <= device.limits().max_compute_workgroups_per_dimension,
            "Histogram exceeds granted compute dispatch limits"
        );
        let Some(slot) = self.slots.iter_mut().find(|s| s.pending.is_none()) else {
            return Ok(false);
        };
        if self
            .bindings
            .as_ref()
            .is_none_or(|(view, _)| *view != source.view)
        {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("histogram source and counters"),
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.counters.as_entire_binding(),
                    },
                ],
            });
            self.bindings = Some((source.view.clone(), group));
        }
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::cast_slice(adjustments.uniform_rows()),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("histogram dispatch and copy"),
        });
        encoder.clear_buffer(&self.counters, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings.as_ref().unwrap().1, &[]);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        encoder.copy_buffer_to_buffer(&self.counters, 0, &slot.buffer, 0, BYTES);
        queue.submit([encoder.finish()]);
        let (tx, ready) = mpsc::sync_channel(1);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        slot.pending = Some(Pending {
            generation,
            pixels: u64::from(source.width) * u64::from(source.height),
            ready,
        });
        Ok(true)
    }

    /// Call after the app's ordinary nonblocking device poll. Each returned slot
    /// is unmapped before reuse. Tags let the caller reject superseded results.
    pub fn collect(&mut self) -> Vec<(u64, Result<Histogram>)> {
        let mut results = Vec::new();
        for slot in &mut self.slots {
            let Some(pending) = &slot.pending else {
                continue;
            };
            let mapping = match pending.ready.try_recv() {
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("Histogram mapping callback disconnected"))
                }
                Ok(result) => result.context("Histogram mapping failed"),
            };
            let pending = slot.pending.take().unwrap();
            let result = mapping.and_then(|()| {
                let mapped = slot.buffer.slice(..).get_mapped_range()?;
                let words: Vec<u32> = mapped
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| u32::from_le_bytes(*b))
                    .collect();
                let histogram = Histogram::from_words(&words).map_err(anyhow::Error::msg)?;
                ensure!(
                    histogram.total_pixels() == pending.pixels,
                    "Histogram pixel total differs from source"
                );
                Ok(histogram)
            });
            slot.buffer.unmap();
            results.push((pending.generation, result));
        }
        results
    }
}
