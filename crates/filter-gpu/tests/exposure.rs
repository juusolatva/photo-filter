//! Explicit GPU test: cargo test -p filter-gpu --test exposure -- --ignored
use filter_core::ImageBuffer;
use filter_gpu::ExposureRenderer;
use half::f16;

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn exposure_shader_and_padded_readback() {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..Default::default()
            })
            .await
            .expect("software adapter required");
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let renderer = ExposureRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let pixel = [
            f16::from_f32(0.125),
            f16::from_f32(0.125),
            f16::from_f32(0.125),
            f16::ONE,
        ];
        let image = ImageBuffer::new(3, 2, vec![pixel; 6]).unwrap();
        let source = renderer.upload(&device, &queue, &image);
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test target"),
            size: wgpu::Extent3d {
                width: 3,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // Width 3 deliberately requires 244 padding bytes after every 12-byte row.
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("padded readback"),
            size: 512,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for ev in [0.0, 1.0] {
            let frame = renderer.frame(&device, &source, ev);
            let mut encoder = device.create_command_encoder(&Default::default());
            let view = output.create_view(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                renderer.paint(&mut pass, &frame);
            }
            encoder.copy_texture_to_buffer(
                output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(2),
                    },
                },
                output.size(),
            );
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            staging
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    tx.send(result).unwrap();
                });
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(30)),
                })
                .unwrap();
            rx.recv().unwrap().unwrap();
            let data = staging.slice(..).get_mapped_range().unwrap();
            let expected =
                (filter_core::color::linear_to_srgb(0.125 * ev.exp2()) * 255.0).round() as i32;
            for row in 0..2 {
                for x in 0..3 {
                    let p = &data[row * 256 + x * 4..row * 256 + x * 4 + 4];
                    for c in &p[..3] {
                        assert!(
                            (*c as i32 - expected).abs() <= 1,
                            "ev {ev}: {p:?}, expected {expected}"
                        );
                    }
                    assert_eq!(p[3], 255);
                }
            }
            drop(data);
            staging.unmap();
        }
    });
}
