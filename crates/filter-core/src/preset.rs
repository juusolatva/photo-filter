//! Versioned preset JSON and selective, transactional patch application.
use crate::{
    edit::{
        CalibrationPatch, ColorPatch, CurvesPatch, DetailPatch, EditState, FinishingPatch,
        MAX_STACK_NODES, ModelError, ProcessVersion, TonePatch,
    },
    lut::LutSlot,
    pipeline::FilterNode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Check before parsing so opaque future operation payloads are bounded too.
pub const MAX_PRESET_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackMode {
    #[default]
    Replace,
    Append,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    pub calibration: bool,
    pub tone: bool,
    pub curves: bool,
    pub color: bool,
    pub detail: bool,
    pub finishing: bool,
    pub custom_stack: bool,
}
impl Sections {
    pub const ALL: Self = Self {
        calibration: true,
        tone: true,
        curves: true,
        color: true,
        detail: true,
        finishing: true,
        custom_stack: true,
    };
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<CalibrationPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<TonePatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curves: Option<CurvesPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<DetailPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finishing: Option<FinishingPatch>,
    pub custom_stack_mode: StackMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_stack: Option<Vec<FilterNode>>,
}
impl EditState {
    /// All changes are validated before publication; a failed patch leaves the
    /// destination untouched. Version policy is enforced by Preset::apply_to.
    pub fn apply(&mut self, patch: &PresetPatch, sections: &Sections) -> Result<(), ModelError> {
        if self.custom_stack.len() > MAX_STACK_NODES {
            return Err(ModelError(format!(
                "custom stack exceeds {MAX_STACK_NODES} nodes"
            )));
        }
        if sections.custom_stack
            && let Some(nodes) = &patch.custom_stack
        {
            let retained = if patch.custom_stack_mode == StackMode::Append {
                self.custom_stack.len()
            } else {
                0
            };
            if nodes.len() > MAX_STACK_NODES - retained {
                return Err(ModelError(format!(
                    "custom stack exceeds {MAX_STACK_NODES} nodes"
                )));
            }
        }
        let mut next = self.clone();
        macro_rules! merge {
            ($($field:ident),*) => {$(
                if sections.$field && let Some(value) = &patch.$field { next.$field.merge(value); }
            )*};
        }
        merge!(calibration, tone, curves, color, detail, finishing);
        if sections.custom_stack
            && let Some(nodes) = &patch.custom_stack
        {
            if patch.custom_stack_mode == StackMode::Replace {
                next.custom_stack.clear();
            }
            next.custom_stack
                .extend(nodes.iter().cloned().map(|mut node| {
                    node.id = Uuid::new_v4();
                    node
                }));
        }
        next.validate()?;
        *self = next;
        Ok(())
    }
}
impl PresetPatch {
    /// The schema a writer needs: a preset embedding a LUT is v2 so that readers
    /// that predate LUTs reject it instead of silently dropping the look.
    pub fn required_schema(&self) -> SchemaVersion {
        let lut = self.finishing.as_ref().and_then(|f| f.lut.as_ref());
        if matches!(lut, Some(LutSlot::Cube(_))) {
            SchemaVersion::V2
        } else {
            SchemaVersion::V1
        }
    }
    /// Capture selected sections without capturing crop or changing process version.
    pub fn from_edit(edit: &EditState, sections: &Sections) -> Self {
        Self {
            calibration: sections.calibration.then(|| edit.calibration.patch()),
            tone: sections.tone.then(|| edit.tone.patch()),
            curves: sections.curves.then(|| edit.curves.patch()),
            color: sections.color.then(|| edit.color.patch()),
            detail: sections.detail.then(|| edit.detail.patch()),
            finishing: sections.finishing.then(|| edit.finishing.patch()),
            custom_stack_mode: StackMode::Replace,
            custom_stack: sections.custom_stack.then(|| edit.custom_stack.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub enum SchemaVersion {
    #[default]
    V1,
    /// Adds the Finishing LUT (`finishing.lut` of kind `cube`).
    V2,
}
impl TryFrom<u32> for SchemaVersion {
    type Error = ModelError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            _ => Err(ModelError(format!(
                "unsupported preset schema version {value}"
            ))),
        }
    }
}
impl From<SchemaVersion> for u32 {
    fn from(version: SchemaVersion) -> Self {
        match version {
            SchemaVersion::V1 => 1,
            SchemaVersion::V2 => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub schema_version: SchemaVersion,
    pub process_version: ProcessVersion,
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub patch: PresetPatch,
}
impl Preset {
    pub fn capture(
        name: impl Into<String>,
        edit: &EditState,
        sections: &Sections,
    ) -> Result<Self, ModelError> {
        edit.validate()?;
        let patch = PresetPatch::from_edit(edit, sections);
        Ok(Self {
            schema_version: patch.required_schema(),
            process_version: edit.process_version,
            id: Uuid::new_v4(),
            name: name.into(),
            author: String::new(),
            tags: Vec::new(),
            patch,
        })
    }
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        if json.len() > MAX_PRESET_BYTES {
            return Err(ModelError(format!(
                "preset exceeds {MAX_PRESET_BYTES} bytes"
            )));
        }
        let raw: Value = serde_json::from_str(json).map_err(|e| ModelError(e.to_string()))?;
        let preset: Self = match migrate(&raw)? {
            Some(migrated) => serde_json::from_value(migrated),
            // Current schema: parse the text again so duplicate keys, which the
            // `Value` form silently collapses, are rejected.
            None => serde_json::from_str(json),
        }
        .map_err(|e| ModelError(e.to_string()))?;
        preset.validate()?;
        Ok(preset)
    }
    pub fn to_json(&self) -> Result<String, ModelError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|e| ModelError(e.to_string()))
    }
    fn validate(&self) -> Result<(), ModelError> {
        if self.schema_version == SchemaVersion::V1
            && self.patch.required_schema() != self.schema_version
        {
            return Err(ModelError(
                "a preset with an embedded LUT needs schema version 2".into(),
            ));
        }
        EditState::default().apply(&self.patch, &Sections::ALL)
    }
    pub fn apply_to(&self, edit: &mut EditState, sections: &Sections) -> Result<(), ModelError> {
        if self.process_version != edit.process_version {
            return Err(ModelError("preset and edit process versions differ".into()));
        }
        edit.apply(&self.patch, sections)
    }
}

/// Raw-value migration boundary. v1 is the first published schema, so there is
/// deliberately no invented v0 conversion. Add ordered vN -> vN+1 steps here as
/// schemas are introduced, before deserializing their concrete Rust types.
/// Returns `None` when the value is already in the current shape. v1 -> v2 only
/// adds the LUT, so it is an identity step plus the rule that a v1 file cannot
/// carry one.
fn migrate(raw: &Value) -> Result<Option<Value>, ModelError> {
    match raw.get("schema_version").and_then(Value::as_u64) {
        Some(1) => reject_v1_lut(raw).map(|()| None),
        Some(2) => Ok(None),
        Some(version) => Err(ModelError(format!(
            "unsupported preset schema version {version}"
        ))),
        None => Err(ModelError("preset needs an integer schema_version".into())),
    }
}
fn reject_v1_lut(raw: &Value) -> Result<(), ModelError> {
    let lut = raw.pointer("/patch/finishing/lut").filter(|l| !l.is_null());
    match lut.map(|l| l.get("kind").and_then(Value::as_str)) {
        None | Some(Some("none")) => Ok(()),
        Some(_) => Err(ModelError(
            "a schema version 1 preset cannot embed a LUT".into(),
        )),
    }
}
