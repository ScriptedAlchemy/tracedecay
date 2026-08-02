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

    /// Append an already-encoded UTF-8 chunk without an intermediate buffer.
    fn write_bytes(&mut self, chunk: &[u8]) -> Result<(), DomainError>;
}

impl CanonicalSink for String {
    fn write(&mut self, chunk: &str) {
        self.push_str(chunk);
    }

    fn write_bytes(&mut self, chunk: &[u8]) -> Result<(), DomainError> {
        let chunk = std::str::from_utf8(chunk)
            .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
        self.push_str(chunk);
        Ok(())
    }
}

impl CanonicalSink for Vec<u8> {
    fn write(&mut self, chunk: &str) {
        self.extend_from_slice(chunk.as_bytes());
    }

    fn write_bytes(&mut self, chunk: &[u8]) -> Result<(), DomainError> {
        self.extend_from_slice(chunk);
        Ok(())
    }
}

impl CanonicalSink for Sha256 {
    fn write(&mut self, chunk: &str) {
        Digest::update(self, chunk.as_bytes());
    }

    fn write_bytes(&mut self, chunk: &[u8]) -> Result<(), DomainError> {
        Digest::update(self, chunk);
        Ok(())
    }
}

/// Adapts a canonical sink to `io::Write`.
///
/// `serde_json` owns JSON string escaping, and its escape tables only ever cut
/// a chunk at an ASCII byte, so every chunk it emits is complete UTF-8. Routing
/// those chunks straight into the sink writes exactly the bytes
/// `serde_json::to_string` would have produced, without allocating and dropping
/// one `Vec<u8>` plus one `String` for every scalar and object key in the tree.
struct SinkWriter<'a, S: CanonicalSink + ?Sized> {
    sink: &'a mut S,
    error: Option<DomainError>,
}

impl<S: CanonicalSink + ?Sized> std::io::Write for SinkWriter<'_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Err(error) = self.sink.write_bytes(buf) {
            let message = error.to_string();
            self.error = Some(error);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            ));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stream one JSON string literal (quotes and escapes included) into the sink.
fn write_json_string(value: &str, output: &mut impl CanonicalSink) -> Result<(), DomainError> {
    let mut writer = SinkWriter {
        sink: output,
        error: None,
    };
    let outcome = serde_json::to_writer(&mut writer, value);
    if let Some(error) = writer.error.take() {
        return Err(error);
    }
    outcome.map_err(|error| DomainError::CanonicalSerialization(error.to_string()))
}

fn write_canonical(value: &Value, output: &mut impl CanonicalSink) -> Result<(), DomainError> {
    match value {
        Value::Null => output.write("null"),
        Value::Bool(value) => output.write(if *value { "true" } else { "false" }),
        Value::Number(value) => output.write(&value.to_string()),
        Value::String(value) => write_json_string(value, output)?,
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
                write_json_string(key, output)?;
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

    /// The streamed string writer must emit exactly the bytes the allocating
    /// `serde_json::to_string` encoder produced, for keys and for values.
    #[test]
    fn streamed_json_strings_match_serde_json_to_string() {
        let cases = [
            "",
            "plain",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "quote: \" backslash: \\ slash: /",
            "controls: \u{0}\u{1}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{1f}",
            "雪😀é",
            "mixed 😀 \" \u{1} tail",
            "\u{7f}\u{80}\u{a0}\u{2028}\u{2029}",
        ];
        for case in cases {
            let expected = serde_json::to_string(case).unwrap();

            let mut text = String::new();
            write_json_string(case, &mut text).unwrap();
            assert_eq!(text, expected, "string sink diverged for {case:?}");

            let mut bytes: Vec<u8> = Vec::new();
            write_json_string(case, &mut bytes).unwrap();
            assert_eq!(
                bytes,
                expected.as_bytes(),
                "byte sink diverged for {case:?}"
            );

            let mut hasher = Sha256::new();
            write_json_string(case, &mut hasher).unwrap();
            assert_eq!(
                hasher.finalize(),
                Sha256::digest(expected.as_bytes()),
                "hash sink diverged for {case:?}"
            );

            // The same corpus routed through a whole document must also match.
            let document = json!({ case: [case, {"nested": case}] });
            let mut canonical = String::new();
            write_canonical(&document, &mut canonical).unwrap();
            assert_eq!(
                canonical,
                format!("{{{expected}:[{expected},{{\"nested\":{expected}}}]}}")
            );
        }
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
