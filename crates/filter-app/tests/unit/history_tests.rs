use super::{History, MAX_BYTES, MAX_STEPS};
use filter_core::{
    edit::EditState,
    pipeline::FilterOp,
    preset::{Preset, Sections},
};

fn exposure(ev: f32) -> EditState {
    let mut edit = EditState::default();
    edit.calibration.exposure = ev;
    edit
}

/// An edit retaining roughly `bytes` in an opaque future node.
fn heavy(bytes: usize) -> EditState {
    const PRESET: &str = r#"{"schema_version":1,"process_version":1,"name":"Heavy",
        "id":"550e8400-e29b-41d4-a716-446655440001",
        "patch":{"custom_stack":[{"id":"7f9c2b1e-3a4d-4e5f-8a6b-1c2d3e4f5a6b",
        "op":{"type":"Future"}}]}}"#;
    let mut edit = EditState::default();
    Preset::from_json(PRESET)
        .unwrap()
        .apply_to(&mut edit, &Sections::ALL)
        .unwrap();
    // Larger than a preset file may be, to exercise the history budget alone.
    let FilterOp::Unknown(raw) = &mut edit.custom_stack[0].op else {
        unreachable!("future operations stay opaque");
    };
    raw["payload"] = "x".repeat(bytes).into();
    edit
}

#[test]
fn unchanged_edits_record_nothing() {
    let mut history = History::new(&EditState::default());
    assert!(!history.commit(&EditState::default()));
    assert!(!history.can_undo() && !history.can_redo());
    assert_eq!(history.undo(&EditState::default()), None);
    assert_eq!(history.redo(&EditState::default()), None);
}

#[test]
fn undo_and_redo_walk_committed_steps() {
    let mut history = History::new(&exposure(0.0));
    assert!(history.commit(&exposure(1.0)));
    assert!(history.commit(&exposure(2.0)));
    assert_eq!(history.undo(&exposure(2.0)), Some(exposure(1.0)));
    assert_eq!(history.undo(&exposure(1.0)), Some(exposure(0.0)));
    assert_eq!(history.undo(&exposure(0.0)), None);
    assert!(history.can_redo());
    assert_eq!(history.redo(&exposure(0.0)), Some(exposure(1.0)));
    assert_eq!(history.redo(&exposure(1.0)), Some(exposure(2.0)));
    assert_eq!(history.redo(&exposure(2.0)), None);
    // Returning to a previously settled edit is itself a step.
    assert!(history.commit(&exposure(0.0)));
    assert_eq!(history.undo(&exposure(0.0)), Some(exposure(2.0)));
}

#[test]
fn unsettled_changes_are_recorded_before_undo_and_discard_redo() {
    let mut history = History::new(&exposure(0.0));
    history.commit(&exposure(1.0));
    // A drag still in progress (not yet committed) becomes its own step.
    assert_eq!(history.undo(&exposure(3.0)), Some(exposure(1.0)));
    assert_eq!(history.redo(&exposure(1.0)), Some(exposure(3.0)));
    assert_eq!(history.undo(&exposure(3.0)), Some(exposure(1.0)));
    // A new change after undo replaces the redo branch.
    assert_eq!(history.redo(&exposure(-1.0)), None);
    assert!(!history.can_redo());
    assert_eq!(history.undo(&exposure(-1.0)), Some(exposure(1.0)));
    assert_eq!(history.undo(&exposure(1.0)), Some(exposure(0.0)));
}

#[test]
fn step_count_is_bounded_by_dropping_the_oldest() {
    let mut history = History::new(&exposure(0.0));
    let steps = MAX_STEPS + 20;
    for i in 1..=steps {
        history.commit(&exposure(i as f32 * 0.01));
    }
    assert_eq!(history.retained().0, MAX_STEPS);
    let mut current = exposure(steps as f32 * 0.01);
    while let Some(previous) = history.undo(&current) {
        current = previous;
    }
    assert_eq!(current, exposure(20.0 * 0.01), "the 20 oldest are gone");
    assert_eq!(history.retained().1, MAX_STEPS);
}

