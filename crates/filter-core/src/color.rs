/// Mirrored sRGB transfer, preserving negative values and HDR headroom.
pub fn srgb_to_linear(x: f32) -> f32 {
    x.signum()
        * if x.abs() <= 0.04045 {
            x.abs() / 12.92
        } else {
            ((x.abs() + 0.055) / 1.055).powf(2.4)
        }
}
pub fn linear_to_srgb(x: f32) -> f32 {
    x.signum()
        * if x.abs() <= 0.0031308 {
            x.abs() * 12.92
        } else {
            1.055 * x.abs().powf(1.0 / 2.4) - 0.055
        }
}
pub fn rec2020_to_linear_srgb(c: [f32; 3]) -> [f32; 3] {
    [
        1.660491 * c[0] - 0.587641 * c[1] - 0.072850 * c[2],
        -0.124550 * c[0] + 1.1329 * c[1] - 0.008349 * c[2],
        -0.018151 * c[0] - 0.100579 * c[1] + 1.11873 * c[2],
    ]
}

pub type Matrix3 = [[f32; 3]; 3];
pub const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn transform(m: Matrix3, c: [f32; 3]) -> [f32; 3] {
    m.map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2])
}
fn product(a: Matrix3, b: Matrix3) -> Matrix3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

// D65 matrices from the CSS Color 4 sample conversion code.
// https://www.w3.org/TR/css-color-4/#color-conversion-code
const REC2020_XYZ: Matrix3 = [
    [0.636958, 0.1446169, 0.16888097],
    [0.2627002, 0.67799807, 0.059301715],
    [0.0, 0.028072692, 1.0609851],
];
const XYZ_REC2020: Matrix3 = [
    [1.7166512, -0.35567078, -0.2533663],
    [-0.6666843, 1.6164812, 0.015768547],
    [0.017639857, -0.042770613, 0.94210315],
];
pub fn rec2020_to_xyz(c: [f32; 3]) -> [f32; 3] {
    transform(REC2020_XYZ, c)
}
pub fn xyz_to_rec2020(c: [f32; 3]) -> [f32; 3] {
    transform(XYZ_REC2020, c)
}
pub fn linear_srgb_to_rec2020(c: [f32; 3]) -> [f32; 3] {
    transform(
        [
            [0.6274039, 0.32928303, 0.043313067],
            [0.06909729, 0.9195404, 0.011362316],
            [0.01639144, 0.088013306, 0.89559525],
        ],
        c,
    )
}
pub fn linear_p3_to_rec2020(c: [f32; 3]) -> [f32; 3] {
    transform(
        [
            [0.75383306, 0.19859737, 0.0475696],
            [0.04574385, 0.9417772, 0.012478931],
            [-0.0012103403, 0.017601717, 0.9836086],
        ],
        c,
    )
}
pub fn rec2020_to_linear_p3(c: [f32; 3]) -> [f32; 3] {
    transform(
        [
            [1.3435782, -0.28217968, -0.06139858],
            [-0.065297455, 1.0757879, -0.010490463],
            [0.002821787, -0.019598495, 1.0167767],
        ],
        c,
    )
}

pub fn rec2020_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let lms = transform(
        [
            [0.8190224, 0.36190626, -0.12887378],
            [0.032983653, 0.92928684, 0.036144666],
            [0.04817719, 0.26423952, 0.63354784],
        ],
        rec2020_to_xyz(c),
    )
    .map(f32::cbrt);
    transform(
        [
            [0.21045427, 0.7936178, -0.004072043],
            [1.9779985, -2.4285922, 0.4505937],
            [0.025904043, 0.7827717, -0.80867577],
        ],
        lms,
    )
}
pub fn oklab_to_rec2020(c: [f32; 3]) -> [f32; 3] {
    let lms = transform(
        [
            [1.0, 0.39633778, 0.21580376],
            [1.0, -0.105561346, -0.06385417],
            [1.0, -0.08948418, -1.2914855],
        ],
        c,
    )
    .map(|x| x * x * x);
    xyz_to_rec2020(transform(
        [
            [1.2268798, -0.557815, 0.28139105],
            [-0.040575746, 1.1122868, -0.071711056],
            [-0.07637294, -0.42149332, 1.5869241],
        ],
        lms,
    ))
}
/// OkLCh hue in radians. Achromatic hue is zero.
pub fn oklab_to_oklch([l, a, b]: [f32; 3]) -> [f32; 3] {
    [l, a.hypot(b), b.atan2(a)]
}
pub fn oklch_to_oklab([l, c, h]: [f32; 3]) -> [f32; 3] {
    [l, c * h.cos(), c * h.sin()]
}

/// Full CAT16 adaptation from D65 to an application-defined relative white.
/// Controls are normalized [-1, 1], positive = warmer / more magenta.
/// This is a creative relative adjustment, not a source-camera Kelvin estimate.
pub fn relative_white_balance(temperature: f32, tint: f32) -> Matrix3 {
    if temperature == 0.0 && tint == 0.0 {
        return IDENTITY;
    }
    let cat = [
        [0.401288, 0.650173, -0.051461],
        [-0.250268, 1.204414, 0.045854],
        [-0.002079, 0.048952, 0.953127],
    ];
    let inverse = [
        [1.8620678, -1.0112547, 0.14918678],
        [0.38752654, 0.62144744, -0.008973985],
        [-0.0158415, -0.03412294, 1.0499644],
    ];
    let x = 0.3127 + 0.04 * temperature + 0.01 * tint;
    let y = 0.3290 + 0.04 * temperature - 0.025 * tint;
    let source = transform(cat, rec2020_to_xyz([1.0; 3]));
    let target = transform(cat, [x / y, 1.0, (1.0 - x - y) / y]);
    let gains = std::array::from_fn::<_, 3, _>(|i| target[i] / source[i]);
    let scaled = std::array::from_fn(|i| cat[i].map(|v| v * gains[i]));
    product(XYZ_REC2020, product(inverse, product(scaled, REC2020_XYZ)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_preserves_extended_range() {
        for x in [-2.0, -0.1, 0.0, 0.001, 0.5, 1.0, 4.0] {
            assert!((srgb_to_linear(linear_to_srgb(x)) - x).abs() < 0.00001);
        }
    }
}
