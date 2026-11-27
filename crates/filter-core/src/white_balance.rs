//! White-balance eyedropper: the Temperature/Tint that neutralize a sampled color.
//!
//! The pipeline applies exposure (a scalar, which preserves chromaticity) and then
//! [`relative_white_balance`](crate::color::relative_white_balance). The white
//! balance stage therefore sees the chromaticity of the original straight-alpha
//! linear Rec.2020 source pixel, and the result is independent of the current
//! Temperature, Tint and exposure and of every later stage.
//!
//! CAT16 adaptation is diagonal in CAT16 LMS with gains `LMS_target / LMS_D65`,
//! where `LMS_D65` is the LMS of the Rec.2020 white `[1, 1, 1]`. A sample with
//! LMS `p` becomes neutral (proportional to `LMS_D65`) exactly when
//! `LMS_target` is proportional to `LMS_D65² / p`, elementwise. That target is
//! converted to XYZ and its chromaticity inverted through the linear control
//! mapping `x = x₀ + 0.040 T + 0.010 Q`, `y = y₀ + 0.040 T - 0.025 Q`.
use crate::{
    ImageBuffer,
    color::{CAT16, CAT16_INVERSE, D65_XY, TEMPERATURE_XY, TINT_XY, rec2020_to_xyz, transform},
};
use std::fmt;

/// Half-width of the sampled square: a 5 x 5 neighborhood.
const RADIUS: u32 = 2;
/// Samples with luminance Y below this (relative to the Rec.2020 white, Y = 1; about
/// 10 stops under it) are too dark for their chromaticity to define a white.
pub const MIN_LUMINANCE: f32 = 1.0e-3;
/// The supported Temperature and Tint control range.
const LIMIT: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickError {
    /// Every sampled pixel had alpha <= 0.
    Transparent,
    /// Luminance below [`MIN_LUMINANCE`].
    TooDark,
    /// Nonfinite, or with a non-positive cone response (outside the spectral locus).
    Invalid,
}
impl fmt::Display for PickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Transparent => "transparent pixel",
            Self::TooDark => "too dark to define a white",
            Self::Invalid => "invalid color (nonfinite or out of gamut)",
        })
    }
}
impl std::error::Error for PickError {}

/// Temperature and Tint that neutralize a sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickedBalance {
    pub temperature: f32,
    pub tint: f32,
    /// A control had to be limited to [-1, 1]; the sample is then not fully neutral.
    pub clamped: bool,
}

/// Alpha-weighted straight-color mean of the 5 x 5 neighborhood centered on `center`,
/// clipped at the image edges. Pixels with alpha <= 0 or any nonfinite component
/// are skipped. With no weight left the result is `Transparent`, or `Invalid`
/// when nonfinite pixels were skipped.
pub fn neighborhood_average(image: &ImageBuffer, center: [u32; 2]) -> Result<[f32; 3], PickError> {
    let (width, height) = (image.width(), image.height());
    if center[0] >= width || center[1] >= height {
        return Err(PickError::Invalid);
    }
    let range = |c: u32, size: u32| c.saturating_sub(RADIUS)..=(c + RADIUS).min(size - 1);
    let (mut sum, mut weight, mut nonfinite) = ([0.0_f64; 3], 0.0_f64, false);
    for y in range(center[1], height) {
        for x in range(center[0], width) {
            let p = image.pixels()[y as usize * width as usize + x as usize].map(|v| v.to_f32());
            if !p.iter().all(|v| v.is_finite()) {
                nonfinite = true;
            } else if p[3] > 0.0 {
                let alpha = f64::from(p[3]);
                (0..3).for_each(|c| sum[c] += f64::from(p[c]) * alpha);
                weight += alpha;
            }
        }
    }
    if weight > 0.0 {
        Ok(sum.map(|v| (v / weight) as f32))
    } else if nonfinite {
        Err(PickError::Invalid)
    } else {
        Err(PickError::Transparent)
    }
}

/// Temperature and Tint that make linear Rec.2020 `rgb` neutral. See the module docs.
pub fn balance_for_color(rgb: [f32; 3]) -> Result<PickedBalance, PickError> {
    let xyz = rec2020_to_xyz(rgb);
    if !xyz.iter().all(|v| v.is_finite()) {
        return Err(PickError::Invalid);
    }
    if xyz[1] < MIN_LUMINANCE {
        return Err(PickError::TooDark);
    }
    let sample = transform(CAT16, xyz);
    if !sample.iter().all(|v| *v > 0.0) {
        return Err(PickError::Invalid);
    }
    let white = transform(CAT16, rec2020_to_xyz([1.0; 3]));
    let target = transform(
        CAT16_INVERSE,
        std::array::from_fn(|i| white[i] * white[i] / sample[i]),
    );
    let total: f32 = target.iter().sum();
    if !(total.is_finite() && total > 0.0) {
        return Err(PickError::Invalid);
    }
    Ok(controls_for_chromaticity([
        target[0] / total,
        target[1] / total,
    ]))
}

/// Invert the linear control mapping, limiting both controls to the supported range.
fn controls_for_chromaticity([x, y]: [f32; 2]) -> PickedBalance {
    let (dx, dy) = (x - D65_XY[0], y - D65_XY[1]);
    // Eliminate T: dx - dy = (TINT_x - TINT_y) Q since TEMPERATURE_x = TEMPERATURE_y.
    let tint = (dx - dy) / (TINT_XY[0] - TINT_XY[1]);
    let temperature = (dx - TINT_XY[0] * tint) / TEMPERATURE_XY[0];
    let limit = |v: f32| v.clamp(-LIMIT, LIMIT);
    PickedBalance {
        temperature: limit(temperature),
        tint: limit(tint),
        clamped: temperature.abs() > LIMIT || tint.abs() > LIMIT,
    }
}

/// Sample the neighborhood of `center` in the oriented source and compute its balance.
pub fn pick_white_balance(
    image: &ImageBuffer,
    center: [u32; 2],
) -> Result<PickedBalance, PickError> {
    balance_for_color(neighborhood_average(image, center)?)
}

#[cfg(test)]
#[path = "../tests/unit/white_balance_tests.rs"]
mod tests;
