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

#[test]
fn default_and_exposure_only_rows_keep_their_layout() {
    let neutral = BasicAdjustments::default();
    let rows = neutral.uniform_rows();
    assert_eq!(rows.len(), 16);
    assert_eq!(rows[0], [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(rows[3], [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(rows[5], [0.0; 4]);
    assert!(rows[6..].iter().flatten().all(|v| *v == 0.0));
    assert_eq!(
        neutral.pixel([0.3, -0.2, 1.7, 0.25]),
        [0.3, -0.2, 1.7, 0.25]
    );
    let exposed = BasicAdjustments::exposure(1.0).unwrap();
    assert_eq!(exposed.uniform_rows()[3][0], 2.0);
    assert_eq!(exposed.uniform_rows()[5], [0.0; 4]);
    assert!(
        exposed.uniform_rows()[6..]
            .iter()
            .flatten()
            .all(|v| *v == 0.0)
    );
}

#[test]
fn hsl_mixer_renders_and_flags_the_color_stage() {
    use filter_core::color::{linear_srgb_to_rec2020, rec2020_to_oklab};
    let mut edit = EditState::default();
    edit.color.hsl[3].saturation = -1.0;
    let green = BasicAdjustments::new(&edit).unwrap();
    let rows = green.uniform_rows();
    assert_eq!([rows[5][2], rows[15][0], rows[15][2]], [1.0, 1.0, 1.0]);
    assert_eq!(rows[5][3], 0.0, "no tone control is active");
    assert_eq!(rows[8][3], -1.0, "band 3 saturation");
    let rgb = linear_srgb_to_rec2020([0.0, 0.8, 0.0]);
    let out = green.pixel([rgb[0], rgb[1], rgb[2], 0.5]);
    let lab = rec2020_to_oklab([out[0], out[1], out[2]]);
    assert!(
        lab[1].hypot(lab[2]) < 2e-3,
        "the green band is removed: {lab:?}"
    );
    assert_eq!(out[3], 0.5);
    // Curves route the color stage through the post-curve round trip.
    edit.curves.master.0 = vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]];
    let curved = BasicAdjustments::new(&edit).unwrap();
    let out = curved.pixel([rgb[0], rgb[1], rgb[2], 1.0]);
    let lab = rec2020_to_oklab([out[0], out[1], out[2]]);
    assert!(lab[1].hypot(lab[2]) < 2e-3, "{lab:?}");
    // HSL alone does not turn on the tone round trip.
    assert_eq!(curved.uniform_rows()[5][3], 0.0);
}

#[test]
fn hsl_ranges_are_validated_and_unfinished_color_controls_stay_rejected() {
    let endpoints = [-1.0, 1.0];
    for value in endpoints {
        let mut edit = EditState::default();
        edit.color.hsl[7] = filter_core::edit::HslBand {
            hue: value,
            saturation: value,
            lightness: value,
        };
        assert!(edit.validate().is_ok() && BasicAdjustments::new(&edit).is_ok());
    }
    for value in [1.0001, -1.0001, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for field in 0..3 {
            let mut edit = EditState::default();
            let band = &mut edit.color.hsl[2];
            match field {
                0 => band.hue = value,
                1 => band.saturation = value,
                _ => band.lightness = value,
            }
            assert!(edit.validate().is_err(), "{value} field {field}");
            assert!(BasicAdjustments::new(&edit).is_err());
        }
    }
    let mut edit = EditState::default();
    edit.color.hsl[1].lightness = 2.0;
    assert_eq!(
        edit.validate().unwrap_err().0,
        "color.hsl[1].lightness must be between -1 and +1"
    );
    // Everything color-related renders now; defringe and other effects do not.
    for mutate in [
        (|e: &mut EditState| e.color.scalars.defringe = 0.1) as fn(&mut EditState),
        |e| e.color.scalars.defringe = -0.1,
        |e| e.tone.clarity = 0.2,
    ] {
        let mut edit = EditState::default();
        mutate(&mut edit);
        edit.validate().unwrap();
        assert!(BasicAdjustments::new(&edit).is_err());
    }
}

#[test]
fn reset_hsl_clears_only_the_bands() {
    let mut edit = EditState::default();
    edit.color.hsl[4].hue = 0.3;
    edit.color.scalars.saturation = 0.2;
    edit.color.grading[1].chroma = 0.4;
    edit.reset_hsl();
    assert_eq!(edit.color, {
        let mut expected = filter_core::edit::Color::default();
        expected.scalars.saturation = 0.2;
        expected.grading[1].chroma = 0.4;
        expected
    });
    assert_eq!(edit.color.scalars.saturation, 0.2);
    assert_eq!(edit.color.grading[1].chroma, 0.4);
}

#[test]
fn grading_ranges_are_validated_with_inclusive_endpoints() {
    type Set = fn(&mut EditState, f32);
    let fields: [(&str, Set); 5] = [
        ("color.grading[1].hue", |e, v| e.color.grading[1].hue = v),
        ("color.grading[1].chroma", |e, v| {
            e.color.grading[1].chroma = v
        }),
        ("color.grading[2].lightness", |e, v| {
            e.color.grading[2].lightness = v
        }),
        ("color.grading[0].lightness", |e, v| {
            e.color.grading[0].lightness = v
        }),
        ("color.grading_balance", |e, v| {
            e.color.scalars.grading_balance = v
        }),
    ];
    for (name, set) in fields {
        let low = if name.ends_with("hue") || name.ends_with("chroma") {
            0.0
        } else {
            -1.0
        };
        for ok in [low, 1.0] {
            let mut edit = EditState::default();
            set(&mut edit, ok);
            assert!(edit.validate().is_ok(), "{name} {ok}");
        }
        for bad in [low - 0.001, 1.001, f32::NAN, f32::INFINITY] {
            let mut edit = EditState::default();
            set(&mut edit, bad);
            assert!(edit.validate().is_err(), "{name} {bad}");
            assert!(BasicAdjustments::new(&edit).is_err());
        }
    }
    let mut edit = EditState::default();
    edit.color.grading[1].chroma = 1.5;
    assert_eq!(
        edit.validate().unwrap_err().0,
        "color.grading[1].chroma must be between 0 and +1"
    );
}

#[test]
fn grading_flags_rows_and_only_chroma_or_lightness_activate_it() {
    let mut edit = EditState::default();
    edit.color.grading[0].hue = 0.5;
    edit.color.scalars.grading_balance = 0.5;
    let inert = BasicAdjustments::new(&edit).unwrap();
    assert_eq!(
        inert.uniform_rows(),
        BasicAdjustments::default().uniform_rows()
    );
    assert_eq!(inert.pixel([0.4, 0.3, 0.2, 1.0]), [0.4, 0.3, 0.2, 1.0]);

    edit.color.grading[2].chroma = 1.0;
    edit.color.grading[2].hue = 0.0;
    edit.color.grading[1].lightness = -1.0;
    let active = BasicAdjustments::new(&edit).unwrap();
    let rows = active.uniform_rows();
    assert_eq!(
        [rows[5][2], rows[15][0], rows[15][1], rows[15][2]],
        [1.0, 0.0, 1.0, 1.0]
    );
    assert_eq!(rows[15][3], 0.55 - 0.20 * 0.5);
    assert!((rows[14][0] - 0.10).abs() < 1e-7 && rows[14][1].abs() < 1e-7);
    assert!((rows[13][2] + 0.09).abs() < 1e-7);
    assert_eq!(rows[12], [0.0; 4]);
}

#[test]
fn grading_tints_neutrals_and_runs_after_saturation_and_curves() {
    use filter_core::color::rec2020_to_oklab;
    let mut edit = EditState::default();
    edit.color.scalars.saturation = -1.0;
    edit.color.grading[2] = filter_core::edit::GradingWheel {
        hue: 0.12,
        chroma: 1.0,
        lightness: 0.0,
    };
    for curved in [false, true] {
        if curved {
            edit.curves.master.0 = vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]];
        }
        let out = BasicAdjustments::new(&edit)
            .unwrap()
            .pixel([0.9, 0.9, 0.9, 1.0]);
        let lab = rec2020_to_oklab([out[0], out[1], out[2]]);
        assert!(
            lab[1].hypot(lab[2]) > 0.05,
            "saturation -1 cannot undo it: {lab:?}"
        );
    }
}

