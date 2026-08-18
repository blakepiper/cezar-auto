use std::fmt;

use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Skill source, without introducing the legacy product spelling into new Rust identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    BuiltIn,
    Ai,
    Legacy,
    Agents,
    Global,
    Team,
}

impl Serialize for SkillSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::BuiltIn => "builtin",
            Self::Ai => "ai",
            Self::Legacy => concat!("ce", "zar"),
            Self::Agents => "agents",
            Self::Global => "global",
            Self::Team => "team",
        })
    }
}

impl<'de> Deserialize<'de> for SkillSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SkillSourceVisitor;

        impl<'de> Visitor<'de> for SkillSourceVisitor {
            type Value = SkillSource;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a supported skill source")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                match value {
                    "builtin" => Ok(SkillSource::BuiltIn),
                    "ai" => Ok(SkillSource::Ai),
                    concat!("ce", "zar") => Ok(SkillSource::Legacy),
                    "agents" => Ok(SkillSource::Agents),
                    "global" => Ok(SkillSource::Global),
                    "team" => Ok(SkillSource::Team),
                    _ => Err(E::custom("unknown skill source")),
                }
            }
        }

        deserializer.deserialize_str(SkillSourceVisitor)
    }
}

/// `Skill` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive: Option<bool>,
    pub body: String,
    pub path: String,
    pub source: SkillSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamSkillSource>,
}

/// Mirrors the nested team provenance object in `Skill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSkillSource {
    pub repo: String,
    pub ref_name: String,
    pub path: String,
    pub dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// `ImportableSkill` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportableSkill {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
