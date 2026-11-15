use filter_core::{color::*, cpu_eval::BasicAdjustments, edit::EditState};

fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) {
    for i in 0..3 {
        assert!((a[i] - b[i]).abs() < tolerance, "{a:?} != {b:?}");
    }
}
#[test]
fn conversions_match_reference_colors_and_preserve_extended_range() {
    close(rec2020_to_oklab([1.0; 3]), [1.0, 0.0, 0.0], 2e-6);
    // Independent reference from the Oklab author's linear-sRGB red example.
    close(
        rec2020_to_oklab(linear_srgb_to_rec2020([1.0, 0.0, 0.0])),
        [0.6279554, 0.2248631, 0.1258463],
        2e-6,
    );
    for c in [
        [0.0; 3],
        [1.0; 3],
        [-2.0, -0.1, 0.5],
        [4.0, 0.3, -1.0],
        [0.0, 1.0, 0.0],
    ] {
        close(xyz_to_rec2020(rec2020_to_xyz(c)), c, 3e-6);
        close(oklab_to_rec2020(rec2020_to_oklab(c)), c, 1e-5);
        close(linear_srgb_to_rec2020(rec2020_to_linear_srgb(c)), c, 1e-5);
        close(linear_p3_to_rec2020(rec2020_to_linear_p3(c)), c, 3e-6);
        let lab = rec2020_to_oklab(c);
        close(oklch_to_oklab(oklab_to_oklch(lab)), lab, 2e-6);
    }
}
#[test]
fn cat16_maps_white_to_the_selected_relative_chromaticity() {
    assert_eq!(relative_white_balance(0.0, 0.0), IDENTITY);
    for temperature in [-1.0, 0.0, 1.0] {
        for tint in [-1.0, 0.0, 1.0] {
            let xyz = rec2020_to_xyz(transform(
                relative_white_balance(temperature, tint),
                [1.0; 3],
            ));
            let total: f32 = xyz.iter().sum();
            close(
                [xyz[0] / total, xyz[1] / total, xyz[1]],
                [
                    0.3127 + 0.04 * temperature + 0.01 * tint,
                    0.329 + 0.04 * temperature - 0.025 * tint,
                    1.0,
                ],
                2e-6,
            );
        }
    }
    let warm = transform(relative_white_balance(1.0, 0.0), [1.0; 3]);
    assert!(warm[0] > warm[2]);
    let magenta = transform(relative_white_balance(0.0, 1.0), [1.0; 3]);
    assert!(magenta[0] > magenta[1] && magenta[2] > magenta[1]);
}
#[test]
fn neutral_is_exact_and_parallel_evaluation_keeps_alpha() {
    let pixels = vec![[-0.5, 0.125, 3.0, 0.0], [0.0, 1.0, 0.25, 0.5]];
    let mut actual = pixels.clone();
    BasicAdjustments::default().apply(&mut actual);
    assert_eq!(actual, pixels);
    BasicAdjustments::exposure(1.0).unwrap().apply(&mut actual);
    for (p, q) in pixels.iter().zip(actual) {
        assert_eq!(q, [p[0] * 2.0, p[1] * 2.0, p[2] * 2.0, p[3]]);
    }
}
#[test]
fn perceptual_controls_change_their_intended_properties() {
    let neutral = EditState::default();
    let sample = [0.125, 0.125, 0.125, 0.5];
    let mut edit = neutral.clone();
    edit.tone.brightness = 1.0;
    let bright = BasicAdjustments::new(&edit).unwrap();
    assert!(bright.pixel(sample)[0] > sample[0]);
    for v in [0.0, 1.0] {
        close(
            bright.pixel([v, v, v, 0.3])[..3].try_into().unwrap(),
            [v; 3],
            2e-6,
        );
    }
    for amount in [-1.0, 1.0] {
        edit = neutral.clone();
        edit.tone.contrast = amount;
        let p = BasicAdjustments::new(&edit).unwrap();
        for l in [0.25_f32, 0.75] {
            let rgb = oklab_to_rec2020([l, 0.0, 0.0]);
            let out = p.pixel([rgb[0], rgb[1], rgb[2], 1.0]);
            let result = rec2020_to_oklab(out[..3].try_into().unwrap())[0];
            assert!((result - l) * (l - 0.5) * amount > 0.0);
        }
    }
    edit = neutral.clone();
    edit.color.scalars.saturation = -1.0;
    let out = BasicAdjustments::new(&edit)
        .unwrap()
        .pixel([0.7, 0.2, 0.1, 0.0]);
    close([out[0], out[1], out[2]], [out[0]; 3], 2e-6);
    assert_eq!(out[3], 0.0);
    let lab = [0.6, 0.04, 0.03];
    let rgb = oklab_to_rec2020(lab);
    edit = neutral;
    edit.color.scalars.vibrance = 1.0;
    let out = BasicAdjustments::new(&edit)
        .unwrap()
        .pixel([rgb[0], rgb[1], rgb[2], 1.0]);
    let out = rec2020_to_oklab(out[..3].try_into().unwrap());
    assert!((out[0] - lab[0]).abs() < 2e-6);
    assert!((out[2] / out[1] - lab[2] / lab[1]).abs() < 1e-4);
    assert!(out[1] > lab[1]);
}
#[test]
fn renderer_rejects_invalid_controls_and_unimplemented_active_effects() {
    for value in [f32::NAN, f32::INFINITY, 1.01, -1.01] {
        let mut edit = EditState::default();
        edit.calibration.temperature = value;
        assert!(BasicAdjustments::new(&edit).is_err());
    }
    let mut edit = EditState::default();
    edit.tone.clarity = 0.1;
    assert!(BasicAdjustments::new(&edit).is_err());
    edit = EditState::default();
    edit.curves.master.0[1][1] = 0.9;
    assert!(BasicAdjustments::new(&edit).is_err());
}