#[test]
fn retained_bytes_are_bounded_across_undo_and_redo() {
    let base = EditState::default().estimated_bytes();
    let large = heavy(MAX_BYTES / 3);
    let mut history = History::new(&large);
    history.commit(&exposure(1.0));
    history.commit(&heavy(MAX_BYTES / 3 + 1));
    history.commit(&exposure(2.0));
    let (undo, redo, bytes) = history.retained();
    assert_eq!((undo, redo), (3, 0));
    assert!(bytes <= MAX_BYTES && bytes > 2 * (MAX_BYTES / 3), "{bytes}");

    // A third large step evicts the oldest large one (and the step after it,
    // until the budget fits), never exceeding the budget.
    history.commit(&heavy(MAX_BYTES / 3 + 2));
    history.commit(&exposure(3.0));
    let (_, _, bytes) = history.retained();
    assert!(bytes <= MAX_BYTES, "{bytes}");
    let mut current = exposure(3.0);
    let mut seen = Vec::new();
    while let Some(previous) = history.undo(&current) {
        seen.push(previous.clone());
        current = previous;
        assert!(history.retained().2 <= MAX_BYTES);
    }
    assert!(!seen.contains(&large), "the oldest large step was evicted");
    while let Some(next) = history.redo(&current) {
        current = next;
        assert!(history.retained().2 <= MAX_BYTES);
    }
    assert_eq!(current, exposure(3.0));

    // An edit larger than the whole budget cannot be retained once replaced.
    let mut history = History::new(&heavy(MAX_BYTES + 1));
    history.commit(&exposure(1.0));
    assert!(!history.can_undo());
    assert_eq!(history.retained(), (0, 0, 0));
    assert!(base < 4096);
}

#[test]
fn undoing_past_large_edits_evicts_the_furthest_redo() {
    let near = heavy(MAX_BYTES * 5 / 8);
    let far = heavy(MAX_BYTES * 5 / 8 + 1);
    let mut history = History::new(&exposure(0.0));
    history.commit(&near);
    history.commit(&far);
    assert_eq!(history.undo(&far), Some(near.clone()));
    // Undoing to the start would retain both large edits as redo steps.
    assert_eq!(history.undo(&near), Some(exposure(0.0)));
    let (undo, redo, bytes) = history.retained();
    assert_eq!((undo, redo), (0, 1));
    assert!(bytes <= MAX_BYTES, "{bytes}");
    assert_eq!(history.redo(&exposure(0.0)), Some(near.clone()));
    assert_eq!(history.redo(&near), None, "the furthest redo was dropped");
}

#[test]
fn a_hundred_steps_with_one_65_cubed_lut_stay_within_budget_and_keep_the_lut_step() {
    use filter_core::lut::{CubeLut, LutSlot};
    let n = 65;
    let texels = vec![0u16; 3 * n * n * n];
    let lut = std::sync::Arc::new(CubeLut::new(n, "big", [0.0; 3], [1.0; 3], texels).unwrap());
    let mut edit = EditState::default();
    let mut history = History::new(&edit);
    for i in 1..=100 {
        edit.calibration.exposure = i as f32 * 0.01;
        if i == 50 {
            edit.finishing.lut = LutSlot::Cube(lut.clone());
        }
        assert!(history.commit(&edit));
    }
    let (undo, redo, bytes) = history.retained();
    assert!(bytes <= MAX_BYTES, "{bytes}");
    assert!(undo > 0 && undo <= MAX_STEPS && redo == 0);
    // LUT steps are retained (the oldest are evicted by the byte budget), and
    // undo then redo walks through them.
    let mut live = history.undo(&edit).unwrap();
    assert_eq!(live.finishing.lut, LutSlot::Cube(lut.clone()));
    assert!(live.calibration.exposure < edit.calibration.exposure);
    live = history.redo(&live).unwrap();
    assert_eq!(live, edit);
}
