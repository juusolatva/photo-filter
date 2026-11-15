use filter_core::{
    edit::*,
    pipeline::{FilterOp, KnownOp, ScaleClass},
    preset::*,
};
use serde_json::{Value, json};

const PARTIAL: &str = include_str!("fixtures/v1-partial.pfpreset.json");
const FUTURE_NODE: &str = include_str!("fixtures/v1-future-node.pfpreset.json");
fn patch(raw: Value) -> PresetPatch {
    serde_json::from_value(raw).unwrap()
}

#[test]
fn neutral_defaults_and_required_algorithm_version() {
    let edit = EditState::default();
    edit.validate().unwrap();
    assert_eq!(edit.crop, CropRect([0.0, 0.0, 1.0, 1.0]));
    assert_eq!(edit.calibration.exposure, 0.0);
    assert_eq!(
        edit.color.scalars.channel_mixer,
        [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]
    );
    assert_eq!(edit.curves.master, Curve(vec![[0., 0.], [1., 1.]]));
    let value = serde_json::to_value(&edit).unwrap();
    assert_eq!(value["process_version"], 1);
    assert_eq!(serde_json::from_value::<EditState>(value).unwrap(), edit);
    assert!(serde_json::from_value::<EditState>(json!({})).is_err());
    assert!(serde_json::from_value::<EditState>(json!({"process_version":2})).is_err());
    assert!(serde_json::from_value::<EditState>(json!({"process_version":0})).is_err());
    let minimal: EditState = serde_json::from_value(json!({"process_version":1})).unwrap();
    assert_eq!(minimal, edit);
}

#[test]
fn null_and_absence_leave_values_while_zero_replaces() {
    let mut edit = EditState::default();
    edit.calibration.exposure = 1.5;
    edit.calibration.temperature = 0.4;
    edit.apply(&patch(json!({"calibration":null})), &Sections::ALL)
        .unwrap();
    edit.apply(
        &patch(json!({"calibration":{"exposure":null}})),
        &Sections::ALL,
    )
    .unwrap();
    assert_eq!(edit.calibration.exposure, 1.5);
    edit.apply(
        &patch(json!({"calibration":{"exposure":0.0}})),
        &Sections::ALL,
    )
    .unwrap();
    assert_eq!(edit.calibration.exposure, 0.0);
    assert_eq!(edit.calibration.temperature, 0.4);
    let empty = serde_json::to_value(PresetPatch::default()).unwrap();
    assert!(empty.get("calibration").is_none());
}

#[test]
fn selective_apply_and_capture_cover_every_section() {
    let changes = patch(json!({
        "calibration":{"exposure":1.0,"tint":0.3},
        "tone":{"brightness":0.2,"contrast":0.5},
        "curves":{"red":[[0.,0.1],[1.,0.9]]},
        "color":{"vibrance":0.4,"monochrome":true},
        "detail":{"sharpening_amount":0.6},
        "finishing":{"grain_amount":0.2}
    }));
    let mut edited = EditState::default();
    edited.apply(&changes, &Sections::ALL).unwrap();
    for index in 0..7 {
        let mut selected = Sections::default();
        match index {
            0 => selected.calibration = true,
            1 => selected.tone = true,
            2 => selected.curves = true,
            3 => selected.color = true,
            4 => selected.detail = true,
            5 => selected.finishing = true,
            _ => selected.custom_stack = true,
        }
        let captured = PresetPatch::from_edit(&edited, &selected);
        let mut applied = EditState::default();
        applied.apply(&captured, &Sections::ALL).unwrap();
        let mut expected = EditState::default();
        expected.apply(&changes, &selected).unwrap();
        assert_eq!(applied, expected);
        let values = serde_json::to_value(captured).unwrap();
        for (i, field) in [
            "calibration",
            "tone",
            "curves",
            "color",
            "detail",
            "finishing",
            "custom_stack",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(values.get(field).is_some(), index == i);
        }
    }
    let before = edited.clone();
    edited
        .apply(&PresetPatch::default(), &Sections::ALL)
        .unwrap();
    edited.apply(&changes, &Sections::default()).unwrap();
    assert_eq!(edited, before);
}

#[test]
fn curves_replace_only_present_channels() {
    let mut edit = EditState::default();
    edit.curves.blue = Curve(vec![[0., 0.2], [1., 0.8]]);
    let blue = edit.curves.blue.clone();
    edit.apply(
        &patch(json!({"curves":{"master":[[0.,0.05],[0.5,0.6],[1.,1.2]]}})),
        &Sections::ALL,
    )
    .unwrap();
    assert_eq!(edit.curves.blue, blue);
    assert_eq!(edit.curves.red, Curve::default());
    assert_eq!(edit.curves.master.0.len(), 3);
    assert_eq!(edit.curves.master.0[2][1], 1.2);
}

