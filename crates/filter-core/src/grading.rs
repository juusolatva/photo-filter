//! CPU reference for the OkLCh HSL mixer, the three-wheel color grading and the
//! channel-mixer fold, mirroring
//! `hsl_stage` and `grade_stage` in `working.wgsl`.
//!
//! The shader uses no trigonometry: band weights come from a rational pseudo-angle
//! and rotations from a Cayley half-angle. All trig and tan work happens here, once
//! per snapshot, when uniforms are built, so CPU and GPU read identical f32 rows.
use crate::color::{IDENTITY, Matrix3, REC2020_LUMA, SRGB_TO_REC2020};
use crate::edit::{GradingWheel, HslBand};
use std::sync::OnceLock;

/// OkLCh hue (degrees) of the band reference colors, measured from the repo's
/// sRGB to Oklab conversion: #FF0000, #FF8000, #FFFF00, #00FF00, #00FFFF,
/// #0000FF, #8000FF and #FF00FF. Tests and the mixer UI swatches/tracks use it; the
/// shader uses the derived constants below.
pub const BAND_CENTER_DEGREES: [f32; 8] = [
    29.234, 52.980, 109.769, 142.495, 194.769, 264.052, 293.940, 328.363,
];
/// Pseudo-angle of the red band center; segment positions are measured from it.
pub const HSL_D0: f32 = 0.358_834_68;
/// Segment breakpoints `D_k = fract((d(center_k) - d(center_0)) / 4) * 4`, with
/// `D_8 = 4`. Repeated as literals in `working.wgsl`; a test parses them back.
pub const HSL_BOUNDS: [f32; 9] = [
    0.0,
    0.211_257_77,
    0.905_552_7,
    1.206_947_2,
    1.849_796,
    2.546_809_2,
    2.948_632_2,
    3.259_941,
    4.0,
];
/// `tan(15 deg)`: the Cayley parameter whose rotation `2 atan(u)` is 30 degrees
/// at full hue shift.
pub const HUE_TAN: f32 = 0.267_949_2;
/// Chroma below which the HSL stage is bypassed or faded, in Oklab units.
const GATE_SQUARED: f32 = 1e-12;
const GATE_LOW: f32 = 0.005;
const GATE_HIGH: f32 = 0.04;
/// Uniform rows used by the HSL stage: hue (2), saturation (2), lightness (2).
pub const HSL_ROWS: usize = 6;

/// Rational pseudo-angle in [0, 4), monotone in the true angle of `(a, b)`.
/// Requires `(a, b) != (0, 0)`.
pub fn pseudo_angle(a: f32, b: f32) -> f32 {
    if b >= 0.0 {
        if a >= 0.0 {
            b / (a + b)
        } else {
            1.0 - a / (b - a)
        }
    } else if a < 0.0 {
        2.0 - b / (-a - b)
    } else {
        3.0 + a / (a - b)
    }
}
/// Band `k` and blend `s` toward band `(k + 1) % 8` for the hue of `(a, b)`.
/// Weights `1 - s` and `s` always sum to one; the wrap from magenta to red is
/// segment 7.
pub fn hue_segment(a: f32, b: f32) -> (usize, f32) {
    let x = (pseudo_angle(a, b) - HSL_D0) * 0.25;
    let hd = (x - x.floor()) * 4.0;
    let k = HSL_BOUNDS[1..8]
        .iter()
        .filter(|bound| hd >= **bound)
        .count();
    let t = (hd - HSL_BOUNDS[k]) / (HSL_BOUNDS[k + 1] - HSL_BOUNDS[k]);
    (k, t * t * (3.0 - 2.0 * t))
}
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// Per-band parameter interpolated at segment `(k, s)`. `rows` holds two vec4
/// rows with bands 0-3 then 4-7, like the uniform.
fn band_mix(rows: &[[f32; 4]], k: usize, s: f32) -> f32 {
    let n = (k + 1) & 7;
    let (x, y) = (rows[k >> 2][k & 3], rows[n >> 2][n & 3]);
    x * (1.0 - s) + y * s
}
/// HSL stage on Oklab `[L, a, b]`. `rows` are the six HSL uniform rows (see
/// [`hsl_rows`]). Hue rotates, saturation scales (a, b); lightness scales L.
pub fn hsl_apply([l, a, b]: [f32; 3], rows: &[[f32; 4]]) -> [f32; 3] {
    let c2 = a * a + b * b;
    if c2 < GATE_SQUARED {
        return [l, a, b];
    }
    let gate = smoothstep(GATE_LOW, GATE_HIGH, c2.sqrt());
    let (k, s) = hue_segment(a, b);
    let u = gate * band_mix(&rows[0..2], k, s);
    let scale = 1.0 + gate * band_mix(&rows[2..4], k, s);
    let l = l * (gate * band_mix(&rows[4..6], k, s)).exp2();
    let denominator = 1.0 + u * u;
    let (cos, sin) = ((1.0 - u * u) / denominator, 2.0 * u / denominator);
    [l, (a * cos - b * sin) * scale, (a * sin + b * cos) * scale]
}
/// Whether any of the 24 HSL values is nonzero.
pub fn hsl_active(bands: &[HslBand; 8]) -> bool {
    bands
        .iter()
        .any(|b| b.hue != 0.0 || b.saturation != 0.0 || b.lightness != 0.0)
}
/// Uniform rows for the HSL stage: Cayley hue parameters `tan(15 deg) * hue`,
/// raw saturation, and `0.5 * lightness` (the stops exponent per unit gate).
pub fn hsl_rows(bands: &[HslBand; 8]) -> [[f32; 4]; HSL_ROWS] {
    let mut rows = [[0.0; 4]; HSL_ROWS];
    for (i, band) in bands.iter().enumerate() {
        rows[i >> 2][i & 3] = HUE_TAN * band.hue;
        rows[2 + (i >> 2)][i & 3] = band.saturation;
        rows[4 + (i >> 2)][i & 3] = 0.5 * band.lightness;
    }
    rows
}

