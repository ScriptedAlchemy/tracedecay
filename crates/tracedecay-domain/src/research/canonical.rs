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

/// The JSON escape `serde_json`'s compact formatter emits for each control
/// byte. Mirroring the table here lets canonical writing stream escapes
/// straight into the sink instead of allocating a `String` per string value.
static CONTROL_ESCAPES: [&str; 32] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007", "\\b",
    "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011", "\\u0012",
    "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019", "\\u001a",
    "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

/// Write one JSON string literal (quotes included) directly into the sink.
///
/// Byte-for-byte equivalent to `serde_json::to_string(value)` for a string:
/// only `"`, `\`, and the C0 control bytes are escaped, non-ASCII is passed
/// through as UTF-8, and `\u00xx` escapes use lowercase hex.
fn write_json_string(value: &str, output: &mut impl CanonicalSink) {
    output.write("\"");
    let mut run_start = 0usize;
    for (index, byte) in value.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x00..=0x1f => CONTROL_ESCAPES[byte as usize],
            _ => continue,
        };
        if run_start < index {
            // Every escaped byte is ASCII, so both ends are char boundaries.
            output.write(&value[run_start..index]);
        }
        output.write(escape);
        run_start = index + 1;
    }
    if run_start < value.len() {
        output.write(&value[run_start..]);
    }
    output.write("\"");
}

/// Write a JSON number without allocating for the common integral cases.
///
/// `serde_json::Number`'s `Display` renders `u64`/`i64` payloads as plain
/// decimal, so the stack-formatted digits are identical; anything else (float
/// payloads) falls back to the owned rendering.
fn write_json_number(number: &serde_json::Number, output: &mut impl CanonicalSink) {
    if let Some(value) = number.as_u64() {
        write_u64(value, output);
    } else if let Some(value) = number.as_i64().filter(|value| *value < 0) {
        output.write("-");
        write_u64(value.unsigned_abs(), output);
    } else {
        output.write(&number.to_string());
    }
}

fn write_u64(value: u64, output: &mut impl CanonicalSink) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    let mut remaining = value;
    loop {
        index -= 1;
        buffer[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    match std::str::from_utf8(&buffer[index..]) {
        Ok(digits) => output.write(digits),
        // Unreachable: the buffer holds ASCII digits by construction.
        Err(_) => output.write(&value.to_string()),
    }
}

/// Whether a JSON object's keys already arrive in canonical (byte-lexicographic)
/// order, in which case the entries need not be collected and sorted.
fn keys_are_canonically_ordered(values: &serde_json::Map<String, Value>) -> bool {
    let mut previous: Option<&str> = None;
    for key in values.keys() {
        if previous.is_some_and(|previous| previous > key.as_str()) {
            return false;
        }
        previous = Some(key);
    }
    true
}

fn write_canonical(value: &Value, output: &mut impl CanonicalSink) -> Result<(), DomainError> {
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
                write_canonical(value, output)?;
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
                    write_canonical(value, output)?;
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
                    write_canonical(value, output)?;
                }
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

    /// The streamed string writer must stay byte-identical to the allocating
    /// `serde_json::to_string` rendering it replaced, for every escape class.
    #[test]
    fn streamed_string_escapes_match_serde_json_for_every_scalar_byte() {
        let mut samples: Vec<String> = Vec::new();
        for code in 0u32..=0x2ff {
            if let Some(character) = char::from_u32(code) {
                samples.push(character.to_string());
                samples.push(format!("prefix{character}suffix"));
            }
        }
        samples.extend(
            [
                "",
                "plain",
                "\"",
                "\\",
                "\"\\\"",
                "back\\slash/solidus",
                "雪😀é",
                "mixed \u{0}\u{1}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{1f} tail",
                "trailing\\",
                "\u{7f}delete",
            ]
            .into_iter()
            .map(str::to_owned),
        );

        for sample in samples {
            let expected = serde_json::to_string(&sample).unwrap();
            let mut streamed = String::new();
            write_json_string(&sample, &mut streamed);
            assert_eq!(streamed, expected, "escape mismatch for {sample:?}");

            let key_object = Value::Object(
                [(sample.clone(), Value::Null)]
                    .into_iter()
                    .collect::<serde_json::Map<String, Value>>(),
            );
            assert_eq!(
                canonical_json_value(&key_object).unwrap(),
                format!("{{{expected}:null}}"),
            );
        }
    }

    /// The stack-formatted integer writer must stay byte-identical to
    /// `Number::to_string`, including the `i64::MIN` boundary.
    #[test]
    fn streamed_numbers_match_owned_number_rendering() {
        let numbers = [
            "0",
            "-0",
            "1",
            "-1",
            "9",
            "10",
            "-10",
            "18446744073709551615",
            "9223372036854775807",
            "-9223372036854775808",
            "0.0",
            "-0.5",
            "1e3",
            "-12.5",
            "1.7976931348623157e308",
        ];

        for text in numbers {
            let value: Value = serde_json::from_str(text).unwrap();
            let Value::Number(number) = &value else {
                panic!("{text} is not a JSON number");
            };
            let mut streamed = String::new();
            write_json_number(number, &mut streamed);
            assert_eq!(streamed, number.to_string(), "number mismatch for {text}");
        }
    }

    /// Objects whose keys already arrive sorted take the collect-free path; it
    /// must agree with the collect-and-sort path on the same input.
    #[test]
    fn presorted_and_unsorted_objects_canonicalize_identically() {
        let sorted: Value = serde_json::from_str(r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#)
            .expect("sorted fixture");
        let unsorted: Value = serde_json::from_str(r#"{"z":[{"a":4}],"b":{"z":3,"a":2},"a":1}"#)
            .expect("unsorted fixture");

        assert!(matches!(&sorted, Value::Object(values) if keys_are_canonically_ordered(values)));
        assert_eq!(
            canonical_json_value(&sorted).unwrap(),
            canonical_json_value(&unsorted).unwrap(),
        );
        assert_eq!(
            canonical_json_value(&sorted).unwrap(),
            r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#,
        );
    }
}
