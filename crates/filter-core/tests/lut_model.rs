mod common;
use common::*;
use filter_core::{
    cpu_eval::BasicAdjustments,
    edit::*,
    lut::{CubeLut, LutSlot},
    preset::*,
};
use serde_json::{Value, json};
use std::sync::Arc;

const IDENTITY2: &str = include_str!("fixtures/lut_identity2.pfpreset.json");
const MATTE17: &str = include_str!("fixtures/lut_matte17.pfpreset.json");
const DOMAIN: &str = include_str!("fixtures/lut_domain.pfpreset.json");
const V1_WITH_CUBE: &str = include_str!("fixtures/lut_v1_with_cube.pfpreset.json");
const CORRUPT: &str = include_str!("fixtures/lut_corrupt.pfpreset.json");

fn finishing_only() -> Sections {
    Sections {
        finishing: true,
        ..Default::default()
    }
}
fn lut_of(edit: &EditState) -> &Arc<CubeLut> {
    edit.finishing.lut.cube().expect("a LUT")
}
fn patch(raw: Value) -> PresetPatch {
    serde_json::from_value(raw).unwrap()
}
fn load(json: &str) -> EditState {
    let mut edit = EditState::default();
    Preset::from_json(json)
        .unwrap()
        .apply_to(&mut edit, &Sections::ALL)
        .unwrap();
    edit
}

#[test]
fn decode_fixtures_restore_known_lattices() {
    let identity = load(IDENTITY2);
    assert_eq!(
        lut_of(&identity).as_ref(),
        &identity_with_title(2, "Identity 2")
    );
    assert_eq!(identity.finishing.lut_strength, 1.0);

    let matte = load(MATTE17);
    let lut = lut_of(&matte);
    assert_eq!((lut.size(), lut.title()), (17, "Matte 17"));
    assert_eq!(matte.finishing.lut_strength, 0.6);
    let mid = lut.node([8, 8, 8]);
    assert!((mid[0] - 0.5).abs() < 2e-4, "{mid:?}");
    assert!((lut.node([0, 0, 0])[1] - 0.05).abs() < 1e-4);

    let wide = load(DOMAIN);
    let lut = lut_of(&wide);
    assert_eq!((lut.domain_min(), lut.domain_max()), ([-0.5; 3], [1.5; 3]));
    assert_eq!(lut.node([1, 0, 1]), [1.5, -0.5, 1.5]);
}
fn identity_with_title(n: usize, title: &str) -> CubeLut {
    let id = identity(n);
    CubeLut::new(
        n,
        title,
        id.domain_min(),
        id.domain_max(),
        id.texels().to_vec(),
    )
    .unwrap()
}

#[test]
fn malformed_fixtures_are_rejected_with_a_message_naming_the_lut() {
    let error = Preset::from_json(CORRUPT).unwrap_err().to_string();
    assert!(error.contains("LUT"), "{error}");
    let error = Preset::from_json(V1_WITH_CUBE).unwrap_err().to_string();
    assert!(
        error.contains("LUT") && error.contains("version 1"),
        "{error}"
    );
}

#[test]
fn schema_version_is_two_only_when_a_lut_is_embedded() {
    let plain = Preset::capture("plain", &EditState::default(), &Sections::ALL).unwrap();
    assert_eq!(plain.schema_version, SchemaVersion::V1);
    let json: Value = serde_json::from_str(&plain.to_json().unwrap()).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["patch"]["finishing"]["lut"], json!({"kind": "none"}));
    assert_eq!(json["patch"]["finishing"]["lut_strength"], 1.0);

    let edit = with_lut(identity(2), 0.5);
    let preset = Preset::capture("look", &edit, &Sections::ALL).unwrap();
    assert_eq!(preset.schema_version, SchemaVersion::V2);
    let json: Value = serde_json::from_str(&preset.to_json().unwrap()).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["patch"]["finishing"]["lut"]["kind"], "cube");
    // Selecting other sections keeps the preset at v1.
    let other = Sections {
        tone: true,
        ..Default::default()
    };
    let without = Preset::capture("tone", &edit, &other).unwrap();
    assert_eq!(without.schema_version, SchemaVersion::V1);
    assert!(without.patch.finishing.is_none());
    // A v1 label cannot carry a LUT.
    let mut wrong = preset.clone();
    wrong.schema_version = SchemaVersion::V1;
    assert!(wrong.to_json().is_err());
    // v2 without a LUT is still readable.
    let mut raw: Value = serde_json::from_str(&plain.to_json().unwrap()).unwrap();
    raw["schema_version"] = json!(2);
    Preset::from_json(&raw.to_string()).unwrap();
}

