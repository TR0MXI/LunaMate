use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

const FORMAT: &str = "model3.json";
const SUPPORTED_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed Cubism `model3.json` data.
///
/// The model file is the manifest for a Live2D model folder. It points to the
/// `.moc3` file, textures, motions, expressions, physics, pose data, display
/// metadata, groups, and hit areas.
pub struct Model3 {
    version: u32,
    file_references: FileReferences,
    groups: Vec<Group>,
    hit_areas: Vec<HitArea>,
}

impl Model3 {
    /// Parses a `model3.json` document from a string.
    pub fn from_json_str(source: &str) -> Result<Self> {
        let raw: RawModel3 = serde_json::from_str(source).map_err(|error| Error::InvalidJson {
            format: FORMAT,
            message: error.to_string(),
        })?;

        if raw.version != SUPPORTED_VERSION {
            return Err(Error::UnsupportedVersion {
                format: FORMAT,
                version: raw.version,
            });
        }

        Ok(Self {
            version: raw.version,
            file_references: raw.file_references,
            groups: raw.groups,
            hit_areas: raw.hit_areas,
        })
    }

    /// Returns the supported Cubism model version.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Returns the referenced `.moc3` path relative to the model file.
    pub fn moc(&self) -> &str {
        &self.file_references.moc
    }

    /// Returns texture paths relative to the model file.
    pub fn textures(&self) -> &[String] {
        &self.file_references.textures
    }

    /// Returns the optional `physics3.json` path.
    pub fn physics(&self) -> Option<&str> {
        self.file_references.physics.as_deref()
    }

    /// Returns the optional `pose3.json` path.
    pub fn pose(&self) -> Option<&str> {
        self.file_references.pose.as_deref()
    }

    /// Returns the optional `cdi3.json` path.
    pub fn display_info(&self) -> Option<&str> {
        self.file_references.display_info.as_deref()
    }

    /// Returns motion groups keyed by their Cubism group name.
    pub fn motions(&self) -> &BTreeMap<String, Vec<MotionReference>> {
        &self.file_references.motions
    }

    /// Returns expression references declared by the model.
    pub fn expressions(&self) -> &[ExpressionReference] {
        &self.file_references.expressions
    }

    /// Returns parameter and part groups declared by the model.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Returns named hit areas declared by the model.
    pub fn hit_areas(&self) -> &[HitArea] {
        &self.hit_areas
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawModel3 {
    #[serde(rename = "Version")]
    version: u32,
    #[serde(rename = "FileReferences")]
    file_references: FileReferences,
    #[serde(rename = "Groups", default)]
    groups: Vec<Group>,
    #[serde(rename = "HitAreas", default)]
    hit_areas: Vec<HitArea>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FileReferences {
    #[serde(rename = "Moc")]
    moc: String,
    #[serde(rename = "Textures")]
    textures: Vec<String>,
    #[serde(rename = "Physics", default)]
    physics: Option<String>,
    #[serde(rename = "Pose", default)]
    pose: Option<String>,
    #[serde(rename = "DisplayInfo", default)]
    display_info: Option<String>,
    #[serde(rename = "Motions", default)]
    motions: BTreeMap<String, Vec<MotionReference>>,
    #[serde(rename = "Expressions", default)]
    expressions: Vec<ExpressionReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// Reference to a motion file from `model3.json`.
pub struct MotionReference {
    #[serde(rename = "File")]
    file: String,
}

impl MotionReference {
    /// Returns the motion file path relative to the model file.
    pub fn file(&self) -> &str {
        &self.file
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// Reference to an expression file from `model3.json`.
pub struct ExpressionReference {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "File")]
    file: String,
}

impl ExpressionReference {
    /// Returns the display name for this expression.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the expression file path relative to the model file.
    pub fn file(&self) -> &str {
        &self.file
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// A named group of parameter or part ids.
pub struct Group {
    #[serde(rename = "Target")]
    target: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Ids")]
    ids: Vec<String>,
}

impl Group {
    /// Returns the group target, such as `Parameter` or `PartOpacity`.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the Cubism group name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns ids that belong to this group.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// A named hit area declared by the model.
pub struct HitArea {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
}

impl HitArea {
    /// Returns the drawable or part id used for hit testing.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the user-facing hit area name.
    pub fn name(&self) -> &str {
        &self.name
    }
}
