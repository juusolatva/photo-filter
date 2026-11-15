//! GUI-independent basic pointwise GPU renderer. Uses the caller's device and queue.
use filter_core::{ImageBuffer, cpu_eval::BasicAdjustments};
use wgpu::util::DeviceExt;

mod tiled;
pub use tiled::{PendingTile, Tile, TilePlan, TiledExposure};

fn exposure_shader(device: &wgpu::Device, body: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("exposure shader"),
        source: wgpu::ShaderSource::Wgsl(
            format!("{}\n{body}", include_str!("working.wgsl")).into(),
        ),
    })
}

pub struct ExposureRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    target_is_srgb: bool,
}
pub struct SourceTexture {
    view: wgpu::TextureView,
}
pub struct ExposureFrame {
    pub bind_group: wgpu::BindGroup,
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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("proxy sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            target_is_srgb: format.is_srgb(),
        }
    }
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageBuffer,
    ) -> SourceTexture {
        assert!(image.width().max(image.height()) <= device.limits().max_texture_dimension_2d);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("linear Rec.2020 proxy"),
            size: wgpu::Extent3d {
                width: image.width(),
                height: image.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Premultiply only the sampled texture to make interpolation alpha-correct.
        let data: Vec<[half::f16; 4]> = image
            .pixels()
            .iter()
            .map(|p| {
                let a = p[3].to_f32();
                [
                    half::f16::from_f32(p[0].to_f32() * a),
                    half::f16::from_f32(p[1].to_f32() * a),
                    half::f16::from_f32(p[2].to_f32() * a),
                    p[3],
                ]
            })
            .collect();
        queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width() * 8),
                rows_per_image: Some(image.height()),
            },
            texture.size(),
        );
        SourceTexture {
            view: texture.create_view(&Default::default()),
        }
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
        let mut params = adjustments.uniform_rows().to_vec();
        params.push([if self.target_is_srgb { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]);
        params.push(uv);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("exposure parameters"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
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
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        ExposureFrame { bind_group }
    }
    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>, frame: &ExposureFrame) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &frame.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