#[test]
fn reset_grading_clears_wheels_and_balance_only() {
    let mut edit = EditState::default();
    edit.color.grading[1].chroma = 0.4;
    edit.color.scalars.grading_balance = 0.3;
    edit.color.hsl[4].hue = 0.3;
    edit.color.scalars.saturation = 0.2;
    edit.reset_grading();
    let mut expected = filter_core::edit::Color::default();
    expected.hsl[4].hue = 0.3;
    expected.scalars.saturation = 0.2;
    assert_eq!(edit.color, expected);
    edit.color.grading[0].lightness = 0.1;
    edit.reset_basic();
    assert_eq!(
        edit.color.grading[0].lightness, 0.1,
        "reset_basic keeps grading"
    );
}

fn mixed(f: impl FnOnce(&mut EditState)) -> BasicAdjustments {
    let mut edit = EditState::default();
    f(&mut edit);
    BasicAdjustments::new(&edit).unwrap()
}

#[test]
fn identity_mixer_keeps_neutral_and_white_balance_rows_bit_exact() {
    let default = BasicAdjustments::default();
    assert_eq!(
        mixed(|e| e.color.scalars.channel_mixer = IDENTITY).uniform_rows(),
        default.uniform_rows()
    );
    let reference = mixed(|e| e.calibration.temperature = 0.4);
    let again = mixed(|e| {
        e.calibration.temperature = 0.4;
        e.color.scalars.channel_mixer = IDENTITY;
    });
    assert_eq!(reference.uniform_rows(), again.uniform_rows());
    // A non-identity mixer or monochrome changes only rows 0-2.
    for adjustments in [
        mixed(|e| e.color.scalars.channel_mixer[0][1] = 0.25),
        mixed(|e| e.color.scalars.monochrome = true),
    ] {
        assert_ne!(adjustments.uniform_rows()[..3], default.uniform_rows()[..3]);
        assert_eq!(adjustments.uniform_rows()[3..], default.uniform_rows()[3..]);
    }
}

