use super::*;
use crate::{
    color::{Matrix3, rec2020_to_oklab, relative_white_balance, transform},
    cpu_eval::BasicAdjustments,
    edit::EditState,
};
use half::f16;

fn invert(m: Matrix3) -> Matrix3 {
    let a = m.map(|r| r.map(f64::from));
    let cofactor = |i: usize, j: usize| {
        let (r, c) = ([(i + 1) % 3, (i + 2) % 3], [(j + 1) % 3, (j + 2) % 3]);
        a[r[0]][c[0]] * a[r[1]][c[1]] - a[r[0]][c[1]] * a[r[1]][c[0]]
    };
    let det: f64 = (0..3).map(|j| a[0][j] * cofactor(0, j)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| (cofactor(j, i) / det) as f32))
}
/// A gray that the white balance (temperature, tint) would turn into a cast, undone.
fn tinted_gray(temperature: f32, tint: f32, level: f32) -> [f32; 3] {
    transform(
        invert(relative_white_balance(temperature, tint)),
        [level; 3],
    )
}
fn image(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> ImageBuffer {
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| pixel(x, y).map(f16::from_f32))
        .collect();
    ImageBuffer::new(width, height, pixels).unwrap()
}
fn grid() -> impl Iterator<Item = (f32, f32)> {
    let steps = [-0.9, -0.5, -0.2, 0.0, 0.3, 0.6, 0.9];
    steps
        .into_iter()
        .flat_map(move |t| steps.into_iter().map(move |q| (t, q)))
}

