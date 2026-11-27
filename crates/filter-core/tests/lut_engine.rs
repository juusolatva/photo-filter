mod common;
use common::*;
use filter_core::{
    color::{linear_srgb_to_rec2020, linear_to_srgb, rec2020_to_linear_srgb, srgb_to_linear},
    cpu_eval::BasicAdjustments,
    edit::EditState,
    lut::{LutSlot, parse_cube},
};

fn engine(edit: &EditState) -> BasicAdjustments {
    BasicAdjustments::new(edit).unwrap()
}
/// A linear Rec.2020 pixel whose encoded sRGB value is `e`.
fn from_encoded(e: [f32; 3]) -> [f32; 4] {
    let rgb = linear_srgb_to_rec2020(e.map(srgb_to_linear));
    [rgb[0], rgb[1], rgb[2], 0.5]
}
fn encoded(pixel: [f32; 4]) -> [f32; 3] {
    rec2020_to_linear_srgb([pixel[0], pixel[1], pixel[2]]).map(linear_to_srgb)
}
fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) {
    assert!(
        (0..3).all(|c| (a[c] - b[c]).abs() <= tolerance),
        "{a:?} vs {b:?}"
    );
}

#[test]
fn inactive_lut_is_bit_identical_to_no_lut() {
    let plain = engine(&EditState::default());
    let samples = [
        [0.2, 0.5, 0.8, 0.5],
        [-0.1, 0.0, 3.0, 1.0],
        [f32::NAN, 0.0, 0.0, 1.0],
        [f32::INFINITY, 0.2, f32::NEG_INFINITY, 1.0],
    ];
    let off = engine(&with_lut(invert(), 0.0));
    assert!(off.lut().is_none());
    assert_eq!(off.lut_rows(), [[0.0; 4]; 3]);
    assert_eq!(off.uniform_rows(), plain.uniform_rows());
    for p in samples {
        let (a, b) = (off.pixel(p), plain.pixel(p));
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
    }
    let mut tuned = EditState::default();
    tuned.calibration.exposure = 0.7;
    tuned.curves.master.0 = vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]];
    tuned.color.scalars.saturation = 0.3;
    let mut with = tuned.clone();
    with.finishing.lut = LutSlot::Cube(std::sync::Arc::new(invert()));
    with.finishing.lut_strength = 0.0;
    for p in samples {
        let (a, b) = (engine(&with).pixel(p), engine(&tuned).pixel(p));
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
    }
}

#[test]
fn strength_one_equals_the_lookup_and_keeps_alpha() {
    let adjustments = engine(&with_lut(invert(), 1.0));
    for e in [[0.2, 0.5, 0.8], [0.0, 1.0, 0.31], [0.9, 0.05, 0.6]] {
        let out = adjustments.pixel(from_encoded(e));
        close(encoded(out), e.map(|v| 1.0 - v), 2e-5);
        assert_eq!(out[3], 0.5);
    }
}

#[test]
fn strength_mixes_in_encoded_space() {
    let e = [0.2, 0.5, 0.8];
    let half = engine(&with_lut(invert(), 0.5)).pixel(from_encoded(e));
    close(encoded(half), [0.5; 3], 2e-5);
    let quarter = engine(&with_lut(invert(), 0.25)).pixel(from_encoded(e));
    close(
        encoded(quarter),
        e.map(|v| v + 0.25 * (1.0 - 2.0 * v)),
        2e-5,
    );
}

#[test]
fn out_of_domain_values_keep_their_residual_at_unit_slope() {
    let adjustments = engine(&with_lut(invert(), 1.0));
    // e > 1: lookup(1) + (e - 1) = 0 + 0.5; e < 0: lookup(0) + e = 1 - 0.25.
    let out = encoded(adjustments.pixel(from_encoded([1.5, -0.25, 1.0])));
    close(out, [0.5, 0.75, 0.0], 1e-4);
    // Continuous across the domain edge.
    let below = encoded(adjustments.pixel(from_encoded([0.9999; 3])))[0];
    let above = encoded(adjustments.pixel(from_encoded([1.0001; 3])))[0];
    assert!((below - above).abs() < 5e-4, "{below} {above}");
}

#[test]
fn identity_luts_keep_the_image_up_to_f16_precision() {
    let identity2 = engine(&with_lut(identity(2), 1.0));
    let identity17 = engine(&with_lut(identity(17), 1.0));
    for e in [
        [0.1, 0.4, 0.9],
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.333, 0.666, 0.999],
    ] {
        let input = from_encoded(e);
        close(encoded(identity2.pixel(input)), e, 1e-5);
        close(encoded(identity17.pixel(input)), e, 5e-4);
    }
    // HDR and negative values pass through as the identity as well.
    for e in [[1.8, 0.5, -0.4], [2.5, 2.5, 2.5]] {
        close(encoded(identity2.pixel(from_encoded(e))), e, 1e-4);
    }
}