#[test]
fn mixer_is_folded_after_white_balance_in_linear_srgb_primaries() {
    let (mixer, temperature, tint) = (
        [[0.9, 0.2, -0.1], [0.0, 1.1, 0.1], [-0.2, 0.3, 0.9]],
        0.5,
        -0.3,
    );
    let adjustments = mixed(|e| {
        e.color.scalars.channel_mixer = mixer;
        e.calibration.temperature = temperature;
        e.calibration.tint = tint;
    });
    let rows = adjustments.uniform_rows();
    let folded = std::array::from_fn(|i| [rows[i][0], rows[i][1], rows[i][2]]);
    let wb = relative_white_balance(temperature, tint);
    for c in [[0.2, 0.5, 0.1], [1.0, 0.0, 0.0], [-0.1, 0.3, 2.0]] {
        // Reference: white balance in Rec.2020, then mix in linear sRGB.
        let balanced = transform(wb, c);
        let mixed = transform(mixer, rec2020_to_linear_srgb(balanced));
        let expected = linear_srgb_to_rec2020(mixed);
        close(transform(folded, c), expected, 2e-5);
    }
}

#[test]
fn monochrome_with_identity_mixer_is_exact_relative_luminance() {
    let rows = *mixed(|e| e.color.scalars.monochrome = true).uniform_rows();
    for row in &rows[..3] {
        close(
            [row[0], row[1], row[2]],
            [0.2627002, 0.67799807, 0.059301715],
            1e-6,
        );
        assert_eq!(rows[0], *row, "rows are identical");
    }
    let gray = mixed(|e| e.color.scalars.monochrome = true).pixel([0.4, 0.4, 0.4, 0.5]);
    close([gray[0], gray[1], gray[2]], [0.4; 3], 1e-6);
    assert_eq!(gray[3], 0.5);
}

