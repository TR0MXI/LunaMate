use serde::{Deserialize, Deserializer};

use crate::{Error, Result};

const FORMAT: &str = "exp3.json";
/// Default fade-in time used when an expression omits `FadeInTime`.
pub const DEFAULT_EXPRESSION_FADE_IN_TIME: f32 = 1.0;
/// Default fade-out time used when an expression omits `FadeOutTime`.
pub const DEFAULT_EXPRESSION_FADE_OUT_TIME: f32 = 1.0;
const MAX_EXPRESSION_PARAMETERS: usize = 4_096;

#[derive(Debug, Clone, PartialEq)]
/// Parsed Cubism `exp3.json` expression data.
pub struct Expression3 {
    kind: String,
    fade_in_time: Option<f32>,
    fade_out_time: Option<f32>,
    parameters: Vec<ExpressionParameter>,
}

impl Expression3 {
    /// Parses an expression JSON document from a string.
    pub fn from_json_str(source: &str) -> Result<Self> {
        let raw: RawExpression3 =
            serde_json::from_str(source).map_err(|error| Error::InvalidJson {
                format: FORMAT,
                message: error.to_string(),
            })?;
        if raw.parameters.len() > MAX_EXPRESSION_PARAMETERS {
            return Err(Error::InvalidJson {
                format: FORMAT,
                message: format!(
                    "expression parameter count {} exceeds limit {MAX_EXPRESSION_PARAMETERS}",
                    raw.parameters.len()
                ),
            });
        }

        Ok(Self {
            kind: raw.kind,
            fade_in_time: raw.fade_in_time,
            fade_out_time: raw.fade_out_time,
            parameters: raw.parameters,
        })
    }

    /// Returns the expression type string from the JSON file.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the optional fade-in time declared by the file.
    pub fn fade_in_time(&self) -> Option<f32> {
        self.fade_in_time
    }

    /// Returns the fade-in time with Cubism defaults applied.
    pub fn resolved_fade_in_time(&self) -> f32 {
        resolved_expression_fade_in_time(self.fade_in_time)
    }

    /// Returns the optional fade-out time declared by the file.
    pub fn fade_out_time(&self) -> Option<f32> {
        self.fade_out_time
    }

    /// Returns the fade-out time with Cubism defaults applied.
    pub fn resolved_fade_out_time(&self) -> f32 {
        resolved_expression_fade_out_time(self.fade_out_time)
    }

    /// Returns parameter operations in this expression.
    pub fn parameters(&self) -> &[ExpressionParameter] {
        &self.parameters
    }
}

/// Resolves an optional expression fade-in time to a non-negative value.
pub fn resolved_expression_fade_in_time(fade_in_time: Option<f32>) -> f32 {
    fade_in_time
        .unwrap_or(DEFAULT_EXPRESSION_FADE_IN_TIME)
        .max(0.0)
}

/// Resolves an optional expression fade-out time to a non-negative value.
pub fn resolved_expression_fade_out_time(fade_out_time: Option<f32>) -> f32 {
    fade_out_time
        .unwrap_or(DEFAULT_EXPRESSION_FADE_OUT_TIME)
        .max(0.0)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RawExpression3 {
    #[serde(rename = "Type")]
    kind: String,
    #[serde(rename = "FadeInTime", default)]
    fade_in_time: Option<f32>,
    #[serde(rename = "FadeOutTime", default)]
    fade_out_time: Option<f32>,
    #[serde(rename = "Parameters", default)]
    parameters: Vec<ExpressionParameter>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
/// One parameter operation in an expression.
pub struct ExpressionParameter {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Value")]
    value: f32,
    #[serde(
        rename = "Blend",
        default,
        deserialize_with = "deserialize_expression_blend"
    )]
    blend: ExpressionBlend,
}

impl ExpressionParameter {
    /// Returns the target parameter id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the value used by this parameter operation.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Returns how the value is blended with the current parameter.
    pub fn blend(&self) -> ExpressionBlend {
        self.blend
    }
}

/// Applies one expression parameter operation to a current value.
pub fn apply_expression_parameter(
    current: f32,
    parameter: &ExpressionParameter,
    weight: f32,
) -> f32 {
    apply_expression_blend(current, parameter.value, parameter.blend, weight)
}

/// Applies an expression blend operation to a current value.
pub fn apply_expression_blend(
    current: f32,
    value: f32,
    blend: ExpressionBlend,
    weight: f32,
) -> f32 {
    match blend {
        ExpressionBlend::Add => current + (value * weight),
        ExpressionBlend::Multiply => current * (1.0 + (value - 1.0) * weight),
        ExpressionBlend::Overwrite if weight == 1.0 => value,
        ExpressionBlend::Overwrite => (current * (1.0 - weight)) + (value * weight),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
/// Blend mode for an expression parameter.
pub enum ExpressionBlend {
    /// Adds the expression value to the current parameter.
    #[default]
    Add,
    /// Multiplies the current parameter by the expression value.
    Multiply,
    /// Interpolates from the current parameter to the expression value.
    Overwrite,
}

impl ExpressionBlend {
    fn from_raw(value: Option<&str>) -> Self {
        match value {
            Some("Multiply") => Self::Multiply,
            Some("Overwrite") => Self::Overwrite,
            _ => Self::Add,
        }
    }
}

fn deserialize_expression_blend<'de, D>(
    deserializer: D,
) -> std::result::Result<ExpressionBlend, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
        .map(|value| ExpressionBlend::from_raw(value.as_deref()))
}
