use super::*;
#[test]
fn transfer_preserves_extended_range() {
    for x in [-2.0, -0.1, 0.0, 0.001, 0.5, 1.0, 4.0] {
        assert!((srgb_to_linear(linear_to_srgb(x)) - x).abs() < 0.00001);
    }
}
