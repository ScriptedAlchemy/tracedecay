//! Closed JSON-object schema construction shared by application tools.

use serde_json::json;

use super::required_object_schema;

pub(super) fn closed_object_schema(
    properties: serde_json::Value,
    required: &[&str],
) -> serde_json::Value {
    let mut schema = required_object_schema(properties, required);
    schema["additionalProperties"] = json!(false);
    schema
}
