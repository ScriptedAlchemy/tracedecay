use serde_json::Value;

use super::canonical_sink::{CanonicalSink, write_json_number, write_json_string};

/// Whether a JSON object's keys already arrive in canonical (byte-lexicographic)
/// order, in which case the entries need not be collected and sorted.
pub(super) fn keys_are_canonically_ordered(values: &serde_json::Map<String, Value>) -> bool {
    let mut previous: Option<&str> = None;
    for key in values.keys() {
        if previous.is_some_and(|previous| previous > key.as_str()) {
            return false;
        }
        previous = Some(key);
    }
    true
}

pub(super) fn write_canonical(value: &Value, output: &mut impl CanonicalSink) {
    match value {
        Value::Null => output.write("null"),
        Value::Bool(value) => output.write(if *value { "true" } else { "false" }),
        Value::Number(value) => write_json_number(value, output),
        Value::String(value) => write_json_string(value, output),
        Value::Array(values) => {
            output.write("[");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.write(",");
                }
                write_canonical(value, output);
            }
            output.write("]");
        }
        Value::Object(values) => {
            output.write("{");
            if keys_are_canonically_ordered(values) {
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.write(",");
                    }
                    write_json_string(key, output);
                    output.write(":");
                    write_canonical(value, output);
                }
            } else {
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        output.write(",");
                    }
                    write_json_string(key, output);
                    output.write(":");
                    write_canonical(value, output);
                }
            }
            output.write("}");
        }
    }
}
