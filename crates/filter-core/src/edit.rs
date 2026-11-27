//! Concrete edit parameters. Basic calibration, tone (except clarity), tone curves,
//! vibrance, saturation, the HSL mixer, grading and the channel mixer or monochrome
//! render today; other sections reserve their preset contracts.
use crate::grading::MIXER_LIMIT;
use crate::pipeline::FilterNode;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Interactive stacks are bounded independently of their encoded preset size.
pub const MAX_STACK_NODES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);
impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ModelError {}

/// Pins working space and algorithms. Unknown versions must never render as v1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub enum ProcessVersion {
    #[default]
    V1,
}
impl TryFrom<u32> for ProcessVersion {
    type Error = ModelError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::V1),
            _ => Err(ModelError(format!("unsupported process version {value}"))),
        }
    }
}
impl From<ProcessVersion> for u32 {
    fn from(version: ProcessVersion) -> Self {
        match version {
            ProcessVersion::V1 => 1,
        }
    }
}

pub(crate) trait Validate {
    fn validate(&self) -> Result<(), ModelError>;
}
impl Validate for f32 {
    fn validate(&self) -> Result<(), ModelError> {
        if self.is_finite() {
            Ok(())
        } else {
            Err(ModelError("non-finite edit parameter".into()))
        }
    }
}
impl Validate for bool {
    fn validate(&self) -> Result<(), ModelError> {
        Ok(())
    }
}
impl<T: Validate, const N: usize> Validate for [T; N] {
    fn validate(&self) -> Result<(), ModelError> {
        self.iter().try_for_each(Validate::validate)
    }
}

// Keep defaults and patch fields together so adding a concrete parameter cannot
// accidentally omit its selective merge or full-preset capture behavior.
macro_rules! section {
    ($state:ident, $patch:ident { $($field:ident: $ty:ty = $default:expr),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $state { $(pub $field: $ty,)* }
        impl Default for $state {
            fn default() -> Self { Self { $($field: $default,)* } }
        }
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $patch {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            pub $field: Option<$ty>,)*
        }
        #[allow(clippy::clone_on_copy)]
        impl $state {
            pub(crate) fn merge(&mut self, patch: &$patch) {
                $(if let Some(value) = &patch.$field { self.$field = value.clone(); })*
            }
            pub(crate) fn patch(&self) -> $patch {
                $patch { $($field: Some(self.$field.clone()),)* }
            }
        }
        impl Validate for $state {
            fn validate(&self) -> Result<(), ModelError> {
                $(self.$field.validate().map_err(|e| ModelError(format!("{}.{}: {e}", stringify!($state), stringify!($field))))?;)*
                Ok(())
            }
        }
    };
}

// Temperature/tint are relative shifts from D65, rather than source-camera Kelvin.
section!(
    Calibration,
    CalibrationPatch {
        exposure: f32 = 0.0,
        temperature: f32 = 0.0,
        tint: f32 = 0.0,
    }
);
section!(
    Tone,
    TonePatch {
        brightness: f32 = 0.0,
        contrast: f32 = 0.0,
        highlights: f32 = 0.0,
        shadows: f32 = 0.0,
        whites: f32 = 0.0,
        blacks: f32 = 0.0,
        clarity: f32 = 0.0,
    }
);
section!(
    Detail,
    DetailPatch {
        lateral_ca: [f32; 2] = [0.0; 2],
        noise_reduction: f32 = 0.0,
        sharpening_amount: f32 = 0.0,
        sharpening_radius: f32 = 1.0,
        sharpening_threshold: f32 = 0.0,
    }
);
section!(
    Finishing,
    FinishingPatch {
        vignette_amount: f32 = 0.0,
        vignette_midpoint: f32 = 0.5,
        vignette_roundness: f32 = 0.0,
        vignette_feather: f32 = 0.5,
        grain_amount: f32 = 0.0,
        grain_size: f32 = 1.0,
        grain_roughness: f32 = 0.5,
    }
);