#[test]
fn color_merges_scalars_and_replaces_selected_bands_and_wheels() {
    let mut edit = EditState::default();
    edit.color.scalars.saturation = 0.7;
    edit.color.hsl[1].saturation = 0.2;
    edit.color.hsl[0].lightness = 0.6;
    edit.color.grading[2].chroma = 0.4;
    edit.apply(
        &patch(json!({"color":{
            "vibrance":0.5,
            "hsl":[{"hue":0.1},null,null,null,null,null,null,null],
            "grading":[null,{"chroma":0.3},null]
        }})),
        &Sections::ALL,
    )
    .unwrap();
    assert_eq!(edit.color.scalars.saturation, 0.7);
    assert_eq!(edit.color.scalars.vibrance, 0.5);
    assert_eq!(
        edit.color.hsl[0],
        HslBand {
            hue: 0.1,
            ..Default::default()
        }
    );
    assert_eq!(edit.color.hsl[1].saturation, 0.2);
    assert_eq!(edit.color.grading[1].chroma, 0.3);
    assert_eq!(edit.color.grading[2].chroma, 0.4);
}

#[test]
fn schema_fixture_loads_and_round_trips_without_appearance_changes() {
    let preset = Preset::from_json(PARTIAL).unwrap();
    assert_eq!(
        Preset::from_json(&preset.to_json().unwrap()).unwrap(),
        preset
    );
    let mut edit = EditState::default();
    edit.calibration.temperature = 0.5;
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    assert_eq!(edit.calibration.exposure, 0.15);
    assert_eq!(edit.calibration.temperature, 0.5);
    assert_eq!(edit.tone.contrast, 0.12);
    assert_eq!(edit.curves.master.0.len(), 4);
    assert_eq!(edit.custom_stack.len(), 1);
    assert_eq!(edit.custom_stack[0].opacity, 0.4);
    let FilterOp::Known(op) = &edit.custom_stack[0].op else {
        panic!("expected typed grain")
    };
    assert_eq!(op.scale(), ScaleClass::Pixel);
    assert!(matches!(op, KnownOp::FilmGrain { .. }));
}

#[test]
fn unknown_operation_payload_survives_load_save_and_apply() {
    let preset = Preset::from_json(FUTURE_NODE).unwrap();
    let original: Value = serde_json::from_str(FUTURE_NODE).unwrap();
    let saved: Value = serde_json::from_str(&preset.to_json().unwrap()).unwrap();
    assert_eq!(
        saved["patch"]["custom_stack"][0]["op"],
        original["patch"]["custom_stack"][0]["op"]
    );
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    let node = &edit.custom_stack[0];
    assert!(node.enabled);
    assert!(!node.effective_enabled());
    assert_eq!(node.op, preset.patch.custom_stack.as_ref().unwrap()[0].op);
}

#[test]
fn stack_append_replace_clear_and_repeated_apply_use_fresh_ids() {
    let preset = Preset::from_json(FUTURE_NODE).unwrap();
    let original_id = preset.patch.custom_stack.as_ref().unwrap()[0].id;
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    let first_id = edit.custom_stack[0].id;
    assert_ne!(first_id, original_id);
    let mut append = preset.patch.clone();
    append.custom_stack_mode = StackMode::Append;
    edit.apply(&append, &Sections::ALL).unwrap();
    edit.apply(&append, &Sections::ALL).unwrap();
    assert_eq!(edit.custom_stack.len(), 3);
    let ids: std::collections::HashSet<_> = edit.custom_stack.iter().map(|n| n.id).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(edit.custom_stack[0].id, first_id);
    edit.apply(&preset.patch, &Sections::ALL).unwrap();
    assert_eq!(edit.custom_stack.len(), 1);
    assert_ne!(edit.custom_stack[0].id, first_id);
    edit.apply(&patch(json!({"custom_stack":[]})), &Sections::ALL)
        .unwrap();
    assert!(edit.custom_stack.is_empty());
}

