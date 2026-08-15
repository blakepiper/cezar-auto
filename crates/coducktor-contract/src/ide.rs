use serde::{Deserialize, Serialize};

/// Mirrors `packages/contract/src/ide.ts::IdeDirectoryQuery`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdeDirectoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Mirrors `packages/contract/src/ide.ts::IdeFileQuery`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdeFileQuery {
    pub path: String,
}

/// Mirrors `packages/contract/src/ide.ts::IdeFileInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdeFileInput {
    pub path: String,
    pub content: String,
}

/// Mirrors `packages/contract/src/ide.ts::IdeEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: IdeEntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// The `type` discriminator in an IDE directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdeEntryType {
    Dir,
    File,
}

/// Mirrors `packages/contract/src/ide.ts::IdeDirectoryResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeDirectoryResponse {
    pub path: String,
    pub entries: Vec<IdeEntry>,
    pub truncated: bool,
}

/// Mirrors `packages/contract/src/ide.ts::IdeFileResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeFileResponse {
    pub path: String,
    pub content: String,
    pub size: u64,
}
