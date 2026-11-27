use super::*;
use filter_core::edit::EditState;

#[test]
fn percent_text_round_trips_and_clamps_to_the_mixer_range() {
    assert_eq!(percent_text(0.5), "50");
    assert_eq!(percent_text(-1.25), "-125");
    assert_eq!(parse_percent(" 50 "), Some(0.5));
    assert_eq!(parse_percent("62%"), Some(0.62));
    assert_eq!(parse_percent("500"), Some(2.0));
    assert_eq!(parse_percent("-500"), Some(-2.0));
    assert_eq!(parse_percent("nan"), None);
    assert_eq!(parse_percent("x"), None);
}
#[test]
fn row_sums_warn_outside_one_percent() {
    assert!(!sum_is_off(1.0));
    assert!(!sum_is_off(1.009));
    assert!(sum_is_off(1.011));
    assert!(sum_is_off(0.5));
    assert_eq!(row_sum(&[0.5, 0.25, 0.25]), 1.0);
}
#[test]
fn normalize_scales_rows_and_skips_zero_sum_or_out_of_range_rows() {
    let mut mixer = [[0.5, 0.5, 0.5], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
    assert!(normalize_rows(&mut mixer));
    for (got, want) in mixer[0].iter().zip([1.0 / 3.0; 3]) {
        assert!((got - want).abs() < 1e-7);
    }
    assert_eq!(mixer[1], [1.0, 0.0, 0.0]);
    assert_eq!(mixer[2], [0.0; 3], "a zero sum is left alone");
    assert!(!normalize_rows(&mut mixer), "second pass changes nothing");
    // A tiny sum would blow coefficients out of range: the row is kept.
    let mut wild = [[1.9, -1.8, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    assert!(!normalize_rows(&mut wild));
    assert_eq!(wild[0], [1.9, -1.8, 0.0]);
}
#[test]
fn unchecking_monochrome_with_equal_rows_resets_the_matrix() {
    let mut edit = EditState::default();
    edit.apply_bw_response([0.4, 0.55, 0.05]);
    assert!(set_monochrome(&mut edit.color.scalars, false));
    assert!(!edit.color.scalars.monochrome);
    assert_eq!(edit.color.scalars.channel_mixer, IDENTITY);
    assert!(!set_monochrome(&mut edit.color.scalars, false), "no change");
}
#[test]
fn unchecking_monochrome_keeps_an_edited_matrix_and_checking_keeps_rows() {
    let mut edit = EditState::default();
    let matrix = [[0.9, 0.2, -0.1], [0.0, 1.1, 0.0], [0.1, 0.1, 0.8]];
    edit.color.scalars.channel_mixer = matrix;
    assert!(set_monochrome(&mut edit.color.scalars, true));
    assert_eq!(edit.color.scalars.channel_mixer, matrix, "rows survive");
    assert!(set_monochrome(&mut edit.color.scalars, false));
    assert_eq!(edit.color.scalars.channel_mixer, matrix);
    // Identity rows count as distinct rows: nothing to reset either way.
    let mut identity = EditState::default();
    set_monochrome(&mut identity.color.scalars, true);
    set_monochrome(&mut identity.color.scalars, false);
    assert_eq!(identity.color.scalars.channel_mixer, IDENTITY);
}
#[test]
fn gray_weight_edits_write_all_three_rows_equal() {
    let mut mixer = IDENTITY;
    set_gray_weight(&mut mixer, 1, 0.5);
    assert_eq!(mixer[0], mixer[1]);
    assert_eq!(mixer[1], mixer[2]);
    let weights = mono_weights(&mixer);
    assert!((weights[1] - 0.5).abs() < 1e-6);
    assert!((weights[0] - 0.2126).abs() < 1e-3, "other weights are kept");
}
#[test]
fn preset_buttons_data_makes_equal_rows_that_sum_to_one() {
    for (name, weights) in BW_PRESETS {
        let mut edit = EditState::default();
        edit.apply_bw_response(weights);
        let rows = edit.color.scalars.channel_mixer;
        assert!(rows[0] == rows[1] && rows[1] == rows[2], "{name}");
        assert!(!sum_is_off(row_sum(&rows[0])), "{name}");
        edit.validate().unwrap();
    }
}