#[test]
fn invalid_selected_parameters_fail_atomically_and_unselected_are_ignored() {
    let mut edit = EditState::default();
    edit.calibration.exposure = 0.5;
    let before = edit.clone();
    for bad in [
        patch(json!({"calibration":{"exposure":6.0},"tone":{"contrast":0.7}})),
        patch(json!({"curves":{"master":[[0.,0.],[0.,0.5],[1.,1.]]}})),
        patch(json!({"curves":{"red":[[0.1,0.],[1.,1.]]}})),
        patch(json!({"curves":{"blue":[]}})),
        PresetPatch {
            tone: Some(TonePatch {
                brightness: Some(f32::NAN),
                ..Default::default()
            }),
            ..Default::default()
        },
    ] {
        assert!(edit.apply(&bad, &Sections::ALL).is_err());
        assert_eq!(edit, before);
        edit.apply(&bad, &Sections::default()).unwrap();
    }
    edit.crop = CropRect([0., 0., 0.5, 1.]);
    assert!(edit.validate().is_err());
}

#[test]
fn versions_are_explicit_and_future_schema_or_process_is_rejected() {
    for field in ["schema_version", "process_version"] {
        for version in [json!(0), json!(2), json!(1.5), json!("1"), Value::Null] {
            let mut raw: Value = serde_json::from_str(PARTIAL).unwrap();
            raw[field] = version;
            assert!(
                Preset::from_json(&raw.to_string()).is_err(),
                "{field}: {raw}"
            );
        }
        let mut raw: Value = serde_json::from_str(PARTIAL).unwrap();
        raw.as_object_mut().unwrap().remove(field);
        assert!(Preset::from_json(&raw.to_string()).is_err());
    }
}

#[test]
fn malformed_known_nodes_are_errors_and_unknown_nodes_are_not_misclassified() {
    for op in [
        json!({"type":"Levels"}),
        json!({"type":null}),
        json!([]),
        json!({"type":"FilmGrain","amount":0.1,"size":-1.,"roughness":0.5}),
    ] {
        let mut raw: Value = serde_json::from_str(PARTIAL).unwrap();
        raw["patch"]["custom_stack"][0]["op"] = op;
        assert!(Preset::from_json(&raw.to_string()).is_err());
    }
    let mut preset = Preset::from_json(PARTIAL).unwrap();
    preset.patch.custom_stack.as_mut().unwrap()[0].opacity = f32::INFINITY;
    assert!(preset.to_json().is_err());
    let mut preset = Preset::from_json(PARTIAL).unwrap();
    preset.patch.custom_stack.as_mut().unwrap()[0].op =
        FilterOp::Unknown(json!({"type":"FilmGrain"}));
    assert!(preset.to_json().is_err());
}

#[test]
fn capture_full_preset_restores_all_parameters_except_node_identity() {
    let mut edit = EditState::default();
    Preset::from_json(PARTIAL)
        .unwrap()
        .apply_to(&mut edit, &Sections::ALL)
        .unwrap();
    edit.color.hsl[4].lightness = 0.12;
    edit.detail.sharpening_radius = 2.3;
    edit.finishing.vignette_amount = 0.4;
    let preset = Preset::capture("Round trip", &edit, &Sections::ALL).unwrap();
    let loaded = Preset::from_json(&preset.to_json().unwrap()).unwrap();
    let mut restored = EditState::default();
    loaded.apply_to(&mut restored, &Sections::ALL).unwrap();
    assert_ne!(edit.custom_stack[0].id, restored.custom_stack[0].id);
    edit.custom_stack[0].id = restored.custom_stack[0].id;
    assert_eq!(restored, edit);
}

#[test]
fn known_pointwise_nodes_validate_and_preserve_matrix_values() {
    let mut raw: Value = serde_json::from_str(PARTIAL).unwrap();
    let op = json!({"type":"ColorMatrix3x3", "matrix":[[1.2,-0.2,0.],[0.,1.,0.],[0.,0.,1.]], "bias":[-0.1,0.,0.1]});
    raw["patch"]["custom_stack"][0]["op"] = op.clone();
    let preset = Preset::from_json(&raw.to_string()).unwrap();
    let FilterOp::Known(known) = &preset.patch.custom_stack.as_ref().unwrap()[0].op else {
        panic!("expected known matrix")
    };
    assert_eq!(known.scale(), ScaleClass::Pointwise);
    assert_eq!(serde_json::to_value(known).unwrap()["type"], op["type"]);
    assert_eq!(
        Preset::from_json(&preset.to_json().unwrap()).unwrap(),
        preset
    );
    raw["patch"]["custom_stack"][0]["op"] = json!({"type":"Levels", "in_black":0., "gamma":0., "in_white":1., "out_black":0., "out_white":1.});
    assert!(Preset::from_json(&raw.to_string()).is_err());
    raw["patch"]["custom_stack"][0]["op"]["gamma"] = json!(1.);
    let preset = Preset::from_json(&raw.to_string()).unwrap();
    assert_eq!(
        Preset::from_json(&preset.to_json().unwrap()).unwrap(),
        preset
    );
    raw["patch"]["custom_stack"][0]["op"]["in_white"] = json!(0.);
    assert!(Preset::from_json(&raw.to_string()).is_err());
}

