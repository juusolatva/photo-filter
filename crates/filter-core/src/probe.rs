//! Single source-pixel measurements in clipped output sRGB, before compositing.
use crate::{color, cpu_eval::BasicAdjustments};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProbeSample {
    Color {
        /// Gamma-encoded sRGB in [0, 1], before quantization or dithering.
        rgb: [f32; 3],
        /// Oklab of the same clipped output color (L in [0, 1]).
        oklab: [f32; 3],
        alpha: f32,
    },
    Transparent,
    Invalid,
}
impl ProbeSample {
    pub fn from_linear_srgb(rgba: [f32; 4]) -> Self {
        if rgba[3] <= 0.0 {
            return Self::Transparent;
        }
        if !rgba.iter().all(|v| v.is_finite()) {
            return Self::Invalid;
        }
        let linear = [rgba[0], rgba[1], rgba[2]].map(|v| v.clamp(0.0, 1.0));
        Self::Color {
            rgb: linear.map(color::linear_to_srgb),
            oklab: color::rec2020_to_oklab(color::linear_srgb_to_rec2020(linear)),
            alpha: rgba[3].clamp(0.0, 1.0),
        }
    }

    /// CPU reference. The GPU uses canonical WGSL on the original straight
    /// half-float source pixel, without proxy reduction or viewport sampling.
    pub fn reference(pixel: [f32; 4], settings: &BasicAdjustments) -> Self {
        if pixel[3] <= 0.0 {
            return Self::Transparent;
        }
        if !pixel.iter().all(|v| v.is_finite()) {
            return Self::Invalid;
        }
        let [r, g, b, _] = settings.pixel(pixel);
        let rgb = color::rec2020_to_linear_srgb([r, g, b]);
        Self::from_linear_srgb([rgb[0], rgb[1], rgb[2], pixel[3]])
    }

    /// Nearest 8-bit sRGB values for RGB/hex presentation. Alpha is separate.
    pub fn rgb8(&self) -> Option<[u8; 3]> {
        match self {
            Self::Color { rgb, .. } => Some(rgb.map(|v| (v * 255.0).round() as u8)),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/probe_tests.rs"]
mod tests;
