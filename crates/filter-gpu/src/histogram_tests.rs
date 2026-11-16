use super::*;
use crate::ExposureRenderer;
use filter_core::ImageBuffer;
use half::f16;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn failed_mapping_and_disconnected_callbacks_allow_retry_without_validation_errors() {
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
        let renderer = ExposureRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let source = renderer.upload(
            &device,
            &queue,
            &ImageBuffer::new(1, 1, vec![[f16::ONE; 4]]).unwrap(),
        );
        let settings = BasicAdjustments::default();
        for disconnected in [false, true] {
            let mut gpu = GpuHistogram::new(&device).unwrap();
            let old_buffer = gpu.slots[0].buffer.clone();
            let (tx, ready) = mpsc::sync_channel(1);
            // Model failed map_async leaving the buffer unmapped, without
            // depending on real memory exhaustion or losing the test device.
            if !disconnected {
                tx.send(Err(wgpu::BufferAsyncError)).unwrap();
            }
            drop(tx);
            gpu.slots[0].pending = Some(Pending {
                generation: 10,
                pixels: 1,
                ready,
            });
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let failed = gpu.collect();
            let validation = scope.pop().await;
            assert!(
                validation.is_none(),
                "failed readback must not unmap an idle buffer: {validation:?}"
            );
            assert_eq!(failed.len(), 1);
            assert_eq!(failed[0].0, 10);
            assert!(failed[0].1.is_err());
            assert!(!gpu.is_pending());

            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            assert!(
                gpu.try_submit(&device, &queue, &source, &settings, 11)
                    .unwrap()
            );
            assert_ne!(
                old_buffer, gpu.slots[0].buffer,
                "retry must replace the failed readback buffer"
            );
            let deadline = Instant::now() + Duration::from_secs(30);
            let completed = loop {
                device.poll(wgpu::PollType::Poll).unwrap();
                let completed = gpu.collect();
                if !completed.is_empty() {
                    break completed;
                }
                assert!(Instant::now() < deadline, "histogram retry timed out");
                std::thread::sleep(Duration::from_millis(1));
            };
            let validation = scope.pop().await;
            assert!(
                validation.is_none(),
                "retry validation error: {validation:?}"
            );
            assert_eq!(completed.len(), 1);
            assert_eq!(completed[0].0, 11);
            assert_eq!(completed[0].1.as_ref().unwrap().sampled, 1);
            assert!(!gpu.is_pending());
        }
    });
}
