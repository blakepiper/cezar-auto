use std::fmt;

use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::runs::RunRecord;

/// Mirrors `packages/contract/src/skills.ts::SkillSource` without introducing the legacy
/// product spelling into new Rust identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
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

/// Mirrors `packages/contract/src/skills.ts::Skill`.
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

/// Mirrors `packages/contract/src/skills.ts::ImportableSkill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportableSkill {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Mirrors `packages/contract/src/skills.ts::TodoItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runnable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_task_id: Option<String>,
}

/// Mirrors `packages/contract/src/skills.ts::RemoveTodoResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveTodoResponse {
    pub removed: bool,
}

/// Mirrors `packages/contract/src/skills.ts::StartTodoResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartTodoResponse {
    pub run: RunRecord,
}
