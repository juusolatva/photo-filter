use super::{
    BasicAdjustments, EditState, Preset, PresetUi, Sections, available, capture, prepare_apply,
};

#[test]
fn superseded_load_reports_cancellation_without_staging_the_result() {
    let preset = Preset::capture("Old image", &EditState::default(), &Sections::ALL).unwrap();
    let mut panel = PresetUi {
        status: "Loading preset…".into(),
        ..Default::default()
    };
    panel.finish_load(false, Some(Ok(Box::new(preset.clone()))));
    assert!(panel.imported.is_none());
    assert_eq!(panel.apply_sections, Sections::default());
    assert!(panel.status.contains("cancelled"));
    panel.finish_load(true, Some(Ok(Box::new(preset.clone()))));
    assert_eq!(panel.imported, Some(preset));
    assert_eq!(panel.apply_sections, Sections::ALL);
}

#[test]
fn selective_apply_and_capture_preserve_omission_and_explicit_zero() {
    let mut edit = EditState::default();
    edit.calibration.exposure = 2.0;
    edit.tone.contrast = 0.5;
    let sections = Sections {
        calibration: true,
        ..Default::default()
    };
    let preset = capture(&EditState::default(), "Neutral", &sections, None).unwrap();
    let next = prepare_apply(&edit, &preset, &sections).unwrap();
    assert_eq!(next.calibration.exposure, 0.0);
    assert_eq!(next.tone.contrast, 0.5);
    assert_eq!(edit.calibration.exposure, 2.0);
    assert!(preset.patch.tone.is_none());
}

#[test]
fn unsupported_selection_is_transactional_and_can_be_deselected() {
    let mut imported = EditState::default();
    imported.calibration.exposure = 1.0;
    imported.tone.clarity = 0.5;
    let preset = Preset::capture("Future", &imported, &Sections::ALL).unwrap();
    let edit = EditState::default();
    let error = prepare_apply(&edit, &preset, &Sections::ALL).unwrap_err();
    assert!(error.0.contains("Tone"));
    assert_eq!(edit, EditState::default());
    let next = prepare_apply(
        &edit,
        &preset,
        &Sections {
            tone: false,
            ..Sections::ALL
        },
    )
    .unwrap();
    assert_eq!(next.calibration.exposure, 1.0);
    assert_eq!(next.tone.clarity, 0.0);
}

#[test]
fn merge_failures_do_not_advise_deselecting_unsupported_effects() {
    let mut preset = Preset::from_json(include_str!(
        "../../../filter-core/tests/fixtures/v1-future-node.pfpreset.json"
    ))
    .unwrap();
    let node = preset.patch.custom_stack.as_ref().unwrap()[0].clone();
    preset.patch.custom_stack = Some(vec![node; filter_core::edit::MAX_STACK_NODES]);
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    preset.patch.custom_stack_mode = filter_core::preset::StackMode::Append;
    let original = edit.clone();
    let error = prepare_apply(&edit, &preset, &Sections::ALL).unwrap_err();
    assert!(error.0.contains("custom stack exceeds"));
    assert!(!error.0.contains("Deselect"));
    assert_eq!(edit, original);
}

#[test]
fn unknown_operations_and_metadata_survive_app_apply_capture_and_disk() {
    let json = include_str!("../../../filter-core/tests/fixtures/v1-future-node.pfpreset.json");
    let mut preset = Preset::from_json(json).unwrap();
    preset.author = "Camera club".into();
    preset.tags = vec!["shared".into(), "future".into()];
    let next = prepare_apply(&EditState::default(), &preset, &Sections::ALL).unwrap();
    let saved = capture(
        &next,
        &preset.name,
        &available(&preset.patch),
        Some(&preset),
    )
    .unwrap();
    assert_ne!(saved.id, preset.id);
    assert_eq!(saved.author, preset.author);
    assert_eq!(saved.tags, preset.tags);
    assert_eq!(
        saved.patch.custom_stack.as_ref().unwrap()[0].op,
        preset.patch.custom_stack.as_ref().unwrap()[0].op
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.pfpreset.json");
    filter_io::preset::write(&path, &saved).unwrap();
    assert_eq!(filter_io::preset::read(&path).unwrap(), saved);
}

#[test]
fn saved_full_preset_restores_the_compiled_render_settings() {
    let mut edit = EditState::default();
    edit.calibration.exposure = 0.75;
    edit.calibration.temperature = -0.25;
    edit.calibration.tint = 0.1;
    edit.tone.brightness = 0.1;
    edit.tone.contrast = 0.2;
    edit.tone.highlights = -0.4;
    edit.tone.shadows = 0.3;
    edit.tone.whites = -0.1;
    edit.tone.blacks = 0.2;
    edit.color.scalars.vibrance = 0.3;
    edit.color.scalars.saturation = -0.2;
    let preset = capture(&edit, "All controls", &Sections::ALL, None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("all.pfpreset.json");
    filter_io::preset::write(&path, &preset).unwrap();
    let loaded = filter_io::preset::read(&path).unwrap();
    let restored = prepare_apply(&EditState::default(), &loaded, &Sections::ALL).unwrap();
    assert_eq!(restored, edit);
    assert_eq!(
        BasicAdjustments::new(&restored).unwrap().uniform_rows(),
        BasicAdjustments::new(&edit).unwrap().uniform_rows()
    );
}

fn lut_edit(title: &str) -> EditState {
    use filter_core::lut::{CubeLut, LutSlot};
    let texels = vec![0x3800u16; 24];
    let mut edit = EditState::default();
    let lut = CubeLut::new(2, title, [0.0; 3], [1.0; 3], texels).unwrap();
    edit.finishing.lut = LutSlot::Cube(std::sync::Arc::new(lut));
    edit.finishing.lut_strength = 0.5;
    edit
}

#[test]
fn applying_an_identical_lut_keeps_the_current_arc() {
    let current = lut_edit("Look");
    let preset = Preset::capture("Look", &lut_edit("Look"), &Sections::ALL).unwrap();
    let preset = Preset::from_json(&preset.to_json().unwrap()).unwrap();
    let next = prepare_apply(&current, &preset, &Sections::ALL).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        current.finishing.lut.cube().unwrap(),
        next.finishing.lut.cube().unwrap()
    ));
    assert_eq!(next, current);
    // A different LUT is adopted.
    let other = prepare_apply(&EditState::default(), &preset, &Sections::ALL).unwrap();
    assert_eq!(other.finishing.lut_strength, 0.5);
    assert!(other.finishing.lut.cube().is_some());
}

#[test]
fn a_lut_only_finishing_preset_applies_but_lut_with_grain_names_finishing() {
    let preset = Preset::capture("Look", &lut_edit("Look"), &Sections::ALL).unwrap();
    assert!(prepare_apply(&EditState::default(), &preset, &Sections::ALL).is_ok());
    let mut grainy = lut_edit("Look");
    grainy.finishing.grain_amount = 0.5;
    let preset = Preset::capture("Grain", &grainy, &Sections::ALL).unwrap();
    let error = prepare_apply(&EditState::default(), &preset, &Sections::ALL).unwrap_err();
    assert!(error.0.contains("Finishing"), "{}", error.0);
}
