use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::error::DomainError;
use super::id::ManifestDigest;

/// Serialize any domain value to the crate's canonical JSON byte form.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    let value = serde_json::to_value(value)
        .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
    let mut output = Vec::new();
    write_canonical(&value, &mut output)?;
    Ok(output)
}

/// Serialize a JSON value with recursively lexicographic object keys and no
/// insignificant whitespace.
pub fn canonical_json_value(value: &Value) -> Result<String, DomainError> {
    let mut output = String::new();
    write_canonical(value, &mut output)?;
    Ok(output)
}

/// Compute the canonical SHA-256 digest encoding used by domain manifests.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<ManifestDigest, DomainError> {
    let value = serde_json::to_value(value)
        .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
    let mut hasher = Sha256::new();
    write_canonical(&value, &mut hasher)?;
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
    }
    ManifestDigest::new(encoded)
}

trait CanonicalSink {
    fn write(&mut self, chunk: &str);
}

impl CanonicalSink for String {
    fn write(&mut self, chunk: &str) {
        self.push_str(chunk);
    }
}

impl CanonicalSink for Vec<u8> {
    fn write(&mut self, chunk: &str) {
        self.extend_from_slice(chunk.as_bytes());
    }
}

impl CanonicalSink for Sha256 {
    fn write(&mut self, chunk: &str) {
        Digest::update(self, chunk.as_bytes());
    }
}

fn write_canonical(value: &Value, output: &mut impl CanonicalSink) -> Result<(), DomainError> {
    match value {
        Value::Null => output.write("null"),
        Value::Bool(value) => output.write(if *value { "true" } else { "false" }),
        Value::Number(value) => output.write(&value.to_string()),
        Value::String(value) => output.write(
            &serde_json::to_string(value)
                .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?,
        ),
        Value::Array(values) => {
            output.write("[");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.write(",");
                }
                write_canonical(value, output)?;
            }
            output.write("]");
        }
        Value::Object(values) => {
            output.write("{");
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.write(",");
                }
                output.write(
                    &serde_json::to_string(key)
                        .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?,
                );
                output.write(":");
                write_canonical(value, output)?;
            }
            output.write("}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_outputs_match_for_nested_ordering_and_scalars() {
        let value = json!({
            "z": null,
            "array": [true, false, -12.5, 0, {"z": 2, "a": 1}],
            "a": {"z": "last", "a": "first"},
        });
        let expected = concat!(
            r#"{"a":{"a":"first","z":"last"},"#,
            r#""array":[true,false,-12.5,0,{"a":1,"z":2}],"#,
            r#""z":null}"#,
        );

        let text = canonical_json_value(&value).unwrap();
        let bytes = canonical_json_bytes(&value).unwrap();

        assert_eq!(text, expected);
        assert_eq!(bytes, expected.as_bytes());
    }

    #[test]
    fn canonical_outputs_preserve_json_escapes_and_unicode() {
        let value = json!({
            "unicode": "雪😀é",
            "escaped": "quote: \" slash: \\ newline:\n tab:\t control:\u{0001}",
        });
        let expected = "{\"escaped\":\"quote: \\\" slash: \\\\ newline:\\n tab:\\t control:\\u0001\",\"unicode\":\"雪😀é\"}";

        assert_eq!(canonical_json_value(&value).unwrap(), expected);
        assert_eq!(canonical_json_bytes(&value).unwrap(), expected.as_bytes());
    }

    #[test]
    fn streaming_digest_matches_digest_of_canonical_bytes() {
        let value = json!({
            "unicode": ["雪", "😀", "é"],
            "nested": {"z": null, "a": [true, "line\nfeed", 42]},
        });
        let bytes = canonical_json_bytes(&value).unwrap();
        let digest = Sha256::digest(&bytes);
        let mut expected = String::from("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut expected, "{byte:02x}").unwrap();
        }

        assert_eq!(canonical_sha256(&value).unwrap().as_str(), expected);
    }
}