/// Channel curves replace all control points together. Endpoints are fixed in x;
/// y may extend to [`CURVE_Y_RANGE`] for HDR and out-of-gamut values. See
/// `crate::curve` for evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve(pub Vec<[f32; 2]>);
impl Default for Curve {
    fn default() -> Self {
        Self(vec![[0.0, 0.0], [1.0, 1.0]])
    }
}
/// Allowed control-point y range, in encoded sRGB units.
pub const CURVE_Y_RANGE: std::ops::RangeInclusive<f32> = -1.0..=2.0;
impl Validate for Curve {
    fn validate(&self) -> Result<(), ModelError> {
        if !(2..=1024).contains(&self.0.len())
            || self.0.first().map(|p| p[0]) != Some(0.0)
            || self.0.last().map(|p| p[0]) != Some(1.0)
        {
            return Err(ModelError(
                "curve needs 2..=1024 points with x endpoints 0 and 1".into(),
            ));
        }
        for point in &self.0 {
            point.validate()?;
            if !CURVE_Y_RANGE.contains(&point[1]) {
                return Err(ModelError(
                    "curve y coordinates must be within -1..=2".into(),
                ));
            }
        }
        if self.0.windows(2).any(|p| p[0][0] >= p[1][0]) {
            return Err(ModelError(
                "curve x coordinates must increase strictly".into(),
            ));
        }
        Ok(())
    }
}
section!(
    Curves,
    CurvesPatch {
        master: Curve = Curve::default(),
        red: Curve = Curve::default(),
        green: Curve = Curve::default(),
        blue: Curve = Curve::default(),
    }
);

/// Each present band/wheel is replaced as a unit; omitted entries are retained.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HslBand {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}
impl Validate for HslBand {
    fn validate(&self) -> Result<(), ModelError> {
        [self.hue, self.saturation, self.lightness].validate()
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradingWheel {
    pub hue: f32,
    pub chroma: f32,
    pub lightness: f32,
}
impl Validate for GradingWheel {
    fn validate(&self) -> Result<(), ModelError> {
        [self.hue, self.chroma, self.lightness].validate()
    }
}
section!(
    ColorScalars,
    ColorScalarsPatch {
        vibrance: f32 = 0.0,
        saturation: f32 = 0.0,
        channel_mixer: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        monochrome: bool = false,
        defringe: f32 = 0.0,
        grading_balance: f32 = 0.0,
    }
);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    #[serde(flatten)]
    pub scalars: ColorScalars,
    /// Red, orange, yellow, green, aqua, blue, purple, magenta.
    pub hsl: [HslBand; 8],
    /// Shadows, midtones, highlights.
    pub grading: [GradingWheel; 3],
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPatch {
    #[serde(flatten)]
    pub scalars: ColorScalarsPatch,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hsl: Option<[Option<HslBand>; 8]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grading: Option<[Option<GradingWheel>; 3]>,
}
impl Color {
    pub(crate) fn merge(&mut self, patch: &ColorPatch) {
        self.scalars.merge(&patch.scalars);
        if let Some(bands) = &patch.hsl {
            for (target, band) in self.hsl.iter_mut().zip(bands) {
                if let Some(band) = band {
                    *target = band.clone();
                }
            }
        }
        if let Some(wheels) = &patch.grading {
            for (target, wheel) in self.grading.iter_mut().zip(wheels) {
                if let Some(wheel) = wheel {
                    *target = wheel.clone();
                }
            }
        }
    }
    pub(crate) fn patch(&self) -> ColorPatch {
        ColorPatch {
            scalars: self.scalars.patch(),
            hsl: Some(self.hsl.clone().map(Some)),
            grading: Some(self.grading.clone().map(Some)),
        }
    }
}
/// Range check for a finite value, naming the offending field.
fn within(name: &str, value: f32, range: std::ops::RangeInclusive<f32>) -> Result<(), ModelError> {
    if range.contains(&value) {
        return Ok(());
    }
    let (lo, hi) = (range.start(), range.end());
    let signed = |v: &f32| {
        if *v == 0.0 {
            "0".to_string()
        } else {
            format!("{v:+}")
        }
    };
    Err(ModelError(format!(
        "{name} must be between {} and {}",
        signed(lo),
        signed(hi)
    )))
}
impl HslBand {
    fn validate_range(&self, index: usize) -> Result<(), ModelError> {
        for (field, value) in [
            ("hue", self.hue),
            ("saturation", self.saturation),
            ("lightness", self.lightness),
        ] {
            within(&format!("color.hsl[{index}].{field}"), value, -1.0..=1.0)?;
        }
        Ok(())
    }
}
impl GradingWheel {
    fn validate_range(&self, index: usize) -> Result<(), ModelError> {
        let name = |field: &str| format!("color.grading[{index}].{field}");
        within(&name("hue"), self.hue, 0.0..=1.0)?;
        within(&name("chroma"), self.chroma, 0.0..=1.0)?;
        within(&name("lightness"), self.lightness, -1.0..=1.0)
    }
}
impl Color {
    fn validate_mixer(&self) -> Result<(), ModelError> {
        let limit = MIXER_LIMIT;
        for (r, row) in self.scalars.channel_mixer.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                within(
                    &format!("color.channel_mixer[{r}][{c}]"),
                    *value,
                    -limit..=limit,
                )?;
            }
        }
        Ok(())
    }
}
impl Validate for Color {
    fn validate(&self) -> Result<(), ModelError> {
        self.scalars.validate()?;
        self.hsl.validate()?;
        self.grading.validate()?;
        self.hsl
            .iter()
            .enumerate()
            .try_for_each(|(i, band)| band.validate_range(i))?;
        self.grading
            .iter()
            .enumerate()
            .try_for_each(|(i, wheel)| wheel.validate_range(i))?;
        within(
            "color.grading_balance",
            self.scalars.grading_balance,
            -1.0..=1.0,
        )?;
        self.validate_mixer()
    }
}