/// Uniform rows used by grading: shadows, midtones, highlights.
pub const GRADE_ROWS: usize = 3;
/// Width of each mask ramp in Oklab L; the tests pin the monotonic-lightness
/// bound derived from it and `GRADE_LIFT`.
const GRADE_WIDTH: f32 = 0.30;
/// Pivot between shadow and highlight ramps at balance 0, and its balance slope.
const GRADE_PIVOT: f32 = 0.55;
const GRADE_PIVOT_SLOPE: f32 = 0.20;
/// Oklab (a, b) magnitude of a full-chroma wheel, and L lift of a full lightness.
const GRADE_TINT: f32 = 0.10;
const GRADE_LIFT: f32 = 0.09;
/// Tint fades in over this L so black and negative L stay untinted.
const GRADE_TAPER: f32 = 0.08;

/// Mask pivot for a balance in [-1, 1]; positive balance lowers it so
/// highlights grow.
pub fn grade_pivot(balance: f32) -> f32 {
    GRADE_PIVOT - GRADE_PIVOT_SLOPE * balance
}
/// Shadow, midtone and highlight weights at Oklab `l` for pivot `c`. The ramps
/// are adjacent, so the weights are non-negative and sum to one.
pub fn grade_masks(l: f32, c: f32) -> [f32; 3] {
    let ws = 1.0 - smoothstep(c - GRADE_WIDTH, c, l);
    let wh = smoothstep(c, c + GRADE_WIDTH, l);
    [ws, 1.0 - ws - wh, wh]
}
/// Grading on Oklab `[L, a, b]`. `rows` hold per-wheel `(a, b, dL, 0)` from
/// [`grade_rows`]; `pivot` is [`grade_pivot`]. Tint is a constant additive cast
/// tapered near black, lightness an additive lift.
pub fn grade_apply([l, a, b]: [f32; 3], rows: &[[f32; 4]], pivot: f32) -> [f32; 3] {
    let [ws, wm, wh] = grade_masks(l, pivot);
    let q = smoothstep(0.0, GRADE_TAPER, l);
    let da = ws * rows[0][0] + wm * rows[1][0] + wh * rows[2][0];
    let db = ws * rows[0][1] + wm * rows[1][1] + wh * rows[2][1];
    let dl = ws * rows[0][2] + wm * rows[1][2] + wh * rows[2][2];
    [l + dl, a + q * da, b + q * db]
}
/// Whether any wheel has chroma or lightness; hue and balance alone do nothing.
pub fn grading_active(wheels: &[GradingWheel; 3]) -> bool {
    wheels.iter().any(|w| w.chroma != 0.0 || w.lightness != 0.0)
}
/// Per-wheel uniform rows `(a_offset, b_offset, dL, 0)`; the wheel hue is turns
/// of OkLCh hue.
pub fn grade_rows(wheels: &[GradingWheel; 3]) -> [[f32; 4]; GRADE_ROWS] {
    std::array::from_fn(|i| {
        let w = &wheels[i];
        let (sin, cos) = (std::f32::consts::TAU * w.hue).sin_cos();
        let magnitude = GRADE_TINT * w.chroma;
        [
            magnitude * cos,
            magnitude * sin,
            GRADE_LIFT * w.lightness,
            0.0,
        ]
    })
}
#[cfg(test)]
#[path = "../tests/unit/grading_tests.rs"]
mod tests;

