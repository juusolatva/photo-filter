//! GUI-independent basic pointwise GPU renderer. Uses the caller's device and queue.
use filter_core::{ImageBuffer, cpu_eval::BasicAdjustments, curve};
use wgpu::util::DeviceExt;

mod histogram;
mod probe;
mod readback;
pub use probe::GpuProbe;
mod tiled;
pub use histogram::GpuHistogram;
pub use tiled::{PendingTile, Tile, TilePlan, TiledExposure};

/// A `ToneCurves` uniform buffer. Its contents are only read while the
/// adjustment parameters flag active curves, so it may start zeroed.
fn curve_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: curve::UNIFORM_BYTES as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
/// Write active curve tables before a submission that uses `adjustments`.
fn write_curves(queue: &wgpu::Queue, buffer: &wgpu::Buffer, adjustments: &BasicAdjustments) {
    if let Some(curves) = adjustments.curves() {
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(curves.uniform_rows()));
    }
}
/// Devices must bind the 16 KiB curve tables as a uniform.
fn ensure_curve_limits(device: &wgpu::Device) -> anyhow::Result<()> {
    anyhow::ensure!(
        device.limits().max_uniform_buffer_binding_size >= curve::UNIFORM_BYTES as u64,
        "Device uniform limits cannot hold tone curve tables"
    );
    Ok(())
}

fn exposure_shader(device: &wgpu::Device, body: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("exposure shader"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}\n{}\n{body}",
                include_str!("working.wgsl"),
                include_str!("output.wgsl")
            )
            .into(),
        ),
    })
}

pub struct ExposureRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    target_is_srgb: bool,
    viewport: wgpu::Buffer,
    curves: wgpu::Buffer,
}
pub struct SourceTexture {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}
#[derive(Clone)]
pub struct ExposureFrame {
    pub bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
}

