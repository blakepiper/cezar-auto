use serde::{Deserialize, Serialize};

/// `IdeEntry` contract shape.
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

/// `IdeDirectoryResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeDirectoryResponse {
    pub path: String,
    pub entries: Vec<IdeEntry>,
    pub truncated: bool,
}

/// `IdeFileResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeFileResponse {
    pub path: String,
    pub content: String,
    pub size: u64,
}
