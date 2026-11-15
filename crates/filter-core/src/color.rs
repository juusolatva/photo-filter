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
