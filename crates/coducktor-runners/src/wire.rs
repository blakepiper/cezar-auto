use serde_json::Value;

pub(crate) fn as_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

pub(crate) fn as_nonempty_str(value: Option<&Value>) -> Option<&str> {
    let text = value?.as_str()?;
    (!text.is_empty()).then_some(text)
}

pub(crate) fn as_finite_number(value: Option<&Value>) -> Option<f64> {
    let number = value?.as_f64()?;
    number.is_finite().then_some(number)
}

pub(crate) fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

pub(crate) fn value_string(value: Option<&Value>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}