/// Rec.709 luminance weights of linear sRGB primaries, rounded to four digits:
/// the "Luminance" B&W preset. The fold uses the exact [`luma_srgb`] instead.
pub const SRGB_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Channel mixer coefficient limit (both signs).
pub const MIXER_LIMIT: f32 = 2.0;
/// Black-and-white presets: name and linear sRGB channel weights summing to one.
/// The first is the default when monochrome is switched on.
pub const BW_PRESETS: [(&str, [f32; 3]); 7] = [
    ("Luminance", SRGB_LUMA),
    ("Panchromatic", [0.30, 0.58, 0.12]),
    ("Orthochromatic", [0.08, 0.60, 0.32]),
    ("Yellow filter", [0.40, 0.55, 0.05]),
    ("Orange filter", [0.62, 0.35, 0.03]),
    ("Red filter", [0.80, 0.20, 0.00]),
    ("Green filter", [0.10, 0.80, 0.10]),
];

type Mat64 = [[f64; 3]; 3];
fn to_f64(m: Matrix3) -> Mat64 {
    m.map(|r| r.map(f64::from))
}
fn mul64(a: &Mat64, b: &Mat64) -> Mat64 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}
/// Row vector times matrix.
fn row_mul64(v: [f64; 3], m: &Mat64) -> [f64; 3] {
    std::array::from_fn(|j| (0..3).map(|k| v[k] * m[k][j]).sum())
}
/// Inverse by cofactors, in f64. The matrices involved are well conditioned.
fn inverse64(m: &Mat64) -> Mat64 {
    let cof = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let det: f64 = (0..3).map(|c| m[0][c] * cof(0, c)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| cof(j, i) / det))
}
fn narrow(m: &Mat64) -> Matrix3 {
    m.map(|r| r.map(|v| v as f32))
}
/// Exact luminance weights of linear sRGB primaries: `Y_2020 * A`, so that
/// `luma_srgb * A^-1` is `Y_2020` and an identity mixer in monochrome is exact
/// relative luminance. The rounded [`SRGB_LUMA`] differs by about 4e-5.
fn luma_srgb() -> [f64; 3] {
    row_mul64(REC2020_LUMA.map(f64::from), &to_f64(SRGB_TO_REC2020))
}
/// Whether the channel mixer matrix is the identity.
pub fn mixer_is_identity(mixer: &Matrix3) -> bool {
    *mixer == IDENTITY
}
/// Whether the mixer or monochrome changes the folded white-balance rows.
pub fn mixer_active(mixer: &Matrix3, monochrome: bool) -> bool {
    monochrome || !mixer_is_identity(mixer)
}
/// Rows of the single folded 3x3 `mixer * WB` in the working space, or `None`
/// when the mixer is the identity and monochrome is off (the caller keeps the
/// white-balance rows untouched). `mixer` is in linear sRGB primaries with rows
/// as output channels. Computed in f64 and narrowed once. Monochrome replaces all
/// three rows with the luminance of the mixed color (`Y * M`), so R = G = B holds
/// exactly.
pub fn fold_mixer(wb: &Matrix3, mixer: &Matrix3, monochrome: bool) -> Option<Matrix3> {
    if !mixer_active(mixer, monochrome) {
        return None;
    }
    static INVERSE: OnceLock<Mat64> = OnceLock::new();
    let a = to_f64(SRGB_TO_REC2020);
    let inverse = *INVERSE.get_or_init(|| inverse64(&a));
    let m = to_f64(*mixer);
    let wb = to_f64(*wb);
    let work = if monochrome {
        let weights = row_mul64(row_mul64(luma_srgb(), &m), &inverse);
        [weights; 3]
    } else {
        mul64(&mul64(&a, &m), &inverse)
    };
    Some(narrow(&mul64(&work, &wb)))
}
/// Gray weights of a monochrome mixer: `Y * M`, the luminance weights of the
/// mixed color in linear sRGB primaries.
pub fn mono_weights(mixer: &Matrix3) -> [f32; 3] {
    let m = to_f64(*mixer);
    row_mul64(luma_srgb(), &m).map(|v| v as f32)
}
