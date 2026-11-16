//! Explicit software-GPU checks: cargo test -p filter-gpu --test tiled -- --ignored
use filter_core::ImageBuffer;
use filter_gpu::{TilePlan, TiledExposure};
use half::f16;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn tiles_match_untiled_and_cpu_with_straight_alpha_and_unaligned_edges() {
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
        let (w, h) = (67, 35);
        let image = ImageBuffer::new(
            w,
            h,
            (0..w * h)
                .map(|i| {
                    [
                        ((i % w) as f32 - 20.0) / 32.0,
                        (i / w) as f32 / 16.0,
                        0.125,
                        [0.0, 0.25, 0.5, 1.0][i as usize % 4],
                    ]
                    .map(f16::from_f32)
                })
                .collect(),
        )
        .unwrap();
        let renderer = TiledExposure::new(&device);
        for ev in [-2.0_f32, 0.0, 1.0] {
            let mut reference = None;
            for edge in [w, 17] {
                let plan = TilePlan::new(w, h, &device.limits(), edge).unwrap();
                let mut output = vec![[f16::ZERO; 4]; (w * h) as usize];
                for tile in plan.tiles() {
                    let mut pending = renderer.submit(&device, &queue, &image, tile, ev).unwrap();
                    let started = Instant::now();
                    let pixels = loop {
                        device.poll(wgpu::PollType::Poll).unwrap();
                        if let Some(pixels) = pending.try_read().unwrap() {
                            break pixels;
                        }
                        assert!(
                            started.elapsed() < Duration::from_secs(30),
                            "readback timed out"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    };
                    assert_eq!(pixels.len(), (tile.width * tile.height) as usize);
                    for (row, pixels) in pixels.chunks_exact(tile.width as usize).enumerate() {
                        let start = (tile.y as usize + row) * w as usize + tile.x as usize;
                        output[start..start + pixels.len()].copy_from_slice(pixels);
                    }
                }
                for (original, actual) in image.pixels().iter().zip(&output) {
                    let mut expected = original.map(|v| f16::from_f32(v.to_f32() * ev.exp2()));
                    expected[3] = original[3];
                    assert_eq!(*actual, expected, "EV {ev}");
                }
                if let Some(reference) = &reference {
                    assert_eq!(&output, reference);
                } else {
                    reference = Some(output);
                }
            }
        }
    });
}
