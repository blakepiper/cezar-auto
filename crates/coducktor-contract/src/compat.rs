//! Compatibility helpers for the Zod semantics used by persisted contract values.

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::{Map, Value};

/// Unknown keys preserved alongside a permissive persisted contract shape.
pub type ExtraFields = Map<String, Value>;

/// Implements the per-field fallback behavior of Zod's `.catch(default)`.
pub fn catch_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Implements a nullable field whose bad value should be discarded rather than failing its record.
pub fn catch_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(serde_json::from_value(value).ok())
}

/// Implements per-entry `safeParse` salvage for persisted arrays.
pub fn salvage_entries<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}