/// Normalized [left, top, right, bottom]. v1 keeps the full image, reserving the
/// coordinate contract for future crop-relative vignette/mask implementations.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CropRect(pub [f32; 4]);
impl Default for CropRect {
    fn default() -> Self {
        Self([0.0, 0.0, 1.0, 1.0])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EditState {
    // Required on deserialization: silently defaulting a missing algorithm version
    // would make an unversioned sidecar look safe to render.
    pub process_version: ProcessVersion,
    #[serde(default)]
    pub crop: CropRect,
    #[serde(default)]
    pub calibration: Calibration,
    #[serde(default)]
    pub tone: Tone,
    #[serde(default)]
    pub curves: Curves,
    #[serde(default)]
    pub color: Color,
    #[serde(default)]
    pub detail: Detail,
    #[serde(default)]
    pub finishing: Finishing,
    #[serde(default)]
    pub custom_stack: Vec<FilterNode>,
}
impl<'de> Deserialize<'de> for EditState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            process_version: ProcessVersion,
            #[serde(default)]
            crop: CropRect,
            #[serde(default)]
            calibration: Calibration,
            #[serde(default)]
            tone: Tone,
            #[serde(default)]
            curves: Curves,
            #[serde(default)]
            color: Color,
            #[serde(default)]
            detail: Detail,
            #[serde(default)]
            finishing: Finishing,
            #[serde(default)]
            custom_stack: Vec<FilterNode>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let edit = Self {
            process_version: wire.process_version,
            crop: wire.crop,
            calibration: wire.calibration,
            tone: wire.tone,
            curves: wire.curves,
            color: wire.color,
            detail: wire.detail,
            finishing: wire.finishing,
            custom_stack: wire.custom_stack,
        };
        edit.validate().map_err(serde::de::Error::custom)?;
        Ok(edit)
    }
}
impl EditState {
    /// Reset only the controls currently exposed by the basic-adjustment panel.
    /// Preserve curves, clarity, grading, detail, finishing and custom nodes.
    pub fn reset_basic(&mut self) {
        self.calibration = Calibration::default();
        self.tone = Tone {
            clarity: self.tone.clarity,
            ..Tone::default()
        };
        self.color.scalars.vibrance = 0.0;
        self.color.scalars.saturation = 0.0;
    }