#[test]
fn recovers_temperature_and_tint_from_a_tinted_gray() {
    for (t, q) in grid() {
        for level in [0.02, 0.18, 0.8] {
            let picked = balance_for_color(tinted_gray(t, q, level)).unwrap();
            assert!(!picked.clamped);
            assert!(
                (picked.temperature - t).abs() < 2e-3 && (picked.tint - q).abs() < 2e-3,
                "({t}, {q}) level {level}: {picked:?}"
            );
        }
    }
}
#[test]
fn recovered_balance_neutralizes_the_sample() {
    for (t, q) in grid() {
        let rgb = tinted_gray(t, q, 0.3);
        let picked = balance_for_color(rgb).unwrap();
        let mut edit = EditState::default();
        edit.calibration.temperature = picked.temperature;
        edit.calibration.tint = picked.tint;
        let out = BasicAdjustments::new(&edit)
            .unwrap()
            .pixel([rgb[0], rgb[1], rgb[2], 1.0]);
        let [_, a, b] = rec2020_to_oklab([out[0], out[1], out[2]]);
        assert!(a.abs() < 2e-3 && b.abs() < 2e-3, "({t}, {q}): a {a} b {b}");
    }
}
#[test]
fn neutral_gray_needs_no_correction() {
    for level in [0.01, 0.18, 1.0, 4.0] {
        let picked = balance_for_color([level; 3]).unwrap();
        assert!(picked.temperature.abs() < 1e-3 && picked.tint.abs() < 1e-3);
        assert!(!picked.clamped);
    }
}
#[test]
fn result_is_independent_of_exposure_scale() {
    let a = balance_for_color(tinted_gray(0.4, -0.3, 0.1)).unwrap();
    let b = balance_for_color(tinted_gray(0.4, -0.3, 2.0)).unwrap();
    assert!((a.temperature - b.temperature).abs() < 2e-3);
    assert!((a.tint - b.tint).abs() < 2e-3);
}
#[test]
fn extreme_colors_are_clamped_and_flagged() {
    // Saturated orange asks for far more than the control range.
    let picked = balance_for_color([0.9, 0.3, 0.01]).unwrap();
    assert!(picked.clamped);
    assert!(picked.temperature.abs() <= 1.0 && picked.tint.abs() <= 1.0);
    assert!(picked.temperature == -1.0 || picked.tint.abs() == 1.0);
    let blue = balance_for_color([0.01, 0.05, 0.9]).unwrap();
    assert!(blue.clamped && blue.temperature == 1.0);
}
#[test]
fn unusable_samples_are_rejected() {
    assert_eq!(balance_for_color([0.0; 3]), Err(PickError::TooDark));
    assert_eq!(balance_for_color([1e-5; 3]), Err(PickError::TooDark));
    // Negative luminance is below the threshold too.
    assert_eq!(balance_for_color([-0.5; 3]), Err(PickError::TooDark));
    assert!(balance_for_color([0.01; 3]).is_ok());
    for rgb in [
        [f32::NAN, 0.5, 0.5],
        [0.5, f32::INFINITY, 0.5],
        // Positive luminance but a negative cone response.
        [1.2, -0.3, 0.9],
    ] {
        assert_eq!(balance_for_color(rgb), Err(PickError::Invalid), "{rgb:?}");
    }
}
#[test]
fn neighborhood_is_five_by_five_clipped_at_edges() {
    // Value encodes the column: x = 0, 1, 2, ... in red.
    let ramp = image(9, 9, |x, _| [x as f32, 0.5, 0.5, 1.0]);
    assert_eq!(neighborhood_average(&ramp, [4, 4]).unwrap()[0], 4.0);
    // Columns 0..=2 only: mean 1.
    assert_eq!(neighborhood_average(&ramp, [0, 4]).unwrap()[0], 1.0);
    // Columns 6..=8 only: mean 7.
    assert_eq!(neighborhood_average(&ramp, [8, 0]).unwrap()[0], 7.0);
    // A pixel three columns away is outside the window.
    let spot = image(9, 9, |x, y| {
        [if (x, y) == (7, 4) { 9.0 } else { 0.0 }, 0.0, 0.0, 1.0]
    });
    assert_eq!(neighborhood_average(&spot, [4, 4]).unwrap()[0], 0.0);
    assert!((neighborhood_average(&spot, [5, 4]).unwrap()[0] - 9.0 / 25.0).abs() < 1e-4);
    assert_eq!(
        neighborhood_average(&ramp, [9, 0]),
        Err(PickError::Invalid),
        "outside the image"
    );
}
#[test]
fn neighborhood_weights_by_alpha_and_skips_unusable_pixels() {
    // Opaque red next to quarter-alpha blue: the weights are 1 and 0.25.
    let mixed = image(2, 1, |x, _| {
        if x == 0 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 0.0, 1.0, 0.25]
        }
    });
    let [r, g, b] = neighborhood_average(&mixed, [0, 0]).unwrap();
    assert!((r - 0.8).abs() < 1e-3 && g == 0.0 && (b - 0.2).abs() < 1e-3);

    // Transparent and nonfinite pixels, even with finite-looking colors, add nothing.
    let skipped = image(3, 1, |x, _| match x {
        0 => [0.5, 0.25, 0.125, 1.0],
        1 => [9.0, 9.0, 9.0, 0.0],
        _ => [f32::NAN, 1.0, 1.0, 1.0],
    });
    assert_eq!(
        neighborhood_average(&skipped, [1, 0]).unwrap(),
        [0.5, 0.25, 0.125]
    );

    let clear = image(3, 3, |_, _| [0.5, 0.5, 0.5, 0.0]);
    assert_eq!(
        neighborhood_average(&clear, [1, 1]),
        Err(PickError::Transparent)
    );
    let broken = image(3, 3, |_, _| [f32::NAN, 0.5, 0.5, 1.0]);
    assert_eq!(
        neighborhood_average(&broken, [1, 1]),
        Err(PickError::Invalid)
    );
}
#[test]
fn picking_an_image_averages_then_balances() {
    let rgb = tinted_gray(-0.4, 0.5, 0.25);
    let tinted = image(8, 8, |_, _| [rgb[0], rgb[1], rgb[2], 1.0]);
    let picked = pick_white_balance(&tinted, [3, 3]).unwrap();
    assert!((picked.temperature + 0.4).abs() < 3e-3 && (picked.tint - 0.5).abs() < 3e-3);
    let clear = image(8, 8, |_, _| [rgb[0], rgb[1], rgb[2], 0.0]);
    assert_eq!(
        pick_white_balance(&clear, [3, 3]),
        Err(PickError::Transparent)
    );
}
