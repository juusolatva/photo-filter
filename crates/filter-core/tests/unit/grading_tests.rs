use super::*;
use crate::color::{linear_srgb_to_rec2020, rec2020_to_oklab};
use crate::edit::GradingWheel;

const REFERENCES: [[f32; 3]; 8] = [
    [1.0, 0.0, 0.0],
    [1.0, 0.5019608, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 1.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.5019608, 0.0, 1.0],
    [1.0, 0.0, 1.0],
];
/// Hue in degrees of a gamma-encoded sRGB reference color.
fn reference_hue(encoded: [f32; 3]) -> f32 {
    let linear = encoded.map(crate::color::srgb_to_linear);
    let [_, a, b] = rec2020_to_oklab(linear_srgb_to_rec2020(linear));
    b.atan2(a).to_degrees().rem_euclid(360.0)
}
fn pseudo(degrees: f64) -> f64 {
    let (b, a) = degrees.to_radians().sin_cos();
    f64::from(pseudo_angle(a as f32, b as f32))
}
fn lab_at(degrees: f32, chroma: f32, l: f32) -> [f32; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [l, chroma * cos, chroma * sin]
}
fn rows_with(f: impl Fn(&mut HslBand, usize)) -> [[f32; 4]; HSL_ROWS] {
    let mut bands: [HslBand; 8] = Default::default();
    bands.iter_mut().enumerate().for_each(|(i, b)| f(b, i));
    hsl_rows(&bands)
}
fn hue_of(lab: [f32; 3]) -> f32 {
    lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0)
}