#[test]
fn readers_that_predate_luts_reject_lut_presets_loudly() {
    // The old reader's `migrate`: only schema version 1 is known.
    let old_reader_accepts = |json: &str| {
        let raw: Value = serde_json::from_str(json).unwrap();
        raw["schema_version"].as_u64() == Some(1)
    };
    let preset = Preset::capture("look", &with_lut(identity(2), 1.0), &Sections::ALL).unwrap();
    assert!(!old_reader_accepts(&preset.to_json().unwrap()));
    let plain = Preset::capture("plain", &EditState::default(), &Sections::ALL).unwrap();
    assert!(old_reader_accepts(&plain.to_json().unwrap()));
    let mut raw: Value = serde_json::from_str(IDENTITY2).unwrap();
    raw["schema_version"] = json!(1);
    assert!(Preset::from_json(&raw.to_string()).is_err());
    raw["schema_version"] = json!(3);
    assert!(
        Preset::from_json(&raw.to_string())
            .unwrap_err()
            .to_string()
            .contains("unsupported preset schema version 3")
    );
    // A v1 file may state `none` or omit the key.
    let mut raw: Value = serde_json::from_str(&plain.to_json().unwrap()).unwrap();
    Preset::from_json(&raw.to_string()).unwrap();
    raw["patch"]["finishing"]
        .as_object_mut()
        .unwrap()
        .remove("lut");
    Preset::from_json(&raw.to_string()).unwrap();
    raw["patch"]["finishing"]["lut"] = Value::Null;
    Preset::from_json(&raw.to_string()).unwrap();
}

#[test]
fn full_preset_round_trip_keeps_the_exact_edit() {
    let mut edit = with_lut(identity_with_title(17, "Round trip"), 0.4);
    edit.tone.contrast = 0.2;
    let preset = Preset::capture("rt", &edit, &Sections::ALL).unwrap();
    let loaded = Preset::from_json(&preset.to_json().unwrap()).unwrap();
    assert_eq!(loaded, preset);
    let mut restored = EditState::default();
    loaded.apply_to(&mut restored, &Sections::ALL).unwrap();
    assert_eq!(restored, edit);
    assert_eq!(restored.finishing.lut_strength, 0.4);
}

#[test]
fn applying_none_clears_the_lut_and_a_missing_key_leaves_it() {
    let mut edit = with_lut(identity(2), 0.5);
    // A patch that omits `lut` (every v1 file) leaves the current LUT untouched.
    edit.apply(
        &patch(json!({"finishing":{"grain_amount":0.0}})),
        &Sections::ALL,
    )
    .unwrap();
    edit.apply(&patch(json!({"finishing":{"lut":null}})), &Sections::ALL)
        .unwrap();
    assert!(edit.finishing.lut.cube().is_some());
    // `{"kind":"none"}` clears it, and survives a serialization round trip.
    let clear = patch(json!({"finishing":{"lut":{"kind":"none"},"lut_strength":1.0}}));
    assert_eq!(clear.finishing.as_ref().unwrap().lut, Some(LutSlot::None));
    let again: PresetPatch = serde_json::from_str(&serde_json::to_string(&clear).unwrap()).unwrap();
    assert_eq!(again, clear);
    edit.apply(&again, &Sections::ALL).unwrap();
    assert_eq!(edit.finishing.lut, LutSlot::None);
    assert_eq!(edit.finishing.lut_strength, 1.0);
    assert_eq!(edit, EditState::default());
    // Capturing a LUT-free Finishing section clears the LUT when applied.
    let cleared = Preset::capture("none", &EditState::default(), &finishing_only()).unwrap();
    let mut looked = with_lut(identity(2), 0.3);
    cleared.apply_to(&mut looked, &finishing_only()).unwrap();
    assert_eq!(looked, EditState::default());
}

