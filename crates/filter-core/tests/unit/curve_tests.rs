use super::{CurveLut, LUT_SIZE, Spline, UNIFORM_BYTES, is_identity};
use crate::color::{
    linear_srgb_to_rec2020, linear_to_srgb, rec2020_to_linear_srgb, srgb_to_linear,
};
use crate::edit::{Curve, Curves};

fn curve(points: &[[f32; 2]]) -> Curve {
    Curve(points.to_vec())
}
fn with_master(points: &[[f32; 2]]) -> Curves {
    Curves {
        master: curve(points),
        ..Curves::default()
    }
}

#[test]
fn identity_curves_skip_the_stage() {
    assert!(CurveLut::new(&Curves::default()).is_none());
    let on_diagonal = with_master(&[[0.0, 0.0], [0.3, 0.3], [0.8, 0.8], [1.0, 1.0]]);
    assert!(is_identity(&on_diagonal.master));
    assert!(CurveLut::new(&on_diagonal).is_none());
    assert!(CurveLut::new(&with_master(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]])).is_some());
}

#[test]
fn two_point_curves_are_straight_lines_extended_beyond_the_ends() {
    let lut = CurveLut::new(&with_master(&[[0.0, 0.1], [1.0, 0.9]])).unwrap();
    assert_eq!(lut.uniform_rows().len() * 16, UNIFORM_BYTES);
    for i in 0..=20 {
        let x = i as f32 / 20.0;
        assert!((lut.lookup(0, x) - (0.1 + 0.8 * x)).abs() < 1e-6, "{x}");
    }
    assert!((lut.lookup(0, -0.5) - (0.1 - 0.4)).abs() < 1e-6);
    assert!((lut.lookup(0, 2.0) - (0.9 + 0.8)).abs() < 1e-6);
    // Untouched channels remain the identity everywhere, including outside [0, 1].
    for x in [-0.25, 0.0, 0.37, 1.0, 1.5] {
        for channel in 1..4 {
            assert!((lut.lookup(channel, x) - x).abs() < 1e-6, "{channel} {x}");
        }
    }
}

#[test]
fn splines_interpolate_points_without_overshoot() {
    let points = [
        [0.0, 0.0],
        [0.2, 0.05],
        [0.5, 0.7],
        [0.55, 0.72],
        [1.0, 1.0],
    ];
    let spline = Spline::new(&curve(&points));
    for [x, y] in points {
        assert!((spline.eval(f64::from(x)) - f64::from(y)).abs() < 1e-12);
    }
    // Monotone points give a monotone curve that stays within each interval.
    let mut previous = f64::NEG_INFINITY;
    for i in 0..=1000 {
        let x = f64::from(i) / 1000.0;
        let y = spline.eval(x);
        assert!(y >= previous - 1e-12, "monotone at {x}");
        let k = points
            .partition_point(|p| f64::from(p[0]) <= x)
            .clamp(1, points.len() - 1);
        let (lo, hi) = (f64::from(points[k - 1][1]), f64::from(points[k][1]));
        assert!(
            y >= lo.min(hi) - 1e-12 && y <= lo.max(hi) + 1e-12,
            "{x}: {y}"
        );
        previous = y;
    }
    // A bump peaks at its control point instead of overshooting it.
    let bump = Spline::new(&curve(&[[0.0, 0.0], [0.4, 0.8], [1.0, 0.2]]));
    let peak = (0..=1000)
        .map(|i| bump.eval(f64::from(i) / 1000.0))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((peak - f64::from(0.8_f32)).abs() < 1e-9, "{peak}");
    // Flat end intervals extend flat rather than tilting.
    let flat = Spline::new(&curve(&[[0.0, 0.2], [0.3, 0.2], [1.0, 1.0]]));
    assert_eq!(flat.end_slopes()[0], 0.0);
    assert_eq!(flat.eval(-1.0), f64::from(0.2_f32));
}

#[test]
fn tables_sample_the_spline_and_continue_with_its_end_tangents() {
    let points = [[0.0, 0.05], [0.25, 0.22], [0.75, 0.78], [1.0, 1.2]];
    let lut = CurveLut::new(&with_master(&points)).unwrap();
    let spline = Spline::new(&curve(&points));
    for i in [0, 1, 511, 1022, 1023] {
        let x = i as f32 / (LUT_SIZE - 1) as f32;
        assert!((f64::from(lut.lookup(0, x)) - spline.eval(f64::from(x))).abs() < 1e-6);
    }
    // Linear interpolation between samples stays close to the spline.
    for i in 0..1000 {
        let x = (i as f32 + 0.5) / 1000.0;
        assert!((f64::from(lut.lookup(0, x)) - spline.eval(f64::from(x))).abs() < 1e-5);
    }
    let [lower, upper] = spline.end_slopes();
    for d in [1e-3_f32, 0.5] {
        assert!((f64::from(lut.lookup(0, 1.0 + d)) - (1.2 + upper * f64::from(d))).abs() < 1e-5);
        assert!((f64::from(lut.lookup(0, -d)) - (0.05 - lower * f64::from(d))).abs() < 1e-5);
    }
    assert!(lut.lookup(0, f32::NAN).is_nan());
}

#[test]
fn apply_runs_master_then_channel_curves_in_srgb_primaries() {
    let curves = Curves {
        master: curve(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]),
        red: curve(&[[0.0, 0.0], [1.0, 0.8]]),
        ..Curves::default()
    };
    let lut = CurveLut::new(&curves).unwrap();
    for linear_srgb in [[0.18, 0.4, 0.05], [-0.02, 0.5, 1.4], [0.0, 0.0, 0.0]] {
        let output = rec2020_to_linear_srgb(lut.apply(linear_srgb_to_rec2020(linear_srgb)));
        for c in 0..3 {
            let master = lut.lookup(0, linear_to_srgb(linear_srgb[c]));
            let expected = srgb_to_linear(if c == 0 { master * 0.8 } else { master });
            assert!(
                (output[c] - expected).abs() < 2e-5 * (1.0 + expected.abs()),
                "{linear_srgb:?} channel {c}: {} vs {expected}",
                output[c]
            );
        }
    }
    // Brightening the midtones lifts gray without changing its hue.
    let gray = linear_srgb_to_rec2020([0.18; 3]);
    let lifted = rec2020_to_linear_srgb(
        CurveLut::new(&with_master(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]))
            .unwrap()
            .apply(gray),
    );
    assert!(
        lifted[0] > 0.18
            && (lifted[0] - lifted[1]).abs() < 1e-5
            && (lifted[1] - lifted[2]).abs() < 1e-5
    );
}