#[test]
fn custom_domains_map_the_input_only() {
    let text = common::cube_text(
        2,
        "DOMAIN_MIN -0.5 -0.5 -0.5\nDOMAIN_MAX 1.5 1.5 1.5",
        |r, g, b| [r, g, b].map(|v| -0.5 + 2.0 * v as f32),
    );
    let lut = parse_cube(text.as_bytes()).unwrap();
    let adjustments = engine(&with_lut(lut, 1.0));
    let rows = adjustments.lut_rows();
    assert_eq!(rows[0], [-0.5, -0.5, -0.5, 1.0]);
    assert_eq!(rows[1], [1.5, 1.5, 1.5, 1.0]);
    assert_eq!(rows[2], [0.5, 0.5, 0.5, 1.0]);
    for e in [[-0.4, 0.0, 1.4], [0.25, 0.5, 0.75], [-0.5, 1.5, 0.0]] {
        close(encoded(adjustments.pixel(from_encoded(e))), e, 1e-4);
    }
    close(
        encoded(adjustments.pixel(from_encoded([1.9, -0.9, 0.2]))),
        [1.9, -0.9, 0.2],
        1e-4,
    );
}

#[test]
fn nonfinite_pixels_skip_the_stage() {
    let with = engine(&with_lut(invert(), 1.0));
    let without = engine(&EditState::default());
    for p in [
        [f32::NAN, 0.2, 0.3, 1.0],
        [0.1, f32::INFINITY, 0.3, 1.0],
        [0.1, 0.2, f32::NEG_INFINITY, 0.25],
    ] {
        let (a, b) = (with.pixel(p), without.pixel(p));
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
    }
}

#[test]
fn the_stage_follows_curves_and_color_and_precedes_alpha() {
    let mut edit = with_lut(invert(), 1.0);
    edit.curves.master.0 = vec![[0.0, 0.1], [1.0, 0.9]];
    edit.color.scalars.saturation = -1.0;
    let both = engine(&edit);
    let mut stages = edit.clone();
    stages.finishing.lut_strength = 0.0;
    let before = engine(&stages).pixel(from_encoded([0.7, 0.2, 0.4]));
    let after = both.pixel(from_encoded([0.7, 0.2, 0.4]));
    // Desaturated gray, curved, then inverted.
    let gray = encoded(before);
    close(encoded(after), gray.map(|v| 1.0 - v), 3e-5);
    assert_eq!(after[3], 0.5);
}

#[test]
fn lut_only_edits_are_renderable_and_not_neutral() {
    let neutral = engine(&EditState::default());
    assert!(neutral.is_neutral());
    assert!(neutral.lut().is_none());
    let looked = engine(&with_lut(identity(2), 1.0));
    assert!(!looked.is_neutral());
    assert_eq!(looked.uniform_rows(), neutral.uniform_rows());
    assert_eq!(looked.lut().unwrap().strength, 1.0);
    assert!(engine(&with_lut(identity(2), 0.0)).is_neutral());
    assert_eq!(engine(&with_lut(identity(2), 0.3)).lut_rows()[0][3], 0.3);
    let mut grained = with_lut(identity(2), 1.0);
    grained.finishing.grain_amount = 0.2;
    assert!(BasicAdjustments::new(&grained).is_err());
    let mut vignette = with_lut(identity(2), 1.0);
    vignette.finishing.vignette_amount = 0.2;
    assert!(BasicAdjustments::new(&vignette).is_err());
    assert_eq!(EditState::default().finishing.lut, LutSlot::None);
    assert_eq!(EditState::default().finishing.lut_strength, 1.0);
}

#[test]
fn lut_rows_use_the_same_f32_values_as_the_cpu_stage() {
    let lut = parse_cube(
        cube_text(5, "DOMAIN_MIN 0 -1 0.25\nDOMAIN_MAX 3 2 1.75", |r, g, b| {
            [r, g, b].map(|v| v as f32 / 4.0)
        })
        .as_bytes(),
    )
    .unwrap();
    let rows = engine(&with_lut(lut, 0.75)).lut_rows();
    assert_eq!(rows[0], [0.0, -1.0, 0.25, 0.75]);
    assert_eq!(rows[1], [3.0, 2.0, 1.75, 1.0]);
    assert_eq!(rows[2], [1.0 / 3.0, 1.0 / 3.0, 1.0 / 1.5, 4.0]);
}
