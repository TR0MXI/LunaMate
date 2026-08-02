use serde::Deserialize;

use crate::{Error, Result};

const FORMAT: &str = "physics3.json";
const SUPPORTED_VERSION: u32 = 3;
const MAX_PHYSICS_SETTINGS: usize = 256;
const MAX_INPUTS_PER_SETTING: usize = 256;
const MAX_OUTPUTS_PER_SETTING: usize = 256;
const MAX_VERTICES_PER_SETTING: usize = 256;
const MAX_TOTAL_INPUTS: usize = 4_096;
const MAX_TOTAL_OUTPUTS: usize = 4_096;
const MAX_TOTAL_VERTICES: usize = 4_096;
const MAX_WEIGHT: f32 = 100.0;
const MIN_NON_ZERO_DELAY: f32 = 0.0001;
const MAX_DYNAMICS_VALUE: f32 = 1_000.0;
const MAX_CALCULATION_MAGNITUDE: f32 = 1_000_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Physics3 {
    version: u32,
    meta: PhysicsMeta,
    settings: Vec<PhysicsSetting>,
}

impl Physics3 {
    pub fn from_json_str(source: &str) -> Result<Self> {
        let raw: RawPhysics3 =
            serde_json::from_str(source).map_err(|error| Error::InvalidJson {
                format: FORMAT,
                message: error.to_string(),
            })?;

        if raw.version != SUPPORTED_VERSION {
            return Err(Error::UnsupportedVersion {
                format: FORMAT,
                version: raw.version,
            });
        }

        validate_physics(&raw)?;

        Ok(Self {
            version: raw.version,
            meta: raw.meta,
            settings: raw.settings,
        })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn meta(&self) -> &PhysicsMeta {
        &self.meta
    }

    pub fn settings(&self) -> &[PhysicsSetting] {
        &self.settings
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RawPhysics3 {
    #[serde(rename = "Version")]
    version: u32,
    #[serde(rename = "Meta")]
    meta: PhysicsMeta,
    #[serde(rename = "PhysicsSettings", default)]
    settings: Vec<PhysicsSetting>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsMeta {
    #[serde(rename = "PhysicsSettingCount")]
    physics_setting_count: u32,
    #[serde(rename = "TotalInputCount")]
    total_input_count: u32,
    #[serde(rename = "TotalOutputCount")]
    total_output_count: u32,
    #[serde(rename = "VertexCount")]
    vertex_count: u32,
    #[serde(rename = "Fps", default)]
    fps: f32,
    #[serde(rename = "EffectiveForces")]
    effective_forces: EffectiveForces,
    #[serde(rename = "PhysicsDictionary", default)]
    physics_dictionary: Vec<PhysicsDictionaryEntry>,
}

impl PhysicsMeta {
    pub fn physics_setting_count(&self) -> u32 {
        self.physics_setting_count
    }

    pub fn total_input_count(&self) -> u32 {
        self.total_input_count
    }

    pub fn total_output_count(&self) -> u32 {
        self.total_output_count
    }

    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }

    pub fn effective_forces(&self) -> &EffectiveForces {
        &self.effective_forces
    }

    pub fn physics_dictionary(&self) -> &[PhysicsDictionaryEntry] {
        &self.physics_dictionary
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EffectiveForces {
    #[serde(rename = "Gravity")]
    gravity: Vector2,
    #[serde(rename = "Wind")]
    wind: Vector2,
}

impl EffectiveForces {
    pub fn gravity(&self) -> &Vector2 {
        &self.gravity
    }

    pub fn wind(&self) -> &Vector2 {
        &self.wind
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsDictionaryEntry {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
}

impl PhysicsDictionaryEntry {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsSetting {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Input", default)]
    inputs: Vec<PhysicsInput>,
    #[serde(rename = "Output", default)]
    outputs: Vec<PhysicsOutput>,
    #[serde(rename = "Vertices", default)]
    vertices: Vec<PhysicsVertex>,
    #[serde(rename = "Normalization")]
    normalization: PhysicsNormalization,
}

impl PhysicsSetting {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn inputs(&self) -> &[PhysicsInput] {
        &self.inputs
    }

    pub fn outputs(&self) -> &[PhysicsOutput] {
        &self.outputs
    }

    pub fn vertices(&self) -> &[PhysicsVertex] {
        &self.vertices
    }

    pub fn normalization(&self) -> &PhysicsNormalization {
        &self.normalization
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsInput {
    #[serde(rename = "Source")]
    source: PhysicsSource,
    #[serde(rename = "Weight")]
    weight: f32,
    #[serde(rename = "Type")]
    kind: PhysicsValueKind,
    #[serde(rename = "Reflect")]
    reflect: bool,
}

impl PhysicsInput {
    pub fn source(&self) -> &PhysicsSource {
        &self.source
    }

    pub fn weight(&self) -> f32 {
        self.weight
    }

    pub fn kind(&self) -> PhysicsValueKind {
        self.kind
    }

    pub fn reflect(&self) -> bool {
        self.reflect
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsOutput {
    #[serde(rename = "Destination")]
    destination: PhysicsSource,
    #[serde(rename = "VertexIndex")]
    vertex_index: u32,
    #[serde(rename = "Scale")]
    scale: f32,
    #[serde(rename = "Weight")]
    weight: f32,
    #[serde(rename = "Type")]
    kind: PhysicsValueKind,
    #[serde(rename = "Reflect")]
    reflect: bool,
}

impl PhysicsOutput {
    pub fn destination(&self) -> &PhysicsSource {
        &self.destination
    }

    pub fn vertex_index(&self) -> u32 {
        self.vertex_index
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn weight(&self) -> f32 {
        self.weight
    }

    pub fn kind(&self) -> PhysicsValueKind {
        self.kind
    }

    pub fn reflect(&self) -> bool {
        self.reflect
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsSource {
    #[serde(rename = "Target")]
    target: String,
    #[serde(rename = "Id")]
    id: String,
}

impl PhysicsSource {
    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize)]
pub enum PhysicsValueKind {
    #[serde(rename = "X")]
    X,
    #[serde(rename = "Y")]
    Y,
    #[serde(rename = "Angle")]
    Angle,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsVertex {
    #[serde(rename = "Mobility")]
    mobility: f32,
    #[serde(rename = "Delay")]
    delay: f32,
    #[serde(rename = "Acceleration")]
    acceleration: f32,
    #[serde(rename = "Radius")]
    radius: f32,
    #[serde(rename = "Position")]
    position: Vector2,
}

impl PhysicsVertex {
    pub fn mobility(&self) -> f32 {
        self.mobility
    }

    pub fn delay(&self) -> f32 {
        self.delay
    }

    pub fn acceleration(&self) -> f32 {
        self.acceleration
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    pub fn position(&self) -> &Vector2 {
        &self.position
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsNormalization {
    #[serde(rename = "Position")]
    position: PhysicsNormalizationValue,
    #[serde(rename = "Angle")]
    angle: PhysicsNormalizationValue,
}

impl PhysicsNormalization {
    pub fn position(&self) -> &PhysicsNormalizationValue {
        &self.position
    }

    pub fn angle(&self) -> &PhysicsNormalizationValue {
        &self.angle
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PhysicsNormalizationValue {
    #[serde(rename = "Minimum")]
    minimum: f32,
    #[serde(rename = "Default")]
    default: f32,
    #[serde(rename = "Maximum")]
    maximum: f32,
}

impl PhysicsNormalizationValue {
    pub fn minimum(&self) -> f32 {
        self.minimum
    }

    pub fn default(&self) -> f32 {
        self.default
    }

    pub fn maximum(&self) -> f32 {
        self.maximum
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Vector2 {
    #[serde(rename = "X")]
    x: f32,
    #[serde(rename = "Y")]
    y: f32,
}

impl Vector2 {
    pub fn x(&self) -> f32 {
        self.x
    }

    pub fn y(&self) -> f32 {
        self.y
    }
}

fn validate_physics(raw: &RawPhysics3) -> Result<()> {
    let actual_setting_count = raw.settings.len();
    validate_count(
        "physics setting count",
        actual_setting_count,
        MAX_PHYSICS_SETTINGS,
    )?;

    let mut actual_input_count = 0_usize;
    let mut actual_output_count = 0_usize;
    let mut actual_vertex_count = 0_usize;
    for (setting_index, setting) in raw.settings.iter().enumerate() {
        validate_setting_count(
            setting_index,
            "input",
            setting.inputs.len(),
            MAX_INPUTS_PER_SETTING,
        )?;
        validate_setting_count(
            setting_index,
            "output",
            setting.outputs.len(),
            MAX_OUTPUTS_PER_SETTING,
        )?;
        validate_setting_count(
            setting_index,
            "vertex",
            setting.vertices.len(),
            MAX_VERTICES_PER_SETTING,
        )?;

        actual_input_count = add_to_total_count(
            actual_input_count,
            setting.inputs.len(),
            "input",
            MAX_TOTAL_INPUTS,
        )?;
        actual_output_count = add_to_total_count(
            actual_output_count,
            setting.outputs.len(),
            "output",
            MAX_TOTAL_OUTPUTS,
        )?;
        actual_vertex_count = add_to_total_count(
            actual_vertex_count,
            setting.vertices.len(),
            "vertex",
            MAX_TOTAL_VERTICES,
        )?;
    }

    validate_reported_counts(
        &raw.meta,
        actual_setting_count,
        actual_input_count,
        actual_output_count,
        actual_vertex_count,
    )?;
    validate_meta_values(&raw.meta)?;
    for (setting_index, setting) in raw.settings.iter().enumerate() {
        validate_setting(setting, setting_index)?;
    }

    Ok(())
}

fn validate_count(name: &str, actual: usize, limit: usize) -> Result<()> {
    if actual > limit {
        return Err(invalid_physics(format!(
            "{name} {actual} exceeds limit {limit}"
        )));
    }
    Ok(())
}

fn validate_setting_count(
    setting_index: usize,
    name: &str,
    actual: usize,
    limit: usize,
) -> Result<()> {
    if actual > limit {
        return Err(invalid_physics(format!(
            "setting {setting_index} {name} count {actual} exceeds per-setting limit {limit}"
        )));
    }
    Ok(())
}

fn add_to_total_count(total: usize, count: usize, name: &str, limit: usize) -> Result<usize> {
    let total = total
        .checked_add(count)
        .ok_or_else(|| invalid_physics(format!("total {name} count overflows")))?;
    validate_count(&format!("total {name} count"), total, limit)?;
    Ok(total)
}

fn validate_reported_counts(
    meta: &PhysicsMeta,
    actual_setting_count: usize,
    actual_input_count: usize,
    actual_output_count: usize,
    actual_vertex_count: usize,
) -> Result<()> {
    for (name, reported, actual) in [
        (
            "physics setting count",
            meta.physics_setting_count,
            actual_setting_count,
        ),
        (
            "total input count",
            meta.total_input_count,
            actual_input_count,
        ),
        (
            "total output count",
            meta.total_output_count,
            actual_output_count,
        ),
        ("vertex count", meta.vertex_count, actual_vertex_count),
    ] {
        if u64::from(reported) != actual as u64 {
            return Err(invalid_physics(format!(
                "reported {name} {reported} does not match actual count {actual}"
            )));
        }
    }
    Ok(())
}

fn validate_meta_values(meta: &PhysicsMeta) -> Result<()> {
    validate_finite(meta.fps, "physics FPS")?;
    if meta.fps < 0.0 {
        return Err(invalid_physics("physics FPS must be non-negative"));
    }
    validate_vector(meta.effective_forces.gravity(), "gravity")?;
    validate_vector(meta.effective_forces.wind(), "wind")
}

fn validate_setting(setting: &PhysicsSetting, setting_index: usize) -> Result<()> {
    validate_normalization(
        setting.normalization.position(),
        &format!("setting {setting_index} position normalization"),
    )?;
    validate_normalization(
        setting.normalization.angle(),
        &format!("setting {setting_index} angle normalization"),
    )?;

    for (input_index, input) in setting.inputs.iter().enumerate() {
        validate_weight(
            input.weight,
            &format!("setting {setting_index} input {input_index} weight"),
        )?;
    }

    for (output_index, output) in setting.outputs.iter().enumerate() {
        let output_name = format!("setting {setting_index} output {output_index}");
        validate_signed_magnitude(output.scale, &format!("{output_name} scale"))?;
        validate_weight(output.weight, &format!("{output_name} weight"))?;

        let vertex_index = usize::try_from(output.vertex_index).map_err(|_| {
            invalid_physics(format!(
                "{output_name} vertex index {} cannot be represented on this platform",
                output.vertex_index
            ))
        })?;
        if vertex_index == 0 || vertex_index >= setting.vertices.len() {
            return Err(invalid_physics(format!(
                "{output_name} vertex index {} is out of range for {} vertices; an output requires its vertex and the preceding vertex",
                output.vertex_index,
                setting.vertices.len()
            )));
        }
    }

    for (vertex_index, vertex) in setting.vertices.iter().enumerate() {
        let vertex_name = format!("setting {setting_index} vertex {vertex_index}");
        validate_non_negative_bounded(
            vertex.mobility,
            MAX_DYNAMICS_VALUE,
            &format!("{vertex_name} mobility"),
        )?;
        validate_delay(vertex.delay, &format!("{vertex_name} delay"))?;
        validate_non_negative_bounded(
            vertex.acceleration,
            MAX_DYNAMICS_VALUE,
            &format!("{vertex_name} acceleration"),
        )?;
        validate_non_negative_bounded(
            vertex.radius,
            MAX_CALCULATION_MAGNITUDE,
            &format!("{vertex_name} radius"),
        )?;
        validate_vector(&vertex.position, &format!("{vertex_name} position"))?;
    }

    Ok(())
}

fn validate_normalization(value: &PhysicsNormalizationValue, name: &str) -> Result<()> {
    validate_signed_magnitude(value.minimum, &format!("{name} minimum"))?;
    validate_signed_magnitude(value.default, &format!("{name} default"))?;
    validate_signed_magnitude(value.maximum, &format!("{name} maximum"))?;

    if value.minimum >= value.maximum {
        return Err(invalid_physics(format!(
            "{name} minimum must be less than maximum"
        )));
    }
    if !(value.minimum..=value.maximum).contains(&value.default) {
        return Err(invalid_physics(format!(
            "{name} default must be between minimum and maximum"
        )));
    }
    Ok(())
}

fn validate_weight(value: f32, name: &str) -> Result<()> {
    validate_finite(value, name)?;
    if !(0.0..=MAX_WEIGHT).contains(&value) {
        return Err(invalid_physics(format!(
            "{name} must be between 0 and {MAX_WEIGHT}"
        )));
    }
    Ok(())
}

fn validate_non_negative_bounded(value: f32, maximum: f32, name: &str) -> Result<()> {
    validate_finite(value, name)?;
    if !(0.0..=maximum).contains(&value) {
        return Err(invalid_physics(format!(
            "{name} must be between 0 and {maximum}"
        )));
    }
    Ok(())
}

fn validate_delay(value: f32, name: &str) -> Result<()> {
    validate_non_negative_bounded(value, MAX_DYNAMICS_VALUE, name)?;
    if value != 0.0 && value < MIN_NON_ZERO_DELAY {
        return Err(invalid_physics(format!(
            "{name} must be zero or at least {MIN_NON_ZERO_DELAY}"
        )));
    }
    Ok(())
}

fn validate_signed_magnitude(value: f32, name: &str) -> Result<()> {
    validate_finite(value, name)?;
    if value.abs() > MAX_CALCULATION_MAGNITUDE {
        return Err(invalid_physics(format!(
            "{name} magnitude must not exceed {MAX_CALCULATION_MAGNITUDE}"
        )));
    }
    Ok(())
}

fn validate_vector(value: &Vector2, name: &str) -> Result<()> {
    validate_signed_magnitude(value.x, &format!("{name} X"))?;
    validate_signed_magnitude(value.y, &format!("{name} Y"))
}

fn validate_finite(value: f32, name: &str) -> Result<()> {
    if !value.is_finite() {
        return Err(invalid_physics(format!("{name} must be finite")));
    }
    Ok(())
}

fn invalid_physics(message: impl Into<String>) -> Error {
    Error::InvalidJson {
        format: FORMAT,
        message: message.into(),
    }
}