#[test]
fn monochrome_output_channels_are_identical_and_gray_through_every_stage() {
    // Linear stages (exposure, white balance, mixer rows): bit-exact.
    let linear = mixed(|e| {
        e.color.scalars.monochrome = true;
        e.color.scalars.channel_mixer = [[0.3, 0.58, 0.12]; 3];
        e.calibration.exposure = 0.4;
        e.calibration.temperature = 0.3;
    });
    // Oklab and curve round trips add rounding noise, so R, G and B agree to a few
    // ulps rather than bit for bit.
    let perceptual = mixed(|e| {
        e.color.scalars.monochrome = true;
        e.color.scalars.channel_mixer = [[0.3, 0.58, 0.12]; 3];
        e.tone.contrast = 0.3;
        e.tone.shadows = 0.4;
        e.curves.master.0[1][1] = 0.9;
        e.color.scalars.saturation = 0.2;
        e.color.scalars.vibrance = 0.3;
        e.color.hsl[0].saturation = 0.5;
    });
    for p in [
        [0.9, 0.1, 0.2, 1.0],
        [-0.3, 0.2, 5.0, 0.5],
        [0.0, 0.0, 0.0, 0.0],
        [0.02, 0.3, 0.7, 1.0],
    ] {
        let out = linear.pixel(p);
        assert!(out[0] == out[1] && out[1] == out[2], "{p:?} -> {out:?}");
        assert_eq!(out[3], p[3]);
        let out = perceptual.pixel(p);
        close(
            [out[0], out[1], out[2]],
            [out[1]; 3],
            4e-6 * (1.0 + out[1].abs()),
        );
        assert_eq!(out[3], p[3]);
    }
    // Grading still tints a mono image.
    let toned = mixed(|e| {
        e.color.scalars.monochrome = true;
        e.color.grading[1].chroma = 0.5;
    })
    .pixel([0.3, 0.3, 0.3, 1.0]);
    assert!(toned[0] != toned[2]);
}

#[test]
fn mixer_with_unit_row_sums_preserves_neutrals() {
    let adjustments = mixed(|e| {
        e.color.scalars.channel_mixer = [[1.4, -0.3, -0.1], [-0.2, 1.5, -0.3], [0.1, -0.6, 1.5]];
    });
    for v in [0.0, 0.05, 0.4, 1.0, 3.0] {
        let out = adjustments.pixel([v, v, v, 1.0]);
        close([out[0], out[1], out[2]], [v; 3], 2e-5 * (1.0 + v));
    }
    let tinted = adjustments.pixel([0.6, 0.2, 0.1, 1.0]);
    assert!((tinted[0] - 0.6).abs() > 0.01, "colors do change");
}

#[test]
fn mixer_ranges_are_validated_with_inclusive_endpoints() {
    for value in [-2.0, 0.0, 2.0] {
        let mut edit = EditState::default();
        edit.color.scalars.channel_mixer[2][0] = value;
        edit.validate().unwrap();
        BasicAdjustments::new(&edit).unwrap();
    }
    for value in [2.0001, -2.0001, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut edit = EditState::default();
        edit.color.scalars.channel_mixer[1][2] = value;
        assert!(edit.validate().is_err(), "{value}");
        assert!(BasicAdjustments::new(&edit).is_err());
    }
    let mut edit = EditState::default();
    edit.color.scalars.channel_mixer[1][2] = 3.0;
    assert_eq!(
        edit.validate().unwrap_err().0,
        "color.channel_mixer[1][2] must be between -2 and +2"
    );
    let mut edit = EditState::default();
    edit.color.scalars.defringe = f32::NAN;
    assert!(edit.validate().is_err());
}

#[test]
fn reset_mixer_and_bw_response_touch_only_the_mixer() {
    let mut edit = EditState::default();
    edit.color.scalars.vibrance = 0.2;
    edit.color.hsl[2].hue = 0.3;
    edit.color.grading[0].chroma = 0.4;
    let others = edit.color.clone();
    edit.apply_bw_response([0.4, 0.55, 0.05]);
    assert!(edit.color.scalars.monochrome);
    assert_eq!(edit.color.scalars.channel_mixer, [[0.4, 0.55, 0.05]; 3]);
    assert_eq!(edit.color.hsl, others.hsl);
    edit.validate().unwrap();
    // Unchecking monochrome alone leaves equal rows, hence a gray image.
    edit.color.scalars.monochrome = false;
    // Equal mixer rows without monochrome give gray to rounding (not bit-exact).
    let gray = BasicAdjustments::new(&edit)
        .unwrap()
        .pixel([0.8, 0.1, 0.1, 1.0]);
    close([gray[0], gray[1], gray[2]], [gray[0]; 3], 1e-6);
    edit.reset_mixer();
    assert_eq!(edit.color, others);
    edit.color.scalars.monochrome = true;
    edit.reset_basic();
    assert!(edit.color.scalars.monochrome, "reset_basic keeps mono");
}