    /// Reset only the eight HSL mixer bands.
    pub fn reset_hsl(&mut self) {
        self.color.hsl = Default::default();
    }

    /// Reset the channel mixer to the identity and switch monochrome off.
    pub fn reset_mixer(&mut self) {
        let neutral = ColorScalars::default();
        self.color.scalars.channel_mixer = neutral.channel_mixer;
        self.color.scalars.monochrome = false;
    }

    /// Set a black-and-white response: all three mixer rows equal to `weights`
    /// (linear sRGB primaries) and monochrome on, so `Y * M == weights`.
    pub fn apply_bw_response(&mut self, weights: [f32; 3]) {
        self.color.scalars.channel_mixer = [weights; 3];
        self.color.scalars.monochrome = true;
    }

    /// Reset the three grading wheels and the balance control.
    pub fn reset_grading(&mut self) {
        self.color.grading = Default::default();
        self.color.scalars.grading_balance = 0.0;
    }

    /// Approximate in-memory size, including curve points, node names and opaque
    /// operation payloads. Used to bound retained copies such as undo history;
    /// allocator overhead and spare capacity are not counted.
    pub fn estimated_bytes(&self) -> usize {
        let curves = [
            &self.curves.master,
            &self.curves.red,
            &self.curves.green,
            &self.curves.blue,
        ]
        .iter()
        .map(|curve| std::mem::size_of_val(curve.0.as_slice()))
        .sum::<usize>();
        let nodes = self
            .custom_stack
            .iter()
            .map(|node| {
                std::mem::size_of::<FilterNode>()
                    + node.name.len()
                    + match &node.op {
                        crate::pipeline::FilterOp::Known(_) => 0,
                        crate::pipeline::FilterOp::Unknown(raw) => json_bytes(raw),
                    }
            })
            .sum::<usize>();
        std::mem::size_of::<Self>() + curves + nodes
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.crop != CropRect::default() {
            return Err(ModelError("v1 supports only the full-image crop".into()));
        }
        self.calibration.validate()?;
        if !(-5.0..=5.0).contains(&self.calibration.exposure) {
            return Err(ModelError("exposure must be between -5 and +5 EV".into()));
        }
        for value in [
            self.calibration.temperature,
            self.calibration.tint,
            self.tone.brightness,
            self.tone.contrast,
            self.tone.highlights,
            self.tone.shadows,
            self.tone.whites,
            self.tone.blacks,
            self.color.scalars.vibrance,
            self.color.scalars.saturation,
        ] {
            if !(-1.0..=1.0).contains(&value) {
                return Err(ModelError(
                    "basic adjustments must be between -1 and +1".into(),
                ));
            }
        }
        self.tone.validate()?;
        self.curves.validate()?;
        self.color.validate()?;
        self.detail.validate()?;
        self.finishing.validate()?;
        if self.custom_stack.len() > MAX_STACK_NODES {
            return Err(ModelError(format!(
                "custom stack exceeds {MAX_STACK_NODES} nodes"
            )));
        }
        let mut ids = std::collections::HashSet::new();
        for node in &self.custom_stack {
            node.validate()?;
            if !ids.insert(node.id) {
                return Err(ModelError("duplicate custom-stack node ID".into()));
            }
        }
        Ok(())
    }
}

/// Heap estimate for an opaque JSON value, walked iteratively so that values
/// built without serde_json's parse depth limit cannot overflow the stack.
fn json_bytes(root: &serde_json::Value) -> usize {
    use serde_json::Value;
    let mut total = 0;
    let mut pending = vec![root];
    while let Some(value) = pending.pop() {
        total += std::mem::size_of::<Value>();
        match value {
            Value::String(text) => total += text.len(),
            Value::Array(items) => pending.extend(items),
            Value::Object(map) => {
                for (key, item) in map {
                    total += std::mem::size_of::<String>() + key.len();
                    pending.push(item);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    total
}
