//! Fixed-size, double-buffered asynchronous source-pixel measurements.
use crate::readback::Readback;
use anyhow::{Result, bail, ensure};
use filter_core::{
    cpu_eval::{BasicAdjustments, BasicRows},
    probe::ProbeSample,
};
use wgpu::util::DeviceExt;

/// The `BasicParams` uniform: 16 vec4 rows.
const PARAM_BYTES: u64 = std::mem::size_of::<BasicRows>() as u64;
/// Linear sRGB RGBA as four f32 values, then a u32 status, padded to 32 bytes.
const BYTES: u64 = 32;

pub struct GpuProbe {
    pipeline: wgpu::ComputePipeline,
    pixel: wgpu::Buffer,
    params: wgpu::Buffer,
    curves: wgpu::Buffer,
    lut: crate::lut::LutGpu,
    output: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    readback: Readback<u64>,
}
impl GpuProbe {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let limits = device.limits();
        ensure!(
            limits.max_buffer_size >= PARAM_BYTES
                && limits.max_uniform_buffer_binding_size >= PARAM_BYTES
                && limits.max_storage_buffer_binding_size >= BYTES
                && limits.max_compute_workgroups_per_dimension >= 1
                && limits.max_compute_invocations_per_workgroup >= 1
                && limits.max_compute_workgroup_size_x >= 1,
            "Device limits cannot support the color probe"
        );
        super::ensure_curve_limits(device)?;
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
        let curves = super::curve_buffer(device, "color probe tone curves");
        let lut = crate::lut::LutGpu::new(device, "color probe LUT")?;
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: curves.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(lut.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: lut.params().as_entire_binding(),
                },
            ],
        });
        Ok(Self {
            pipeline,
            pixel,
            params,
            curves,
            lut,
            output,
            bindings,
            readback: Readback::new(device, "Color probe", BYTES),
        })
    }
    /// Lattice uploads performed so far (diagnostics and tests).
    pub fn lut_uploads(&self) -> u64 {
        self.lut.uploads()
    }
    pub fn is_pending(&self) -> bool {
        self.readback.is_pending()
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
        // Only fails for a lattice beyond the device limit, which `new` rules out.
        if self.lut.set(queue, settings).is_err() {
            return false;
        }
        self.readback.submit(device, generation, |readback| {
            queue.write_buffer(&self.pixel, 0, bytemuck::cast_slice(&pixel));
            queue.write_buffer(
                &self.params,
                0,
                bytemuck::cast_slice(settings.uniform_rows()),
            );
            super::write_curves(queue, &self.curves, settings);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("color probe dispatch and copy"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bindings, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&self.output, 0, readback, 0, BYTES);
            queue.submit([encoder.finish()]);
        })
    }
    /// Consume callbacks after an ordinary nonblocking device poll. Only a
    /// successful map is unmapped; errors/disconnections retire that buffer.
    pub fn collect(&mut self) -> Vec<(u64, Result<ProbeSample>)> {
        self.readback.collect(|bytes, _| {
            // The readback ring maps exactly BYTES, so these words exist.
            let (words, _) = bytes.as_chunks::<4>();
            let float = |i: usize| f32::from_le_bytes(words[i]);
            Ok(match u32::from_le_bytes(words[4]) {
                0 => ProbeSample::from_linear_srgb(std::array::from_fn(float)),
                1 => ProbeSample::Transparent,
                2 => ProbeSample::Invalid,
                _ => bail!("Invalid color probe status"),
            })
        })
    }
}

#[cfg(test)]
#[path = "../tests/unit/probe_tests.rs"]
mod tests;
