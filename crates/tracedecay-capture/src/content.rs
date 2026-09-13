use serde_json::Value;

/// Whether a native message `content` value carries nothing renderable:
/// null, blank text, or an empty collection. Numbers and booleans count as
/// content.
pub fn content_is_empty(content: &Value) -> bool {
    match content {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}
