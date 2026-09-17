use super::structured::{
    StructuredSanitizationError, StructuredSanitizationLimits, sanitize_structured_payload,
};

const SECRET: &str = "sk-test-123456789012345678901234";

fn limits() -> StructuredSanitizationLimits {
    StructuredSanitizationLimits::new(1_048_576, 1_048_576, 64, 16_384).expect("valid test limits")
}

#[test]
fn json_is_parsed_before_sensitive_values_are_redacted() {
    let input = format!(r#"{{"nested":{{"api_key":"{SECRET}"}},"safe":"kept"}}"#);
    let sanitized = sanitize_structured_payload(input.as_bytes(), limits())
        .expect("bounded JSON input sanitizes");
    let rendered = serde_json::to_string(sanitized.payload()).expect("render safe payload");

    assert!(!rendered.contains(SECRET));
    assert!(rendered.contains("kept"));
    assert!(sanitized.was_structurally_parsed());
}

#[test]
fn malformed_json_is_scanned_without_claiming_structural_parse() {
    let input = format!("{{\"api_key\":\"{SECRET}\"");
    let sanitized = sanitize_structured_payload(input.as_bytes(), limits())
        .expect("bounded malformed input receives a complete raw scan");
    let rendered = serde_json::to_string(sanitized.payload()).expect("render safe payload");

    assert!(!rendered.contains(SECRET));
    assert!(!sanitized.was_structurally_parsed());
}

#[test]
fn credential_bearing_object_keys_are_a_typed_quarantine_state() {
    // A key carrying credential material cannot be redacted without rewriting
    // the document's structure, so the sanitizer quarantines the payload. The
    // typed state must say so — collapsing it into "sanitizer unavailable"
    // made real quarantines surface as construction faults downstream.
    let input = format!(r#"{{"{SECRET}":"ordinary-value"}}"#);
    let quarantined = sanitize_structured_payload(input.as_bytes(), limits());
    assert_eq!(
        quarantined.unwrap_err(),
        StructuredSanitizationError::CredentialKeyQuarantine
    );
}

#[test]
fn structured_limits_deny_raw_expansion_depth_and_item_overruns() {
    let raw = sanitize_structured_payload(
        br#"{"safe":"payload"}"#,
        StructuredSanitizationLimits::new(8, 128, 8, 8).expect("valid limits"),
    );
    assert_eq!(
        raw.unwrap_err(),
        StructuredSanitizationError::RawBytesExceeded
    );

    let expanded = sanitize_structured_payload(
        br#"{"value":"payload"}"#,
        StructuredSanitizationLimits::new(128, 8, 8, 8).expect("valid limits"),
    );
    assert_eq!(
        expanded.unwrap_err(),
        StructuredSanitizationError::ExpandedBytesExceeded
    );

    let depth = sanitize_structured_payload(
        br#"{"one":{"two":{"three":true}}}"#,
        StructuredSanitizationLimits::new(128, 128, 2, 16).expect("valid limits"),
    );
    assert_eq!(
        depth.unwrap_err(),
        StructuredSanitizationError::NestingDepthExceeded
    );

    let items = sanitize_structured_payload(
        br#"{"items":[1,2,3,4]}"#,
        StructuredSanitizationLimits::new(128, 128, 8, 3).expect("valid limits"),
    );
    assert_eq!(
        items.unwrap_err(),
        StructuredSanitizationError::ItemCountExceeded
    );
}

/// Pins the format a table-headed document is classified as, separately from
/// what the sanitizer then does with it. The sanitizer reports "no format" both
/// when nothing claimed the document and when it was quarantined, so a failure
/// at that level cannot say which happened; this asserts the parse outcome
/// itself.
#[test]
fn table_headed_documents_are_classified_as_toml() {
    let text = "[vault]\nregion = \"us-east\"\nvault_passphrase = \"placeholder\"\n";

    let outcome = super::structured::parse_structured_text(text)
        .map(|parsed| parsed.map(|parsed| parsed.format));

    assert_eq!(
        outcome,
        Ok(Some(super::structured::StructuredTextFormatV1::Toml))
    );
}