#[test]
fn band_centers_match_the_conversion_and_breakpoints_match_the_literals() {
    for (reference, degrees) in REFERENCES.iter().zip(BAND_CENTER_DEGREES) {
        let measured = reference_hue(*reference);
        assert!((measured - degrees).abs() < 0.1, "{measured} vs {degrees}");
    }
    let d0 = pseudo(f64::from(BAND_CENTER_DEGREES[0]));
    assert!((d0 - f64::from(HSL_D0)).abs() < 1e-6);
    for (k, degrees) in BAND_CENTER_DEGREES.iter().enumerate() {
        let x = (pseudo(f64::from(*degrees)) - d0) / 4.0;
        let expected = (x - x.floor()) * 4.0;
        assert!(
            (expected - f64::from(HSL_BOUNDS[k])).abs() < 1e-6,
            "band {k}"
        );
    }
    assert_eq!(HSL_BOUNDS[8], 4.0);
    assert!(HSL_BOUNDS.windows(2).all(|w| w[0] < w[1]));
}
#[test]
fn shader_constants_are_the_rust_constants() {
    let source = include_str!("../../../filter-gpu/src/working.wgsl");
    let number = |text: &str| text.trim().trim_end_matches(';').parse::<f32>().unwrap();
    let d0 = source
        .split("const HSL_D0: f32 =")
        .nth(1)
        .and_then(|t| t.split(';').next())
        .unwrap();
    assert_eq!(number(d0), HSL_D0);
    let bounds = source
        .split("const HSL_BOUNDS = array<f32, 9>(")
        .nth(1)
        .and_then(|t| t.split(");").next())
        .unwrap();
    let parsed: Vec<f32> = bounds.split(',').map(number).collect();
    assert_eq!(parsed, HSL_BOUNDS);
}
#[test]
fn segments_partition_unity_and_follow_hue_order() {
    // Sweep one full turn starting just past the red center; the position
    // `k + s` along the ring never decreases.
    let mut previous = 0.0_f32;
    for i in 0..7200 {
        let degrees = BAND_CENTER_DEGREES[0] + 0.01 + i as f32 * 0.05;
        let (b, a) = degrees.to_radians().sin_cos();
        let (k, s) = hue_segment(a, b);
        assert!(k < 8 && (0.0..=1.0).contains(&s), "{degrees}: {k} {s}");
        let position = k as f32 + s;
        assert!(
            position + 1e-4 >= previous,
            "{degrees}: {position} < {previous}"
        );
        previous = position;
    }
}
#[test]
fn each_center_returns_exactly_its_band() {
    for (k, degrees) in BAND_CENTER_DEGREES.iter().enumerate() {
        let rows = rows_with(|b, i| b.saturation = if i == k { 0.5 } else { -0.7 });
        let [_, a, b] = hsl_apply(lab_at(*degrees, 0.2, 0.6), &rows);
        let scale = a.hypot(b) / 0.2;
        assert!((scale - 1.5).abs() < 2e-3, "band {k}: {scale}");
    }
}
#[test]
fn parameters_are_continuous_across_every_center_and_the_seam() {
    let rows = rows_with(|b, i| b.saturation = if i % 2 == 0 { 1.0 } else { -1.0 });
    let mut previous: Option<f32> = None;
    for i in 0..36000 {
        let degrees = i as f32 * 0.01;
        let [_, a, b] = hsl_apply(lab_at(degrees, 0.2, 0.5), &rows);
        let scale = a.hypot(b) / 0.2;
        if let Some(before) = previous {
            // Adjacent bands differ by 2; segments are at least ~11 degrees wide.
            assert!(
                (scale - before).abs() < 0.05,
                "{degrees}: {before} -> {scale}"
            );
        }
        previous = Some(scale);
    }
}
#[test]
fn uniform_bands_act_globally() {
    let lab = lab_at(77.0, 0.15, 0.6);
    let rows = rows_with(|b, _| b.hue = 1.0);
    let rotated = hsl_apply(lab, &rows);
    assert!((hue_of(rotated) - hue_of(lab) - 30.0).abs() < 1e-3);
    assert!((rotated[1].hypot(rotated[2]) - 0.15).abs() < 1e-5);
    assert_eq!(rotated[0], lab[0]);
    let rows = rows_with(|b, _| b.saturation = 0.4);
    let scaled = hsl_apply(lab, &rows);
    assert!((scaled[1].hypot(scaled[2]) - 0.21).abs() < 1e-5);
    let rows = rows_with(|b, _| b.lightness = -1.0);
    let darker = hsl_apply(lab, &rows);
    assert!((darker[0] - 0.6 * 0.5_f32.exp2().recip()).abs() < 1e-5);
    assert_eq!([darker[1], darker[2]], [lab[1], lab[2]]);
}
#[test]
fn gray_and_low_chroma_pixels_are_unchanged() {
    let rows = rows_with(|b, _| {
        *b = HslBand {
            hue: 1.0,
            saturation: 1.0,
            lightness: 1.0,
        }
    });
    for lab in [
        [0.5, 0.0, 0.0],
        lab_at(10.0, 0.004, 0.5),
        lab_at(200.0, 0.0049, 0.9),
    ] {
        assert_eq!(hsl_apply(lab, &rows), lab);
    }
}
#[test]
fn hue_shift_keeps_chroma_and_lightness() {
    let rows = rows_with(|b, i| b.hue = if i % 2 == 0 { 0.8 } else { -0.6 });
    for degrees in (0..360).step_by(7) {
        let lab = lab_at(degrees as f32, 0.12, 0.55);
        let out = hsl_apply(lab, &rows);
        assert_eq!(out[0], lab[0]);
        assert!((out[1].hypot(out[2]) - 0.12).abs() < 1e-5);
    }
}
#[test]
fn saturation_minus_one_removes_a_band_at_its_center() {
    let rows = rows_with(|b, i| b.saturation = if i == 3 { -1.0 } else { 0.0 });
    let [_, a, b] = hsl_apply(lab_at(BAND_CENTER_DEGREES[3], 0.2, 0.7), &rows);
    assert!(a.hypot(b) < 1e-3, "{a} {b}");
}
#[test]
fn hsl_rows_pack_the_documented_layout() {
    let mut bands: [HslBand; 8] = Default::default();
    assert!(!hsl_active(&bands));
    assert_eq!(hsl_rows(&bands), [[0.0; 4]; HSL_ROWS]);
    bands[5] = HslBand {
        hue: 0.5,
        saturation: -0.25,
        lightness: 1.0,
    };
    assert!(hsl_active(&bands));
    let rows = hsl_rows(&bands);
    assert_eq!(rows[1][1], HUE_TAN * 0.5);
    assert_eq!(rows[3][1], -0.25);
    assert_eq!(rows[5][1], 0.5);
    assert_eq!(rows.iter().flatten().filter(|v| **v != 0.0).count(), 3);
}
#[test]
fn extreme_inputs_stay_finite() {
    let rows = rows_with(|b, i| {
        *b = HslBand {
            hue: if i % 2 == 0 { 1.0 } else { -1.0 },
            saturation: if i % 3 == 0 { 1.0 } else { -1.0 },
            lightness: if i % 4 == 0 { 1.0 } else { -1.0 },
        }
    });
    let chromas = [0.0, 1e-7, 0.005, 0.03, 0.4, 5.0, 1e6];
    for l in [-1.0, -0.001, 0.0, 0.3, 1.0, 10.0, 1e6] {
        for chroma in chromas {
            for degrees in (0..360).step_by(15) {
                let out = hsl_apply(lab_at(degrees as f32, chroma, l), &rows);
                assert!(out.iter().all(|v| v.is_finite()), "{l} {chroma} {degrees}");
            }
        }
        assert!(
            hsl_apply([l, 0.0, 0.0], &rows)
                .iter()
                .all(|v| v.is_finite())
        );
        for axis in [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0]] {
            let out = hsl_apply([l, 0.1 * axis[0], 0.1 * axis[1]], &rows);
            assert!(out.iter().all(|v| v.is_finite()));
        }
    }
}

