use super::*;
use filter_core::{edit::EditState, lut::LutSlot};

fn adjustments(n: usize) -> BasicAdjustments {
    let texels = (0..3 * n * n * n).map(|_| 0x3800).collect();
    let lut = CubeLut::new(n, "flat", [0.0; 3], [1.0; 3], texels).unwrap();
    let mut edit = EditState::default();
    edit.finishing.lut = LutSlot::Cube(Arc::new(lut));
    BasicAdjustments::new(&edit).unwrap()
}

#[test]
fn lattices_beyond_the_device_limit_are_errors() {
    assert!(ensure_lut_size(65, 65).is_ok());
    assert!(ensure_lut_size(2, 256).is_ok());
    let error = ensure_lut_size(65, 64).unwrap_err().to_string();
    assert!(error.contains("65") && error.contains("64"), "{error}");
}
#[test]
fn staging_adds_an_opaque_alpha_lane_in_lattice_order() {
    let lut = adjustments(2).lut().unwrap().lut.clone();
    let rgba = staging(&lut);
    assert_eq!(rgba.len(), 8 * 4);
    assert!(
        rgba.as_chunks::<4>()
            .0
            .iter()
            .all(|t| *t == [0x3800, 0x3800, 0x3800, F16_ONE])
    );
}
#[test]
#[ignore = "requires a Vulkan/DX12/Metal software adapter"]
fn a_lattice_above_a_faked_texture_limit_errors_without_uploading() {
    let (device, queue) = pollster::block_on(async {
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
        adapter.request_device(&Default::default()).await.unwrap()
    });
    let mut gpu = LutGpu::new(&device, "test LUT").unwrap();
    gpu.set_max_dimension(16);
    assert!(gpu.set(&queue, &adjustments(17)).is_err());
    assert_eq!(gpu.uploads(), 0);
    assert!(gpu.set(&queue, &adjustments(16)).is_ok());
    assert_eq!(gpu.uploads(), 1);
}
