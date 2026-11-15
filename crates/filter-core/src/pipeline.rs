//! Serializable custom-stack contract. Execution is added with the stack engine.
use crate::edit::{ModelError, Validate};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackPosition {
    AfterCalibration,
    AfterColor,
    #[default]
    AfterFinishing,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleClass {
    Pointwise,
    Pixel,
    Image,
}

/// Initial typed node schemas; other operations remain opaque until their schema
/// and implementation are introduced. Known malformed operations are errors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum KnownOp {
    Levels {
        in_black: f32,
        gamma: f32,
        in_white: f32,
        out_black: f32,
        out_white: f32,
    },
    ColorMatrix3x3 {
        matrix: [[f32; 3]; 3],
        bias: [f32; 3],
    },
    FilmGrain {
        amount: f32,
        size: f32,
        roughness: f32,
    },
}
impl KnownOp {
    pub fn scale(&self) -> ScaleClass {
        match self {
            Self::FilmGrain { .. } => ScaleClass::Pixel,
            _ => ScaleClass::Pointwise,
        }
    }
    fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::Levels {
                in_black,
                gamma,
                in_white,
                out_black,
                out_white,
            } => {
                [*in_black, *gamma, *in_white, *out_black, *out_white].validate()?;
                if *gamma <= 0.0 || in_white <= in_black {
                    return Err(ModelError(
                        "levels need positive gamma and increasing input bounds".into(),
                    ));
                }
            }
            Self::ColorMatrix3x3 { matrix, bias } => {
                matrix.validate()?;
                bias.validate()?;
            }
            Self::FilmGrain {
                amount,
                size,
                roughness,
            } => {
                [*amount, *size, *roughness].validate()?;
                if *amount < 0.0 || *size <= 0.0 || !(0.0..=1.0).contains(roughness) {
                    return Err(ModelError("invalid film-grain parameters".into()));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterOp {
    Known(KnownOp),
    Unknown(Value),
}
impl Serialize for FilterOp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(op) => op.serialize(serializer),
            Self::Unknown(raw) => raw.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for FilterOp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let kind = raw
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("filter operation needs a string type"))?;
        if matches!(kind, "Levels" | "ColorMatrix3x3" | "FilmGrain") {
            let op = serde_json::from_value(raw).map_err(serde::de::Error::custom)?;
            Ok(Self::Known(op))
        } else {
            Ok(Self::Unknown(raw))
        }
    }
}
fn enabled() -> bool {
    true
}
fn opacity() -> f32 {
    1.0
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterNode {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub position: StackPosition,
    #[serde(default)]
    pub blend_mode: BlendMode,
    #[serde(default = "opacity")]
    pub opacity: f32,
    pub op: FilterOp,
}
impl FilterNode {
    /// Unknown operations retain their stored enabled flag but must be skipped by
    /// an executor. Keeping the flag preserves intent when opened by a newer app.
    pub fn effective_enabled(&self) -> bool {
        self.enabled && !matches!(self.op, FilterOp::Unknown(_))
    }
    pub fn validate(&self) -> Result<(), ModelError> {
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(ModelError("node opacity must be between 0 and 1".into()));
        }
        match &self.op {
            FilterOp::Known(op) => op.validate(),
            FilterOp::Unknown(raw) => {
                let kind = raw.get("type").and_then(Value::as_str);
                if kind.is_none() || matches!(kind, Some("Levels" | "ColorMatrix3x3" | "FilmGrain"))
                {
                    return Err(ModelError("invalid opaque operation type".into()));
                }
                Ok(())
            }
        }
    }
}
