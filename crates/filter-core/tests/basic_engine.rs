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
    edit.curves.master.0[1][1] = 2.5;
    assert!(BasicAdjustments::new(&edit).is_err(), "curve y beyond 2");
}

#[test]
fn tone_curves_render_between_tone_and_color() {
    use filter_core::color::{linear_srgb_to_rec2020, rec2020_to_oklab};
    let mut edit = EditState::default();
    edit.curves.red.0 = vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]];
    let curved = BasicAdjustments::new(&edit).unwrap();
    assert!(curved.curves().is_some());
    let gray = linear_srgb_to_rec2020([0.18; 3]);
    let [r, g, b, a] = curved.pixel([gray[0], gray[1], gray[2], 0.5]);
    assert_eq!(a, 0.5);
    let [_, chroma_a, _] = rec2020_to_oklab([r, g, b]);
    assert!(chroma_a > 0.02, "the red curve tints gray: {chroma_a}");

    // Saturation follows the curves, so -1 removes the tint again.
    edit.color.scalars.saturation = -1.0;
    let [r, g, b, _] = BasicAdjustments::new(&edit)
        .unwrap()
        .pixel([gray[0], gray[1], gray[2], 1.0]);
    let [_, a, b] = rec2020_to_oklab([r, g, b]);
    assert!(a.abs() < 1e-5 && b.abs() < 1e-5, "{a} {b}");

    // Tone runs first: contrast changes the input the curve sees.
    let mut toned = EditState::default();
    toned.tone.contrast = 1.0;
    toned.curves.master.0 = vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]];
    let tone_only = {
        let mut e = toned.clone();
        e.curves = Default::default();
        BasicAdjustments::new(&e).unwrap()
    };
    let curve_only = {
        let mut e = toned.clone();
        e.tone.contrast = 0.0;
        BasicAdjustments::new(&e).unwrap()
    };
    let dark = [0.05, 0.05, 0.05, 1.0];
    let expected = curve_only.pixel(tone_only.pixel(dark));
    let actual = BasicAdjustments::new(&toned).unwrap().pixel(dark);
    for c in 0..3 {
        assert!(
            (actual[c] - expected[c]).abs() < 1e-6,
            "{actual:?} vs {expected:?}"
        );
    }
    // Identity curves keep the exact neutral path.
    let neutral = BasicAdjustments::new(&EditState::default()).unwrap();
    assert!(neutral.curves().is_none());
    assert_eq!(
        neutral.pixel([0.3, -0.2, 1.7, 0.25]),
        [0.3, -0.2, 1.7, 0.25]
    );
}

#[test]
fn resetting_basic_controls_preserves_unexposed_sections() {
    let mut other = EditState::default();
    other.tone.clarity = 0.4;
    other.curves.red.0[1][1] = 0.9;
    other.color.hsl[0].saturation = 0.7;
    other.color.scalars.monochrome = true;
    other.detail.sharpening_amount = 0.5;
    other.finishing.grain_amount = 0.2;
    other.custom_stack.push(
        serde_json::from_value(serde_json::json!({
            "id":"90bccb08-a7ad-4a2f-a995-89eea097383b", "op":{"type":"FutureV7","payload":[1,2,3]}
        }))
        .unwrap(),
    );
    let mut edit = other.clone();
    edit.calibration.exposure = 1.5;
    edit.calibration.temperature = 0.5;
    edit.calibration.tint = -0.2;
    edit.tone.brightness = 0.8;
    edit.tone.contrast = -0.2;
    edit.tone.highlights = 0.3;
    edit.tone.shadows = -0.6;
    edit.tone.whites = 0.5;
    edit.tone.blacks = 0.1;
    edit.color.scalars.vibrance = 0.9;
    edit.color.scalars.saturation = -0.5;
    edit.reset_basic();
    assert_eq!(edit, other);
}