/// The GPU cube root is Newton-refined to within about an ulp, but the HSL hue
/// weights and chroma gate are steep. A relative 1.2e-7 error in each Oklab cube
/// root (one ulp for roots in [0.5, 1), in every sign combination) must move the
/// output by less than the GPU parity tolerance. Extreme opposed adjacent-band
/// settings measured about 1.9x the tolerance at 3e-7 (about 2.5 ulp), so parity
/// relies on the refined root.
#[test]
fn hsl_output_is_well_conditioned_against_cube_root_error() {
    let edits = conditioning_edits();
    let inputs = conditioning_inputs();
    let rel = 1.2e-7f32;
    let mut worst = Conditioning::default();
    for edit in &edits {
        let adjustments = BasicAdjustments::new(edit).unwrap();
        for rgb in &inputs {
            worst.merge(perturbation_ratio(&adjustments, *rgb, rel));
        }
    }
    let (ratio, at) = (worst.ratio, worst.at);
    println!("max cube-root conditioning ratio: {ratio} at {at}");
    assert!(ratio < 1.0, "ratio {ratio} at {at}");
}

#[derive(Default)]
struct Conditioning {
    ratio: f32,
    at: String,
}
impl Conditioning {
    fn merge(&mut self, other: Self) {
        if other.ratio > self.ratio {
            *self = other;
        }
    }
}

/// Extreme single-band edits plus seeded random all-band edits.
fn conditioning_edits() -> Vec<EditState> {
    use filter_core::edit::HslBand;
    let mut seed = 7919u32;
    let mut lcg = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
    };
    let mut edits = Vec::new();
    for band in 0..8 {
        let mut e = EditState::default();
        e.color.hsl[band] = HslBand {
            hue: 0.9,
            saturation: if band % 2 == 0 { 0.6 } else { -1.0 },
            lightness: if band % 3 == 0 { -0.7 } else { 1.0 },
        };
        edits.push(e);
    }
    for _ in 0..3 {
        let mut e = EditState::default();
        for band in &mut e.color.hsl {
            *band = HslBand {
                hue: lcg(),
                saturation: lcg(),
                lightness: lcg(),
            };
        }
        e.color.scalars.vibrance = 0.5;
        e.color.scalars.saturation = -0.3;
        e.tone.contrast = 0.5;
        edits.push(e);
    }
    edits
}

/// Hue sweeps across the chroma gate, plus gray, black, negative and HDR pixels.
fn conditioning_inputs() -> Vec<[f32; 3]> {
    let lab = |degrees: f32, chroma: f32, l: f32| {
        let (sin, cos) = degrees.to_radians().sin_cos();
        oklab_to_rec2020([l, chroma * cos, chroma * sin])
    };
    let mut inputs = Vec::new();
    for i in 0..400 {
        let x = i as f32 / 400.0;
        inputs.push(lab(x * 360.0, 0.02 + 0.2 * x, 0.6));
        inputs.push(lab(
            x * 360.0 + 3.0,
            [0.003, 0.006, 0.02, 0.045, 0.2][i % 5],
            0.2 + 0.6 * x,
        ));
    }
    inputs.extend([[0.18; 3], [0.0; 3], [-0.3, 0.2, 0.05], [4.0, 0.1, -0.2]]);
    inputs
}

/// Largest output shift over every sign combination of the cube-root error,
/// relative to the GPU parity tolerance.
fn perturbation_ratio(adjustments: &BasicAdjustments, rgb: [f32; 3], rel: f32) -> Conditioning {
    let px = [rgb[0], rgb[1], rgb[2], 1.0];
    let base = adjustments.pixel(px);
    let mut worst = Conditioning::default();
    for signs in 0..8u32 {
        let err = std::array::from_fn(|i| if signs >> i & 1 == 0 { rel } else { -rel });
        let out = adjustments.pixel_perturbed(px, err);
        for c in 0..3 {
            let ratio = (out[c] - base[c]).abs() / (2e-5 * (1.0 + base[c].abs()));
            worst.merge(Conditioning {
                ratio,
                at: format!("{rgb:?} channel {c}: {} -> {}", base[c], out[c]),
            });
        }
    }
    worst
}