fn wheels(f: impl Fn(&mut GradingWheel, usize)) -> [GradingWheel; 3] {
    let mut wheels: [GradingWheel; 3] = Default::default();
    wheels.iter_mut().enumerate().for_each(|(i, w)| f(w, i));
    wheels
}
#[test]
fn grade_masks_partition_unity_and_follow_the_pivot() {
    for balance in [-1.0, -0.3, 0.0, 0.6, 1.0] {
        let c = grade_pivot(balance);
        for i in -50..=300 {
            let l = i as f32 * 0.01;
            let m = grade_masks(l, c);
            assert!(m.iter().all(|w| *w >= 0.0), "{l}: {m:?}");
            assert!((m.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
        assert_eq!(grade_masks(c, c)[1], 1.0);
        assert_eq!(grade_masks(-1.0, c), [1.0, 0.0, 0.0]);
        assert_eq!(grade_masks(0.0, c)[0], 1.0 - smoothstep(c - 0.3, c, 0.0));
        assert_eq!(grade_masks(50.0, c), [0.0, 0.0, 1.0]);
    }
    assert!((grade_pivot(0.0) - 0.55).abs() < 1e-7);
    assert!((grade_pivot(1.0) - 0.35).abs() < 1e-7);
    assert!((grade_pivot(-1.0) - 0.75).abs() < 1e-7);
    // Positive balance grows highlights (Lightroom direction).
    assert!(grade_masks(0.5, grade_pivot(1.0))[2] > grade_masks(0.5, grade_pivot(-1.0))[2]);
    let pivots: Vec<f32> = (-10..=10).map(|i| grade_pivot(i as f32 / 10.0)).collect();
    assert!(pivots.windows(2).all(|w| w[1] < w[0]));
}
#[test]
fn lightness_map_is_strictly_increasing_at_every_corner() {
    for balance in [-1.0, 0.0, 1.0] {
        let c = grade_pivot(balance);
        for corner in 0..8 {
            let rows = grade_rows(&wheels(|w, i| {
                w.lightness = if corner >> i & 1 == 1 { 1.0 } else { -1.0 }
            }));
            let mut previous = grade_apply([-1.0, 0.0, 0.0], &rows, c)[0];
            for i in -999..4000 {
                let l = i as f32 * 0.001;
                let out = grade_apply([l, 0.0, 0.0], &rows, c)[0];
                let slope = (out - previous) / 0.001;
                assert!(
                    slope >= 0.09,
                    "corner {corner} balance {balance} L {l}: {slope}"
                );
                previous = out;
            }
        }
    }
}
#[test]
fn tint_is_absent_at_or_below_black_and_applies_to_neutrals() {
    let rows = grade_rows(&wheels(|w, _| {
        w.chroma = 1.0;
        w.hue = 0.25;
    }));
    for l in [-2.0, -0.01, 0.0] {
        assert_eq!(grade_apply([l, 0.0, 0.0], &rows, 0.55), [l, 0.0, 0.0]);
    }
    let [l, a, b] = grade_apply([0.5, 0.0, 0.0], &rows, 0.55);
    assert_eq!(l, 0.5);
    assert!(a.abs() < 1e-6 && (b - 0.10).abs() < 1e-6, "{a} {b}");
    // The cast is a constant offset: HDR L is not scaled.
    let [_, a, b] = grade_apply([8.0, 0.1, 0.0], &rows, 0.55);
    assert!((a - 0.1).abs() < 1e-6 && (b - 0.10).abs() < 1e-6);
}
#[test]
fn wheels_pick_their_own_masks_and_hue_is_in_turns() {
    let rows = grade_rows(&wheels(|w, i| {
        if i == 0 {
            w.chroma = 0.5;
            w.hue = 0.5;
        }
    }));
    assert!((rows[0][0] + 0.05).abs() < 1e-6 && rows[0][1].abs() < 1e-6);
    // Hue 0 and 1 are the same direction.
    let (zero, one) = (
        grade_rows(&wheels(|w, _| w.chroma = 1.0)),
        grade_rows(&wheels(|w, _| {
            w.chroma = 1.0;
            w.hue = 1.0;
        })),
    );
    assert!((zero[0][0] - one[0][0]).abs() < 1e-6 && one[0][1].abs() < 1e-6);
    let dark = grade_apply([0.05, 0.0, 0.0], &rows, 0.55);
    let bright = grade_apply([1.0, 0.0, 0.0], &rows, 0.55);
    assert!(dark[1] < -0.02, "{dark:?}");
    assert_eq!(bright, [1.0, 0.0, 0.0]);
}
#[test]
fn grading_activity_ignores_hue_and_pack_is_exact() {
    let mut w: [GradingWheel; 3] = Default::default();
    assert!(!grading_active(&w));
    assert_eq!(grade_rows(&w), [[0.0; 4]; GRADE_ROWS]);
    w[1].hue = 0.7;
    assert!(!grading_active(&w));
    w[2].lightness = -0.5;
    assert!(grading_active(&w));
    assert!((grade_rows(&w)[2][2] + 0.045).abs() < 1e-7);
    w[2].lightness = 0.0;
    w[0].chroma = 0.1;
    assert!(grading_active(&w));
}
#[test]
fn grading_stays_finite() {
    let rows = grade_rows(&wheels(|w, i| {
        w.chroma = 1.0;
        w.lightness = if i == 1 { 1.0 } else { -1.0 };
        w.hue = 0.3 * i as f32;
    }));
    for l in [-1e6, -1.0, 0.0, 0.3, 1.0, 10.0, 1e6] {
        for balance in [-1.0, 1.0] {
            let out = grade_apply([l, 0.1, -0.2], &rows, grade_pivot(balance));
            assert!(out.iter().all(|v| v.is_finite()));
        }
    }
}

fn srgb_to_working_inverse_error() -> f64 {
    // A * A^-1 must be the identity to f64 precision.
    let a = to_f64(SRGB_TO_REC2020);
    let product = mul64(&a, &inverse64(&a));
    (0..3)
        .flat_map(|i| (0..3).map(move |j| (i, j)))
        .map(|(i, j)| (product[i][j] - if i == j { 1.0 } else { 0.0 }).abs())
        .fold(0.0, f64::max)
}
#[test]
fn matrix_inverse_is_exact_in_f64() {
    assert!(srgb_to_working_inverse_error() < 1e-14);
}
#[test]
fn fold_is_skipped_for_identity_and_present_otherwise() {
    assert!(fold_mixer(&IDENTITY, &IDENTITY, false).is_none());
    assert!(fold_mixer(&IDENTITY, &IDENTITY, true).is_some());
    let mut m = IDENTITY;
    m[0][2] = 1e-7;
    assert!(fold_mixer(&IDENTITY, &m, false).is_some());
}
#[test]
fn mono_rows_are_identical_and_equal_luminance_for_identity() {
    let rows = fold_mixer(&IDENTITY, &IDENTITY, true).unwrap();
    assert_eq!(rows[0], rows[1]);
    assert_eq!(rows[1], rows[2]);
    let y = [0.2627002, 0.67799807, 0.059301715];
    for (got, want) in rows[0].iter().zip(y) {
        assert!((got - want).abs() < 1e-6);
    }
}
#[test]
fn bw_presets_sum_to_one_and_round_trip_through_the_weights() {
    assert_eq!(BW_PRESETS[0].1, SRGB_LUMA);
    for (name, w) in BW_PRESETS {
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6, "{name}");
        let got = mono_weights(&[w; 3]);
        for i in 0..3 {
            assert!((got[i] - w[i]).abs() < 1e-6, "{name}");
        }
    }
    assert_eq!(
        mono_weights(&IDENTITY).map(|v| (v * 1e4).round()),
        SRGB_LUMA.map(|v| (v * 1e4).round())
    );
}
