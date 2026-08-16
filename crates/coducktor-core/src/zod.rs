//! Small value-level helpers that replay the bits of Zod's `.min()/.max()/.catch()`
//! semantics `coducktor_contract::compat`'s type-level helpers don't cover: a bounded
//! number whose catch value differs from the type's `Default`, and a bounded string.
//!
//! Every workspace/config schema in this crate is read field-by-field against a decoded
//! [`serde_json::Value`] rather than via `#[derive(Deserialize)]`, because Zod's
//! `.catch(x)` degrades ONE bad key to `x` without discarding its siblings or failing the
//! whole object — `serde`'s derive can only fail (or default) the whole struct on a type
//! mismatch. Reading the raw object once and pulling each field through these helpers is
//! what makes that per-key salvage possible in Rust.

use serde_json::{Map, Value};

/// An integer field with `.int().min(lo).max(hi)`, degrading to `default` when absent,
/// the wrong type, out of range, or non-integral (mirrors `Number.isInteger`, so a JSON
/// literal like `5.0` — indistinguishable from `5` in JS — is accepted).
pub fn bounded_i64(value: Option<&Value>, lo: i64, hi: i64, default: i64) -> i64 {
    value
        .and_then(as_integer)
        .filter(|v| *v >= lo && *v <= hi)
        .unwrap_or(default)
}

/// The nullable counterpart: `.int().min(lo).max(hi).nullable()`. A JSON `null` is a
/// meaningful user choice and is returned as `Some(None)`; anything else invalid degrades
/// to `default`. Only the object's absence of the key at all is not representable here —
/// callers that need to distinguish "absent" from "explicit null" read the map directly.
pub fn bounded_i64_nullable(
    value: Option<&Value>,
    lo: i64,
    hi: i64,
    default: Option<i64>,
) -> Option<i64> {
    match value {
        None => default,
        Some(Value::Null) => None,
        Some(other) => as_integer(other)
            .filter(|v| *v >= lo && *v <= hi)
            .or(default),
    }
}

fn as_integer(value: &Value) -> Option<i64> {
    if let Some(i) = value.as_i64() {
        return Some(i);
    }
    value
        .as_f64()
        .filter(|f| f.fract() == 0.0)
        .map(|f| f as i64)
}

/// A (possibly fractional) numeric field with `.min(lo).max(hi)`, degrading to `default`
/// when absent, the wrong type, or out of range.
pub fn bounded_f64(value: Option<&Value>, lo: f64, hi: f64, default: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|v| *v >= lo && *v <= hi)
        .unwrap_or(default)
}

/// A boolean field with a plain `.default(default).catch(default)`.
pub fn bool_or(value: Option<&Value>, default: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(default)
}

/// The optional counterpart: `.boolean().optional().catch(undefined)`.
pub fn bool_opt(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

/// `.string().trim().min(lo).max(hi).optional().catch(undefined)`: `None` when absent,
/// the wrong type, or out of the trimmed length bounds. Trimming happens before the
/// bounds check (mirroring Zod's `.trim()` running first) and the stored value keeps the
/// trim — every caller of `.trim()` in the TS source stores the trimmed form.
pub fn trimmed_str_opt(value: Option<&Value>, lo: usize, hi: usize) -> Option<String> {
    let raw = value.and_then(Value::as_str)?.trim();
    let len = raw.chars().count();
    (len >= lo && len <= hi).then(|| raw.to_owned())
}

/// A string field with a plain `.max(hi).catch(default)` — no trim, no lower bound
/// (the registry's display fields: `name`, `addedAt`, `lastOpenedAt`, account `label`).
pub fn capped_str_or<'a>(value: Option<&'a Value>, hi: usize, default: &'a str) -> String {
    value
        .and_then(Value::as_str)
        .filter(|s| s.chars().count() <= hi)
        .unwrap_or(default)
        .to_owned()
}

/// The optional counterpart: `.string().max(hi).optional().catch(undefined)` — no trim,
/// no lower bound (an agent-account selection's per-provider account id).
pub fn capped_str_opt(value: Option<&Value>, hi: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| s.chars().count() <= hi)
        .map(|s| s.to_owned())
}

/// A string field matched against `regex`, with no `.catch` — the caller treats a
/// non-match as "drop this record" (load-bearing ids: project/account slugs).
pub fn regex_str(value: Option<&Value>, is_match: impl Fn(&str) -> bool) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| is_match(s))
}

/// Reads `object.get(key)` where `object` is itself optional — the common case of pulling
/// a nested field out of a value that may not even be an object.
pub fn field<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
    object.and_then(|o| o.get(key))
}

/// Coerces a decoded value into an object map, treating anything else (missing, `null`,
/// an array, a scalar) as an empty object — mirrors every schema here treating a
/// non-object as "nothing was set", never a parse failure.
pub fn as_map(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// Everything in `object` NOT named in `known_keys` — the Rust side of a Zod
/// `.passthrough()`: keys a newer coducktor wrote survive a round-trip through this crate
/// unread and unmodified.
pub fn extra_fields(object: &Map<String, Value>, known_keys: &[&str]) -> Map<String, Value> {
    object
        .iter()
        .filter(|(key, _)| !known_keys.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Builds a JSON object from `extra` (a captured `.passthrough()` bucket) overlaid with
/// `fields` — the known, typed keys always win over a same-named stray extra key.
pub fn merge_extra(extra: &Map<String, Value>, fields: Vec<(&str, Value)>) -> Value {
    let mut object = extra.clone();
    for (key, value) in fields {
        object.insert(key.to_owned(), value);
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounded_i64_accepts_whole_floats_like_zod_isinteger() {
        let value = json!(5.0);
        assert_eq!(bounded_i64(Some(&value), 1, 16, 2), 5);
    }

    #[test]
    fn bounded_i64_rejects_out_of_range_and_falls_back() {
        let value = json!(5000);
        assert_eq!(bounded_i64(Some(&value), 1, 16, 2), 2);
        assert_eq!(bounded_i64(None, 1, 16, 2), 2);
    }

    #[test]
    fn bounded_i64_nullable_distinguishes_null_from_absent() {
        assert_eq!(
            bounded_i64_nullable(Some(&Value::Null), 1, 60, Some(5)),
            None
        );
        assert_eq!(bounded_i64_nullable(None, 1, 60, Some(5)), Some(5));
    }

    #[test]
    fn trimmed_str_opt_trims_before_checking_bounds() {
        let value = json!("  hi  ");
        assert_eq!(trimmed_str_opt(Some(&value), 1, 10), Some("hi".to_owned()));
        let too_short = json!("   ");
        assert_eq!(trimmed_str_opt(Some(&too_short), 1, 10), None);
    }

    #[test]
    fn capped_str_or_has_no_lower_bound() {
        let value = json!("");
        assert_eq!(capped_str_or(Some(&value), 200, "fallback"), "");
    }
}
