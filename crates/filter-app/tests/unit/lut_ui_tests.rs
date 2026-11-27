use super::*;
use filter_core::lut::parse_cube;

fn identity(n: usize, title: &str) -> CubeLut {
    let s = (n - 1) as f32;
    let mut text = format!("TITLE \"{title}\"\nLUT_3D_SIZE {n}\n");
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (r, g, b) = (r as f32 / s, g as f32 / s, b as f32 / s);
                text.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    parse_cube(text.as_bytes()).unwrap()
}
fn loaded(n: usize, title: &str) -> Option<Result<Box<Loaded>, String>> {
    Some(Ok(Box::new(Loaded {
        lut: identity(n, title),
        file_name: "look.cube".into(),
    })))
}

#[test]
fn percent_text_round_trips_and_clamps() {
    assert_eq!(percent(0.256), "26");
    assert_eq!(parse_percent(" 50 % "), Some(0.5));
    assert_eq!(parse_percent("250"), Some(1.0));
    assert_eq!(parse_percent("-5"), Some(0.0));
    assert_eq!(parse_percent("inf"), None);
    assert_eq!(parse_percent("x"), None);
}
#[test]
fn first_load_sets_full_strength_and_a_replacement_keeps_it() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing {
        lut_strength: 0.3,
        ..Default::default()
    };
    assert!(ui.finish_load(true, loaded(2, "A"), &mut finishing));
    assert_eq!(finishing.lut_strength, 1.0, "a first LUT starts at 100%");
    finishing.lut_strength = 0.4;
    assert!(ui.finish_load(true, loaded(3, "B"), &mut finishing));
    assert_eq!(finishing.lut_strength, 0.4);
    assert_eq!(finishing.lut.cube().unwrap().size(), 3);
}
#[test]
fn an_identical_load_keeps_the_arc_and_changes_nothing() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing::default();
    assert!(ui.finish_load(true, loaded(2, "A"), &mut finishing));
    let before = finishing.lut.cube().unwrap().clone();
    assert!(!ui.finish_load(true, loaded(2, "A"), &mut finishing));
    assert!(Arc::ptr_eq(&before, finishing.lut.cube().unwrap()));
    // A different title is a different edit.
    assert!(ui.finish_load(true, loaded(2, "B"), &mut finishing));
}
#[test]
fn failures_cancels_and_stale_results_leave_the_edit_alone() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing::default();
    let long = "x".repeat(1000);
    assert!(!ui.finish_load(true, Some(Err(long)), &mut finishing));
    assert!(ui.status.starts_with("Could not load LUT: xxx"));
    assert!(ui.status.chars().count() < 400);
    assert!(!ui.finish_load(true, Some(Err("line 7: bad".into())), &mut finishing));
    assert_eq!(ui.status, "Could not load LUT: line 7: bad");
    assert!(!ui.finish_load(true, None, &mut finishing));
    assert!(ui.status.is_empty());
    ui.loading();
    assert!(!ui.finish_load(false, loaded(2, "A"), &mut finishing));
    assert!(ui.status.is_empty(), "a stale result is dropped silently");
    assert_eq!(finishing, Finishing::default());
}
#[test]
fn remove_restores_the_default_finishing() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing::default();
    ui.finish_load(true, loaded(2, "A"), &mut finishing);
    finishing.lut_strength = 0.2;
    remove(&mut finishing);
    assert_eq!(finishing, Finishing::default());
}
#[test]
fn names_fall_back_to_the_file_then_a_placeholder() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing::default();
    ui.finish_load(true, loaded(2, ""), &mut finishing);
    let lut = finishing.lut.cube().unwrap().clone();
    assert_eq!(ui.display_name(&lut), "look.cube");
    assert_eq!(ui.display_name(&Arc::new(identity(2, ""))), "Untitled LUT");
    ui.finish_load(true, loaded(2, "Warm"), &mut finishing);
    assert_eq!(ui.display_name(finishing.lut.cube().unwrap()), "Warm");
}
#[test]
fn description_shows_size_and_only_a_non_default_domain() {
    assert_eq!(describe(&identity(17, "")), "17³");
    let text =
        "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n".to_string() + &"0 0 0\n".repeat(8);
    let lut = parse_cube(text.as_bytes()).unwrap();
    assert!(describe(&lut).starts_with("2³ · domain"));
}
#[test]
fn remove_forgets_the_file_name_and_releases_the_lut() {
    let mut ui = LutUi::default();
    let mut finishing = Finishing::default();
    ui.finish_load(true, loaded(2, ""), &mut finishing);
    let weak = Arc::downgrade(finishing.lut.cube().unwrap());
    ui.remove(&mut finishing);
    assert!(ui.named.is_none());
    assert_eq!(finishing, Finishing::default());
    assert!(
        weak.upgrade().is_none(),
        "nothing keeps the removed LUT alive"
    );
}
#[test]
fn both_actions_can_be_requested_in_one_frame() {
    let both = Actions {
        load: true,
        changed: true,
    };
    assert!(both.load && both.changed);
    assert_eq!(
        Actions::default(),
        Actions {
            load: false,
            changed: false
        }
    );
}