#[test]
fn selective_apply_respects_the_finishing_section() {
    let preset = Preset::from_json(IDENTITY2).unwrap();
    let mut with_tone = EditState::default();
    preset
        .apply_to(
            &mut with_tone,
            &Sections {
                tone: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(with_tone, EditState::default());
    let mut edit = with_lut(identity(2), 0.2);
    let before = edit.clone();
    preset.apply_to(&mut edit, &Sections::default()).unwrap();
    assert_eq!(edit, before);
    preset.apply_to(&mut edit, &finishing_only()).unwrap();
    assert_eq!(lut_of(&edit).title(), "Identity 2");
    assert_eq!(edit.finishing.lut_strength, 1.0);
}

#[test]
fn identical_luts_are_interned_and_share_one_payload() {
    let mut edit = EditState::default();
    let first = Preset::from_json(IDENTITY2).unwrap();
    first.apply_to(&mut edit, &Sections::ALL).unwrap();
    // The same loaded preset shares its payload with every edit it is applied to.
    let mut again = edit.clone();
    first.apply_to(&mut again, &Sections::ALL).unwrap();
    assert!(Arc::ptr_eq(lut_of(&again), lut_of(&edit)));
    // A second decode of the file is equal by content but a distinct allocation
    // until it is interned against the current slot.
    let mut next = edit.clone();
    let second = Preset::from_json(IDENTITY2).unwrap();
    second.apply_to(&mut next, &Sections::ALL).unwrap();
    assert_eq!(next, edit);
    assert!(!Arc::ptr_eq(lut_of(&next), lut_of(&edit)));
    next.finishing.lut = std::mem::take(&mut next.finishing.lut).interned(&edit.finishing.lut);
    assert!(Arc::ptr_eq(lut_of(&next), lut_of(&edit)));
    assert_eq!(next, edit);
}

#[test]
fn strength_is_validated_on_every_path() {
    for bad in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        let mut edit = EditState::default();
        edit.finishing.lut_strength = bad;
        assert!(edit.validate().is_err(), "{bad}");
        assert!(BasicAdjustments::new(&edit).is_err());
    }
    for good in [0.0, 0.5, 1.0] {
        let mut edit = EditState::default();
        edit.finishing.lut_strength = good;
        edit.validate().unwrap();
    }
    let mut edit = EditState::default();
    let before = edit.clone();
    assert!(
        edit.apply(
            &patch(json!({"finishing":{"lut_strength":1.5}})),
            &Sections::ALL
        )
        .is_err()
    );
    assert_eq!(edit, before);
    let mut raw: Value = serde_json::from_str(IDENTITY2).unwrap();
    raw["patch"]["finishing"]["lut_strength"] = json!(2.0);
    assert!(Preset::from_json(&raw.to_string()).is_err());
}

#[test]
fn edit_state_serialization_round_trips_the_lut_slot() {
    let edit = with_lut(identity(3), 0.25);
    let value = serde_json::to_value(&edit).unwrap();
    assert_eq!(value["finishing"]["lut"]["kind"], "cube");
    assert_eq!(serde_json::from_value::<EditState>(value).unwrap(), edit);
    let old = json!({"process_version":1,"finishing":{"grain_amount":0.1}});
    let loaded: EditState = serde_json::from_value(old).unwrap();
    assert_eq!(loaded.finishing.lut, LutSlot::None);
    assert_eq!(loaded.finishing.lut_strength, 1.0);
}

#[test]
fn embedded_lut_limits_apply_at_the_preset_level() {
    let edit = |f: &dyn Fn(&mut Value)| {
        let mut raw: Value = serde_json::from_str(IDENTITY2).unwrap();
        f(&mut raw["patch"]["finishing"]["lut"]);
        Preset::from_json(&raw.to_string())
    };
    edit(&|_| {}).unwrap();
    for (name, change) in [
        (
            "kind",
            (|l: &mut Value| l["kind"] = json!("hald")) as fn(&mut Value),
        ),
        ("field", |l| l["extra"] = json!(1)),
        ("encoding", |l| l["encoding"] = json!("raw")),
        ("size", |l| l["size"] = json!(65)),
        ("size low", |l| l["size"] = json!(1)),
        ("size negative", |l| l["size"] = json!(-2)),
        ("data type", |l| l["data"] = json!(5)),
        ("base64", |l| l["data"] = json!("not base64!")),
        ("domain", |l| l["domain_max"] = json!([0, 1, 1])),
        ("huge", |l| l["data"] = json!("A".repeat(3 << 20))),
    ] {
        assert!(edit(&change).is_err(), "{name}");
    }
    let loaded = edit(&|l| l["title"] = json!("a\u{7}b\u{0}c")).unwrap();
    let lut = loaded.patch.finishing.unwrap().lut.unwrap();
    assert_eq!(lut.cube().unwrap().title(), "abc");
    let loaded = edit(&|l| l["title"] = json!("x".repeat(500))).unwrap();
    let lut = loaded.patch.finishing.unwrap().lut.unwrap();
    assert_eq!(lut.cube().unwrap().title().chars().count(), 128);
}

#[test]
fn duplicate_lut_keys_are_rejected() {
    let duplicated = IDENTITY2.replacen("\"lut\":", "\"lut\": {\"kind\":\"none\"}, \"lut\":", 1);
    assert_ne!(duplicated, IDENTITY2);
    let error = Preset::from_json(&duplicated).unwrap_err().to_string();
    assert!(error.contains("duplicate"), "{error}");
}

#[test]
fn preset_size_cap_covers_lut_presets() {
    let preset = Preset::from_json(IDENTITY2).unwrap();
    let base = preset.to_json().unwrap();
    let mut at_limit = base.clone();
    at_limit.push_str(&" ".repeat(MAX_PRESET_BYTES - base.len()));
    Preset::from_json(&at_limit).unwrap();
    at_limit.push(' ');
    assert!(
        Preset::from_json(&at_limit)
            .unwrap_err()
            .to_string()
            .contains("bytes")
    );
}

#[test]
fn estimated_bytes_count_the_lut_payload() {
    let base = EditState::default().estimated_bytes();
    let edit = with_lut(identity(33), 1.0);
    let lut = lut_of(&edit);
    let expected = std::mem::size_of::<CubeLut>() + lut.texels().len() * 2 + lut.title().len();
    assert_eq!(edit.estimated_bytes(), base + expected);
    assert!(expected >= 33 * 33 * 33 * 6);
    let large = with_lut(identity(65), 1.0);
    assert!(large.estimated_bytes() - base >= 65 * 65 * 65 * 6);
}
