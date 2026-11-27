//! GPU storage for the Finishing `.cube` LUT: an `Rgba16Float` 3D texture read only
//! with `textureLoad` (see `lut.wgsl`) and a 48-byte `LutParams` uniform.
use anyhow::{Result, ensure};
use filter_core::{
    cpu_eval::{BasicAdjustments, LutRows},
    lut::{CubeLut, LUT_MAX_SIZE},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Bytes of the `LutParams` uniform.
pub(crate) const PARAM_BYTES: u64 = std::mem::size_of::<LutRows>() as u64;
const F16_ONE: u16 = 0x3c00;

pub(crate) fn ensure_lut_limits(device: &wgpu::Device) -> Result<()> {
    ensure!(
        device.limits().max_texture_dimension_3d >= LUT_MAX_SIZE as u32,
        "Device cannot allocate a {LUT_MAX_SIZE}^3 LUT texture"
    );
    Ok(())
}
/// Reject a lattice the device cannot hold. Never panics.
pub(crate) fn ensure_lut_size(size: usize, max_dimension: u32) -> Result<()> {
    ensure!(
        size <= max_dimension as usize,
        "A {size}^3 LUT exceeds the device 3D texture limit of {max_dimension}"
    );
    Ok(())
}
pub(crate) fn param_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: PARAM_BYTES,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
/// A 3D `Rgba16Float` texture of `size` cells per edge.
pub(crate) fn lut_texture(device: &wgpu::Device, label: &str, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: size,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
pub(crate) fn lut_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    })
}
/// Expand the RGB f16 lattice to RGBA16F with alpha 1 (at most 2.2 MiB).
fn staging(lut: &CubeLut) -> Vec<u16> {
    let mut out = Vec::with_capacity(lut.texels().len() / 3 * 4);
    for rgb in lut.texels().as_chunks::<3>().0 {
        out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], F16_ONE]);
    }
    out
}
/// Upload the lattice into the `[0, N)^3` corner of `texture`. A queue write has
/// no row alignment requirement and never blocks.
pub(crate) fn write_lattice(queue: &wgpu::Queue, texture: &wgpu::Texture, lut: &CubeLut) {
    let n = lut.size() as u32;
    queue.write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&staging(lut)),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8 * n),
            rows_per_image: Some(n),
        },
        wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
    );
}
pub(crate) fn write_params(queue: &wgpu::Queue, buffer: &wgpu::Buffer, rows: &LutRows) {
    queue.write_buffer(buffer, 0, bytemuck::cast_slice(rows));
}

/// Fixed-capacity (65^3) texture and params for a long-lived consumer. Bind
/// groups hold the view for the consumer's lifetime, so they never go stale: a
/// smaller LUT uploads into the sub-extent and `textureLoad` never leaves it.
pub(crate) struct LutGpu {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    params: wgpu::Buffer,
    uploaded: Mutex<Option<Arc<CubeLut>>>,
    uploads: AtomicU64,
    max_dimension: u32,
}
impl LutGpu {
    pub(crate) fn new(device: &wgpu::Device, label: &str) -> Result<Self> {
        ensure_lut_limits(device)?;
        let texture = lut_texture(device, label, LUT_MAX_SIZE as u32);
        let view = lut_view(&texture);
        let params = param_buffer(device, label);
        Ok(Self {
            texture,
            view,
            params,
            uploaded: Mutex::new(None),
            uploads: AtomicU64::new(0),
            max_dimension: device.limits().max_texture_dimension_3d,
        })
    }
    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
    pub(crate) fn params(&self) -> &wgpu::Buffer {
        &self.params
    }
    /// Number of lattice uploads so far.
    pub(crate) fn uploads(&self) -> u64 {
        self.uploads.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    pub(crate) fn set_max_dimension(&mut self, max: u32) {
        self.max_dimension = max;
    }
    /// Write the 48-byte params always, and the lattice only when it differs
    /// from the last upload (by `Arc`, then by content). Without an active LUT the
    /// stage is flagged off. On error the stage is flagged off too.
    pub(crate) fn set(&self, queue: &wgpu::Queue, adjustments: &BasicAdjustments) -> Result<()> {
        let Some(stage) = adjustments.lut() else {
            write_params(queue, &self.params, &adjustments.lut_rows());
            return Ok(());
        };
        let result = ensure_lut_size(stage.lut.size(), self.max_dimension)
            .map(|()| self.upload(queue, &stage.lut));
        let rows = if result.is_ok() {
            adjustments.lut_rows()
        } else {
            [[0.0; 4]; 3]
        };
        write_params(queue, &self.params, &rows);
        result
    }
    fn upload(&self, queue: &wgpu::Queue, lut: &Arc<CubeLut>) {
        let mut uploaded = self
            .uploaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if uploaded
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, lut) || old.same_lattice(lut))
        {
            return;
        }
        write_lattice(queue, &self.texture, lut);
        // Holding the Arc keeps ptr_eq safe against address reuse.
        *uploaded = Some(Arc::clone(lut));
        self.uploads.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "../tests/unit/lut_tests.rs"]
mod tests;