#[test]
fn invalid_stack_rolls_back_scalar_changes_and_existing_node_ids() {
    let preset = Preset::from_json(PARTIAL).unwrap();
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    let before = edit.clone();
    let mut invalid = preset.patch.clone();
    invalid.calibration.as_mut().unwrap().exposure = Some(2.0);
    invalid.custom_stack.as_mut().unwrap()[0].opacity = -0.1;
    for mode in [StackMode::Replace, StackMode::Append] {
        invalid.custom_stack_mode = mode;
        assert!(edit.apply(&invalid, &Sections::ALL).is_err());
        assert_eq!(edit, before);
    }
    edit.color.scalars.monochrome = true;
    edit.apply(
        &patch(json!({"color":{"monochrome":false}})),
        &Sections::ALL,
    )
    .unwrap();
    assert!(!edit.color.scalars.monochrome);
}

#[test]
fn edit_deserialization_rejects_invalid_parameters_and_duplicate_node_ids() {
    for raw in [
        json!({"process_version":1,"calibration":{"exposure":9.}}),
        json!({"process_version":1,"crop":[0.,0.,0.5,1.]}),
        json!({"process_version":1,"curves":{"master":[[0.,0.],[0.,1.]]}}),
    ] {
        assert!(serde_json::from_value::<EditState>(raw).is_err());
    }
    let preset = Preset::from_json(PARTIAL).unwrap();
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    edit.custom_stack.push(edit.custom_stack[0].clone());
    assert!(edit.validate().is_err());
    assert!(serde_json::from_value::<EditState>(serde_json::to_value(edit).unwrap()).is_err());
}

#[test]
fn stack_limits_apply_to_import_append_replace_and_edit_deserialization() {
    let mut preset = Preset::from_json(FUTURE_NODE).unwrap();
    let node = preset.patch.custom_stack.as_ref().unwrap()[0].clone();
    preset.patch.custom_stack = Some(vec![node.clone(); MAX_STACK_NODES]);
    let mut edit = EditState::default();
    preset.apply_to(&mut edit, &Sections::ALL).unwrap();
    assert_eq!(edit.custom_stack.len(), MAX_STACK_NODES);
    let saved = preset.to_json().unwrap();
    Preset::from_json(&saved).unwrap();
    let before = edit.clone();
    let append = PresetPatch {
        custom_stack: Some(vec![node.clone()]),
        custom_stack_mode: StackMode::Append,
        ..Default::default()
    };
    assert!(edit.apply(&append, &Sections::ALL).is_err());
    assert_eq!(edit, before);
    // Replacing a full stack with a short one remains legal.
    let replace = PresetPatch {
        custom_stack: Some(vec![node.clone()]),
        ..Default::default()
    };
    edit.apply(&replace, &Sections::ALL).unwrap();
    assert_eq!(edit.custom_stack.len(), 1);
    preset
        .patch
        .custom_stack
        .as_mut()
        .unwrap()
        .push(node.clone());
    assert!(preset.to_json().is_err());
    let mut raw: Value = serde_json::from_str(&saved).unwrap();
    let extra = raw["patch"]["custom_stack"][0].clone();
    raw["patch"]["custom_stack"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    assert!(Preset::from_json(&raw.to_string()).is_err());
    edit.custom_stack = (0..=MAX_STACK_NODES)
        .map(|_| {
            let mut node = node.clone();
            node.id = uuid::Uuid::new_v4();
            node
        })
        .collect();
    assert!(serde_json::from_value::<EditState>(serde_json::to_value(edit).unwrap()).is_err());
}

#[test]
fn preset_input_byte_limit_is_checked_before_parsing() {
    let mut at_limit = PARTIAL.to_owned();
    at_limit.extend(std::iter::repeat_n(' ', MAX_PRESET_BYTES - at_limit.len()));
    Preset::from_json(&at_limit).unwrap();
    at_limit.push(' ');
    let error = Preset::from_json(&at_limit).unwrap_err();
    assert!(error.to_string().contains("bytes"));
}
