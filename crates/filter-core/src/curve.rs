//! Tone curves (process version 1): shape-preserving monotone cubic splines
//! through each curve's control points, baked to 1024-entry lookup tables.
//!
//! Curves operate per channel on mirrored sRGB-encoded values in sRGB primaries:
//! `working -> linear sRGB -> encode -> master -> red/green/blue -> decode -> working`.
//! Inside [0, 1] a curve is the table with linear interpolation; outside it
//! continues linearly with the spline's end tangent, preserving HDR headroom
//! and negative (out-of-gamut) values.
use crate::color::{
    linear_srgb_to_rec2020, linear_to_srgb, rec2020_to_linear_srgb, srgb_to_linear,
};
use crate::edit::{Curve, Curves};

/// Samples per curve over encoded [0, 1]; sample `i` lies at `i / (LUT_SIZE - 1)`.
pub const LUT_SIZE: usize = 1024;
/// GPU uniform bytes: lower slopes, upper slopes, then `LUT_SIZE` rows, each
/// row holding (master, red, green, blue). Matches `ToneCurves` in working.wgsl.
pub const UNIFORM_BYTES: usize = (2 + LUT_SIZE) * 16;

/// Piecewise cubic Hermite interpolation with PCHIP (Fritsch–Butland) tangents:
/// it passes through every control point and never overshoots between them, so
/// monotone points give a monotone curve.
#[derive(Clone, Debug)]
pub struct Spline {
    x: Vec<f64>,
    y: Vec<f64>,
    tangents: Vec<f64>,
}
impl Spline {
    /// Expects a validated curve: at least two points with increasing x.
    pub fn new(curve: &Curve) -> Self {
        let x: Vec<f64> = curve.0.iter().map(|p| f64::from(p[0])).collect();
        let y: Vec<f64> = curve.0.iter().map(|p| f64::from(p[1])).collect();
        let h: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
        let s: Vec<f64> = y
            .windows(2)
            .zip(&h)
            .map(|(w, h)| (w[1] - w[0]) / h)
            .collect();
        let n = x.len();
        let mut tangents = vec![s[0]; n];
        if n > 2 {
            for k in 1..n - 1 {
                let (h0, h1, s0, s1) = (h[k - 1], h[k], s[k - 1], s[k]);
                // Weighted harmonic mean; flat at local extrema and plateaus.
                tangents[k] = if s0 * s1 > 0.0 {
                    3.0 * (h0 + h1) / ((2.0 * h1 + h0) / s0 + (h1 + 2.0 * h0) / s1)
                } else {
                    0.0
                };
            }
            tangents[0] = end_tangent(h[0], h[1], s[0], s[1]);
            tangents[n - 1] = end_tangent(h[n - 2], h[n - 3], s[n - 2], s[n - 3]);
        }
        Self { x, y, tangents }
    }
    /// The curve at `x`, extended linearly with the end tangents outside [0, 1].
    pub fn eval(&self, x: f64) -> f64 {
        let last = self.x.len() - 1;
        if x <= self.x[0] {
            return self.y[0] + self.tangents[0] * (x - self.x[0]);
        }
        if x >= self.x[last] {
            return self.y[last] + self.tangents[last] * (x - self.x[last]);
        }
        let k = self.x.partition_point(|&v| v <= x) - 1;
        let h = self.x[k + 1] - self.x[k];
        let t = (x - self.x[k]) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.y[k]
            + (t3 - 2.0 * t2 + t) * h * self.tangents[k]
            + (-2.0 * t3 + 3.0 * t2) * self.y[k + 1]
            + (t3 - t2) * h * self.tangents[k + 1]
    }
    /// End tangents: the slopes used outside [0, 1], at x = 0 and x = 1.
    pub fn end_slopes(&self) -> [f64; 2] {
        [self.tangents[0], self.tangents[self.tangents.len() - 1]]
    }
}

/// Three-point end tangent, limited to preserve the shape of the end interval.
fn end_tangent(h0: f64, h1: f64, s0: f64, s1: f64) -> f64 {
    let d = ((2.0 * h0 + h1) * s0 - h0 * s1) / (h0 + h1);
    if d.signum() != s0.signum() || s0 == 0.0 {
        0.0
    } else if s0.signum() != s1.signum() && d.abs() > 3.0 * s0.abs() {
        3.0 * s0
    } else {
        d
    }
}

/// A curve whose control points all lie on y = x is exactly the identity.
pub fn is_identity(curve: &Curve) -> bool {
    curve.0.iter().all(|p| p[0] == p[1])
}

/// Baked master/red/green/blue tables shared by CPU evaluation and GPU uniforms.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveLut {
    /// Row 0: slopes below 0; row 1: slopes above 1; then `LUT_SIZE` samples.
    rows: Vec<[f32; 4]>,
}
impl CurveLut {
    /// Returns `None` when every curve is the identity, so renderers can skip
    /// the curve stage and its transfer round trip. Expects validated curves.
    pub fn new(curves: &Curves) -> Option<Self> {
        let channels = [&curves.master, &curves.red, &curves.green, &curves.blue];
        if channels.iter().all(|c| is_identity(c)) {
            return None;
        }
        let splines = channels.map(Spline::new);
        let mut rows = Vec::with_capacity(2 + LUT_SIZE);
        rows.push(splines.each_ref().map(|s| s.end_slopes()[0] as f32));
        rows.push(splines.each_ref().map(|s| s.end_slopes()[1] as f32));
        let step = 1.0 / (LUT_SIZE - 1) as f64;
        rows.extend(
            (0..LUT_SIZE).map(|i| splines.each_ref().map(|s| s.eval(i as f64 * step) as f32)),
        );
        Some(Self { rows })
    }
    /// `ToneCurves` uniform rows (`UNIFORM_BYTES` when cast to bytes).
    pub fn uniform_rows(&self) -> &[[f32; 4]] {
        &self.rows
    }
    /// Evaluate channel 0 (master), 1 (red), 2 (green) or 3 (blue) at an
    /// encoded value, exactly as the WGSL `curve_lookup` does.
    pub fn lookup(&self, channel: usize, encoded: f32) -> f32 {
        let samples = &self.rows[2..];
        if encoded < 0.0 {
            return samples[0][channel] + self.rows[0][channel] * encoded;
        }
        if encoded > 1.0 {
            return samples[LUT_SIZE - 1][channel] + self.rows[1][channel] * (encoded - 1.0);
        }
        let t = encoded * (LUT_SIZE - 1) as f32;
        // NaN converts to 0 here; the fraction stays NaN and propagates.
        let i = (t as usize).min(LUT_SIZE - 2);
        let f = t - i as f32;
        // WGSL mix(a, b, f) is defined as a * (1 - f) + b * f.
        samples[i][channel] * (1.0 - f) + samples[i + 1][channel] * f
    }
    /// Apply master then per-channel curves to straight linear Rec.2020 RGB.
    pub fn apply(&self, working: [f32; 3]) -> [f32; 3] {
        let linear = rec2020_to_linear_srgb(working);
        linear_srgb_to_rec2020(std::array::from_fn(|c| {
            let master = self.lookup(0, linear_to_srgb(linear[c]));
            srgb_to_linear(self.lookup(c + 1, master))
        }))
    }
}

#[cfg(test)]
#[path = "../tests/unit/curve_tests.rs"]
mod tests;