/// Reusable staging memory for a viewport's many small uniform updates.
/// Flush after updating all frames and before submitting their paint pass.
pub struct ExposureUpdates {
    device: wgpu::Device,
    queue: wgpu::Queue,
    staging: wgpu::util::StagingBelt,
    encoder: Option<wgpu::CommandEncoder>,
}
impl ExposureUpdates {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            staging: wgpu::util::StagingBelt::new(device.clone(), 16 * 1024),
            encoder: None,
        }
    }
    fn write(&mut self, target: &wgpu::Buffer, params: &[[f32; 4]; 9]) {
        let encoder = self.encoder.get_or_insert_with(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewport uniform updates"),
                })
        });
        let bytes = bytemuck::cast_slice(params);
        self.staging
            .write_buffer(
                encoder,
                target,
                0,
                wgpu::BufferSize::new(bytes.len() as u64).unwrap(),
            )
            .copy_from_slice(bytes);
    }
    /// Submit all staged updates together, then recycle staging chunks through
    /// asynchronous mapping. The caller's regular nonblocking poll drives reuse.
    pub fn flush(&mut self) {
        if let Some(encoder) = self.encoder.take() {
            self.staging.finish();
            self.queue.submit([encoder.finish()]);
            self.staging.recall();
        }
    }
}
impl ExposureRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("exposure bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(curve::UNIFORM_BYTES as u64),
                    },
                    count: None,
                },
            ],
        });
        let shader = exposure_shader(device, include_str!("exposure.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("exposure layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("exposure viewport"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Physical bounds, then the before/after split (0: edited everywhere).
        let viewport = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("viewport physical bounds and compare split"),
            contents: bytemuck::cast_slice(&[0.0_f32, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            pipeline,
            layout,
            target_is_srgb: format.is_srgb(),
            viewport,
            curves: curve_buffer(device, "viewport tone curves"),
        }
    }
    /// Set physical viewport origin/size before painting. All frames from this
    /// renderer share these bounds; use one renderer per simultaneous viewport.
    pub fn set_viewport(&self, queue: &wgpu::Queue, bounds: [f32; 4]) {
        assert!(bounds.iter().all(|v| v.is_finite()) && bounds[2] > 0.0 && bounds[3] > 0.0);
        queue.write_buffer(&self.viewport, 0, bytemuck::cast_slice(&bounds));
    }
    /// Before/after comparison for all frames of this renderer: fragments whose
    /// physical framebuffer x lies left of `x` show the unedited source, without
    /// basic adjustments. `0.0` (the default) shows the edit everywhere; a value
    /// beyond the target's right edge, such as `f32::MAX`, shows only the original.
    ///
    /// # Panics
    /// Panics if `x` is not finite.
    pub fn set_before_until(&self, queue: &wgpu::Queue, x: f32) {
        assert!(x.is_finite(), "compare split must be finite");
        queue.write_buffer(
            &self.viewport,
            16,
            bytemuck::cast_slice(&[x, 0.0, 0.0, 0.0]),
        );
    }
    /// Tone curves for all frames of this renderer, like [`Self::set_viewport`].
    /// Call before painting frames whose adjustments have active curves; frame
    /// creation and [`Self::update_frame`] do not upload them. Identity curves
    /// need no call: frames then skip the curve stage.
    pub fn set_curves(&self, queue: &wgpu::Queue, adjustments: &BasicAdjustments) {
        write_curves(queue, &self.curves, adjustments);
    }
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageBuffer,
    ) -> SourceTexture {
        self.upload_region(
            device,
            queue,
            image,
            Tile {
                x: 0,
                y: 0,
                width: image.width(),
                height: image.height(),
            },
        )
        .expect("valid viewport texture")
    }
    /// Upload a bounded source region, premultiplying before bilinear sampling.
    /// Callers can include a one-pixel border for seamless adjacent tiles.
    pub fn upload_region(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageBuffer,
        region: Tile,
    ) -> anyhow::Result<SourceTexture> {
        anyhow::ensure!(
            region.width > 0
                && region.height > 0
                && region
                    .x
                    .checked_add(region.width)
                    .is_some_and(|x| x <= image.width())
                && region
                    .y
                    .checked_add(region.height)
                    .is_some_and(|y| y <= image.height()),
            "Viewport region lies outside the source image"
        );
        anyhow::ensure!(
            region.width.max(region.height) <= device.limits().max_texture_dimension_2d,
            "Viewport region exceeds granted texture limits"
        );
        let mut data = Vec::new();
        data.try_reserve_exact(region.width as usize * region.height as usize)?;
        for y in region.y..region.y + region.height {
            let start = y as usize * image.width() as usize + region.x as usize;
            data.extend(
                image.pixels()[start..start + region.width as usize]
                    .iter()
                    .map(|p| {
                        let a = p[3].to_f32();
                        [
                            half::f16::from_f32(p[0].to_f32() * a),
                            half::f16::from_f32(p[1].to_f32() * a),
                            half::f16::from_f32(p[2].to_f32() * a),
                            p[3],
                        ]
                    }),
            );
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("linear Rec.2020 viewport source"),
            size: wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(region.width * 8),
                rows_per_image: Some(region.height),
            },
            texture.size(),
        );
        Ok(SourceTexture {
            view: texture.create_view(&Default::default()),
            width: region.width,
            height: region.height,
        })
    }
    /// Exposure-only viewport, with the same [-5, 5] EV contract as EditState.
    ///
    /// # Panics
    /// Panics if EV is non-finite or outside [-5, 5]. Use a validated
    /// BasicAdjustments snapshot with frame_region_adjusted for fallible inputs.
    pub fn frame(&self, device: &wgpu::Device, source: &SourceTexture, ev: f32) -> ExposureFrame {
        self.frame_region(device, source, ev, [0.0, 0.0, 1.0, 1.0])
    }
    /// UV origin and extent for the visible part of a panned/zoomed image.
    ///
    /// # Panics
    /// Panics if EV is non-finite or outside [-5, 5].
    pub fn frame_region(
        &self,
        device: &wgpu::Device,
        source: &SourceTexture,
        ev: f32,
        uv: [f32; 4],
    ) -> ExposureFrame {
        self.frame_region_adjusted(
            device,
            source,
            &BasicAdjustments::exposure(ev).expect("valid exposure"),
            uv,
        )
    }
    /// Render a validated basic-adjustment snapshot over a UV region.
    pub fn frame_region_adjusted(
        &self,
        device: &wgpu::Device,
        source: &SourceTexture,
        adjustments: &BasicAdjustments,
        uv: [f32; 4],
    ) -> ExposureFrame {
        self.frame_region_placed(device, source, adjustments, uv, [0.0, 0.0, 1.0, 1.0])
    }
    /// Draw into a normalized subrectangle of the viewport (top-left origin).
    /// UVs refer to the uploaded texture, including any filtering border.
    pub fn frame_region_placed(
        &self,
        device: &wgpu::Device,
        source: &SourceTexture,
        adjustments: &BasicAdjustments,
        uv: [f32; 4],
        rect: [f32; 4],
    ) -> ExposureFrame {
        let params = self.frame_params(adjustments, uv, rect);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("exposure parameters"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("exposure frame"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.viewport.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.curves.as_entire_binding(),
                },
            ],
        });
        ExposureFrame {
            bind_group,
            uniform,
        }
    }
    /// Stage an update to a reusable frame; flush updates before painting it.
    /// Clones share GPU resources and receive these updates too. Use separate frames
    /// for separate draws in a submission, rather than updating one frame per draw.
    pub fn update_frame(
        &self,
        updates: &mut ExposureUpdates,
        frame: &ExposureFrame,
        adjustments: &BasicAdjustments,
        uv: [f32; 4],
        rect: [f32; 4],
    ) {
        updates.write(&frame.uniform, &self.frame_params(adjustments, uv, rect));
    }
    fn frame_params(
        &self,
        adjustments: &BasicAdjustments,
        uv: [f32; 4],
        rect: [f32; 4],
    ) -> [[f32; 4]; 9] {
        let mut params = [[0.0; 4]; 9];
        params[..6].copy_from_slice(adjustments.uniform_rows());
        params[6][0] = if self.target_is_srgb { 1.0 } else { 0.0 };
        params[7] = uv;
        params[8] = rect;
        params
    }
    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>, frame: &ExposureFrame) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &frame.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}
