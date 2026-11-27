//! Bounded, double-buffered asynchronous histogram readback on the caller's device.
use crate::{SourceTexture, readback::Readback};
use anyhow::{Result, ensure};
use filter_core::{
    cpu_eval::BasicAdjustments,
    histogram::{Histogram, WORDS},
};
use wgpu::util::DeviceExt;

const BYTES: u64 = (WORDS * 4) as u64;
pub const MAX_HISTOGRAM_EDGE: u32 = 2048;

/// Readback tag: edit generation and expected source pixel count.
#[derive(Clone, Copy)]
struct Job {
    generation: u64,
    pixels: u64,
}

pub struct GpuHistogram {
    pipeline: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    curves: wgpu::Buffer,
    counters: wgpu::Buffer,
    bindings: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    readback: Readback<Job>,
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
        super::ensure_curve_limits(device)?;
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
        Ok(Self {
            pipeline,
            params,
            curves: super::curve_buffer(device, "histogram tone curves"),
            counters,
            bindings: None,
            readback: Readback::new(device, "Histogram", BYTES),
        })
    }

    /// Release the cached source binding; already submitted jobs remain readable.
    pub fn clear_source(&mut self) {
        self.bindings = None;
    }
    pub fn is_pending(&self) -> bool {
        self.readback.is_pending()
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
        let job = Job {
            generation,
            pixels: u64::from(source.width) * u64::from(source.height),
        };
        Ok(self.readback.submit(device, job, |readback| {
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
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.curves.as_entire_binding(),
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
            super::write_curves(queue, &self.curves, adjustments);
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
            encoder.copy_buffer_to_buffer(&self.counters, 0, readback, 0, BYTES);
            queue.submit([encoder.finish()]);
        }))
    }

    /// Call after the app's ordinary nonblocking device poll. Each returned slot
    /// is unmapped after successful mapping; failed buffers are replaced on the
    /// next submission. Tags let the caller reject superseded results.
    pub fn collect(&mut self) -> Vec<(u64, Result<Histogram>)> {
        self.readback
            .collect(|mapped, job| {
                let words: Vec<u32> = mapped
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| u32::from_le_bytes(*b))
                    .collect();
                let histogram = Histogram::from_words(&words).map_err(anyhow::Error::msg)?;
                ensure!(
                    histogram.total_pixels() == job.pixels,
                    "Histogram pixel total differs from source"
                );
                Ok(histogram)
            })
            .into_iter()
            .map(|(job, result)| (job.generation, result))
            .collect()
    }
}

#[cfg(test)]
#[path = "../tests/unit/histogram_tests.rs"]
mod tests;
