//! Fixed-size, double-buffered asynchronous source-pixel measurements.
use anyhow::{Context, Result, bail, ensure};
use filter_core::{cpu_eval::BasicAdjustments, probe::ProbeSample};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use wgpu::util::DeviceExt;

const BYTES: u64 = 32;
struct Pending {
    generation: u64,
    ready: Receiver<Result<(), wgpu::BufferAsyncError>>,
}
struct Slot {
    buffer: wgpu::Buffer,
    pending: Option<Pending>,
    replace_on_submit: bool,
}
impl Slot {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("color probe readback"),
                size: BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            pending: None,
            replace_on_submit: false,
        }
    }
}

pub struct GpuProbe {
    pipeline: wgpu::ComputePipeline,
    pixel: wgpu::Buffer,
    params: wgpu::Buffer,
    output: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    slots: [Slot; 2],
}
impl GpuProbe {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let limits = device.limits();
        ensure!(
            limits.max_buffer_size >= 96
                && limits.max_uniform_buffer_binding_size >= 96
                && limits.max_storage_buffer_binding_size >= BYTES
                && limits.max_compute_workgroups_per_dimension >= 1
                && limits.max_compute_invocations_per_workgroup >= 1
                && limits.max_compute_workgroup_size_x >= 1,
            "Device limits cannot support the color probe"
        );
        let shader = super::exposure_shader(device, include_str!("probe.wgsl"));
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("source pixel color probe"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let pixel = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("color probe source pixel"),
            contents: bytemuck::cast_slice(&[0.0_f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("color probe adjustments"),
            contents: bytemuck::cast_slice(BasicAdjustments::default().uniform_rows()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("color probe output"),
            size: BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("color probe bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: pixel.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        Ok(Self {
            pipeline,
            pixel,
            params,
            output,
            bindings,
            slots: std::array::from_fn(|_| Slot::new(device)),
        })
    }
    pub fn is_pending(&self) -> bool {
        self.slots.iter().any(|s| s.pending.is_some())
    }
    /// Input is one original straight-alpha linear Rec.2020 pixel, not a
    /// premultiplied viewport texel. Returns false while both slots are occupied.
    /// The caller coalesces pointer/edit changes; this method never polls/waits.
    pub fn try_submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixel: [f32; 4],
        settings: &BasicAdjustments,
        generation: u64,
    ) -> bool {
        let Some(slot) = self.slots.iter_mut().find(|s| s.pending.is_none()) else {
            return false;
        };
        if slot.replace_on_submit {
            *slot = Slot::new(device);
        }
        queue.write_buffer(&self.pixel, 0, bytemuck::cast_slice(&pixel));
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::cast_slice(settings.uniform_rows()),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("color probe dispatch and copy"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &slot.buffer, 0, BYTES);
        queue.submit([encoder.finish()]);
        let (tx, ready) = mpsc::sync_channel(1);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        slot.pending = Some(Pending { generation, ready });
        true
    }
    /// Consume callbacks after an ordinary nonblocking device poll. Only a
    /// successful map is unmapped; errors/disconnections retire that buffer.
    pub fn collect(&mut self) -> Vec<(u64, Result<ProbeSample>)> {
        let mut results = Vec::new();
        for slot in &mut self.slots {
            let Some(pending) = &slot.pending else {
                continue;
            };
            let mapping = match pending.ready.try_recv() {
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("Color probe mapping callback disconnected"))
                }
                Ok(result) => result.context("Color probe mapping failed"),
            };
            let pending = slot.pending.take().unwrap();
            let success = mapping.is_ok();
            let result = mapping.and_then(|()| {
                let bytes = slot.buffer.slice(..).get_mapped_range()?;
                let status = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
                Ok(match status {
                    0 => ProbeSample::from_linear_srgb(std::array::from_fn(|i| {
                        f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                    })),
                    1 => ProbeSample::Transparent,
                    2 => ProbeSample::Invalid,
                    _ => bail!("Invalid color probe status"),
                })
            });
            if success {
                slot.buffer.unmap();
            } else {
                slot.replace_on_submit = true;
            }
            results.push((pending.generation, result));
        }
        results
    }
}

#[cfg(test)]
#[path = "../tests/unit/probe_tests.rs"]
mod tests;
