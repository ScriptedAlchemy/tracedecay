//! Object keys that trip a detector are rewritten in place, never exempted.
//!
//! Detection stays exactly as strict for keys as for values and prose: a key
//! that reads as a high-entropy token or a credential is replaced by a
//! placeholder unique within its object, and every scalar under it is
//! redacted. The document keeps its format, so the rest of it stays indexable.

use serde_json::Value;
use tracedecay_domain::SanitizerDispositionV1;

use super::{
    CodeSourceShapeV1, DetectionError, StructuredTextFormatV1, sanitize_code_source_bytes,
    sanitize_lcm_payload_text, sanitize_structured_text,
};

/// Passphrase-shaped secrets that read like identifiers. No shape rule can
/// tell them apart from long type or field names, so they must stay redacted
/// in every position.
const PASSPHRASES: [&str; 3] = [
    "violin-pocket-jungle-quartz-island-778",
    "RocketButtonStapleBridgeYellowOrange90",
    "MySuperSecretPasswordForProduction2024Admin",
];

fn sanitize_data(raw: &str) -> String {
    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
        .unwrap_or_else(|error| panic!("structured data sanitizes in place ({error}): {raw}"));
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    String::from_utf8(sanitized.into_parts().0).expect("sanitized text is UTF-8")
}

fn assert_fixed_point(sanitized: &str) {
    let replayed = sanitize_structured_text(sanitized).expect("replay scan runs");
    assert!(replayed.quarantine_findings().is_empty());
    assert_eq!(
        replayed.sanitized_text(),
        sanitized,
        "sanitizing the sanitized payload must be a fixed point"
    );
}

#[test]
fn json_secret_keys_become_placeholders_and_their_values_are_redacted() {
    for secret in PASSPHRASES {
        let raw = format!(r#"{{"api_keys": {{"{secret}": "admin-user"}}, "region": "us-east"}}"#);

        let text = sanitize_data(&raw);

        assert!(!text.contains(secret), "secret key survived: {text}");
        assert!(
            !text.contains("admin-user"),
            "value under it survived: {text}"
        );
        let parsed: Value = serde_json::from_str(&text).expect("still valid JSON");
        assert_eq!(parsed["region"], "us-east");
        assert_eq!(
            parsed["api_keys"]["TraceDecay-redacted-key-1"],
            "TraceDecay-redacted-sensitive-field"
        );
        assert_fixed_point(&text);
    }
}

#[test]
fn placeholders_are_unique_within_their_object() {
    let [first, second, third] = PASSPHRASES;
    let raw = format!(
        "{{\n  \"TraceDecay-redacted-key-1\": \"kept\",\n  \"{first}\": 1,\n  \
         \"{second}\": {{\"nested\": \"x\"}},\n  \"inner\": {{\"{third}\": true}}\n}}\n"
    );

    let text = sanitize_data(&raw);

    for secret in PASSPHRASES {
        assert!(!text.contains(secret), "secret key survived: {text}");
    }
    let parsed: Value = serde_json::from_str(&text).expect("still valid JSON");
    let keys: Vec<&str> = parsed
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "TraceDecay-redacted-key-1",
            "TraceDecay-redacted-key-2",
            "TraceDecay-redacted-key-3",
            "inner"
        ],
        "{text}"
    );
    assert_eq!(parsed["TraceDecay-redacted-key-1"], "kept");
    assert_eq!(
        parsed["TraceDecay-redacted-key-3"]["nested"],
        "TraceDecay-redacted-sensitive-field"
    );
    assert_eq!(parsed["inner"]["TraceDecay-redacted-key-1"], true);
    assert_fixed_point(&text);
}

#[test]
fn schema_definitions_with_long_type_names_stay_indexable() {
    let raw = r##"{
  "$defs": {
    "WorkDuplicateAdjudicationAppendOutcomeV1": {"type": "string", "enum": ["appended"]},
    "ProjectId": {"description": "Canonical project identity.", "type": "string"}
  },
  "properties": {"outcome": {"$ref": "#/$defs/ProjectId"}}
}
"##;

    let text = sanitize_data(raw);

    assert!(
        !text.contains("WorkDuplicateAdjudicationAppendOutcomeV1"),
        "{text}"
    );
    assert!(
        text.contains(
            r#""ProjectId": {"description": "Canonical project identity.", "type": "string"}"#
        ),
        "ordinary definitions are untouched: {text}"
    );
    assert!(text.contains(r##""$ref": "#/$defs/ProjectId""##), "{text}");
    assert!(
        text.contains(r#""TraceDecay-redacted-key-1": {"type""#),
        "{text}"
    );
    serde_json::from_str::<Value>(&text).expect("still valid JSON");
}

#[test]
fn benchmark_receipt_with_a_long_target_name_stays_indexable() {
    let raw = r#"{"target_results": {"clone_peak_rss_added_at_most_1_gib_for_39582_symbols": true, "cold_index_completed": true}}"#;

    let text = sanitize_data(raw);

    assert_eq!(
        text,
        r#"{"target_results": {"TraceDecay-redacted-key-1": true, "cold_index_completed": true}}"#
    );
}

#[test]
fn escaped_json_keys_are_replaced_byte_exactly() {
    let raw = r#"{"Violin-pocket-jungle-quartz-island-778": "x", "ok": "y"}"#;

    let text = sanitize_data(raw);

    assert_eq!(
        text,
        r#"{"TraceDecay-redacted-key-1": "TraceDecay-redacted-sensitive-field", "ok": "y"}"#
    );
}

#[test]
fn nested_escaped_json_keys_are_redacted_under_a_flagged_parent() {
    let secret = PASSPHRASES[0];
    let raw = format!(
        r#"{{"{secret}":{{"violin-pocket-ju\u006egle-quartz-island-778":true,"ordinary":"private-value"}}}}"#
    );
    let original: Value = serde_json::from_str(&raw).expect("real JSON with an escaped nested key");
    assert_eq!(original[secret][secret], true);

    let text = sanitize_data(&raw);
    let parsed: Value = serde_json::from_str(&text).expect("redaction keeps valid JSON");
    assert!(
        !serde_json::to_string(&parsed).unwrap().contains(secret),
        "{text}"
    );
    assert!(
        !text.contains("private-value"),
        "forced value redaction survives: {text}"
    );
    assert_fixed_point(&text);
}

fn assert_repeated_toml_keys_are_redacted_or_withheld(raw: &str) {
    let original: toml::Value = toml::from_str(raw).expect("valid repeated TOML path components");
    assert!(original.as_table().unwrap().contains_key(PASSPHRASES[0]));
    let sanitized =
        match sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData) {
            Ok(sanitized) => sanitized,
            Err(DetectionError::CredentialKeyQuarantine) => {
                return;
            }
            Err(error) => panic!("expected exact redaction or typed key quarantine: {error}"),
        };
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    let text = String::from_utf8(sanitized.into_parts().0).unwrap();
    let parsed: toml::Value = toml::from_str(&text).expect("redacted TOML remains valid");
    assert!(
        !toml::to_string(&parsed).unwrap().contains(PASSPHRASES[0]),
        "{text}"
    );
    assert_fixed_point(&text);
}

#[test]
fn repeated_escaped_toml_dotted_keys_do_not_leave_decoded_secrets() {
    let raw = concat!(
        "\"violin-pocket-ju\\u006egle-quartz-island-778\".a=true\n",
        "\"violin-pocket-ju\\u006egle-quartz-island-778\".b=true\n",
    );
    assert_repeated_toml_keys_are_redacted_or_withheld(raw);
    assert_eq!(
        sanitize_lcm_payload_text(raw).map(|_| ()),
        Err(DetectionError::CredentialKeyQuarantine)
    );
}

#[test]
fn repeated_escaped_toml_quoted_headers_do_not_leave_decoded_secrets() {
    let raw = concat!(
        "[\"violin-pocket-ju\\u006egle-quartz-island-778\".a]\nx=true\n",
        "[\"violin-pocket-ju\\u006egle-quartz-island-778\".b]\nx=true\n",
    );
    assert_repeated_toml_keys_are_redacted_or_withheld(raw);
    // LCM's existing JSON-intent guard withholds a leading `["` before the
    // structured-source route gets to identify the quoted TOML header.
    assert_eq!(
        sanitize_lcm_payload_text(raw).map(|_| ()),
        Err(DetectionError::StructuredQuarantine)
    );
}

#[test]
fn toml_secret_keys_become_bare_placeholders() {
    let [first, second, _] = PASSPHRASES;
    let raw = format!(
        "[users]\n{first} = \"admin-user\"\n\"{second}\" = {{ role = \"ops\" }}\nregion = \"us-east\"\n"
    );

    let scanned = sanitize_structured_text(&raw).expect("structured scan runs");
    assert_eq!(scanned.format(), Some(StructuredTextFormatV1::Toml));
    assert!(scanned.quarantine_findings().is_empty());
    let text = scanned.sanitized_text();

    assert_eq!(
        text,
        "[users]\nTraceDecay-redacted-key-1 = \"TraceDecay-redacted-sensitive-field\"\n\
         TraceDecay-redacted-key-2 = { role = \"TraceDecay-redacted-sensitive-field\" }\n\
         region = \"us-east\"\n"
    );
    assert_fixed_point(text);
}

#[test]
fn credential_keys_redact_in_place_on_code_source_and_lcm_text_routes() {
    let credential_key = ["sk", "-test-", "1234567890abcdef"].concat();
    let raw = format!("{credential_key} = \"ordinary-value\"\nregion = \"us-east\"\n");
    let expected = "TraceDecay-redacted-key-1 = \"TraceDecay-redacted-sensitive-field\"\nregion = \"us-east\"\n";

    assert_eq!(sanitize_data(&raw), expected);
    let sanitized = sanitize_lcm_payload_text(&raw).expect("key redacts in place");
    assert_eq!(sanitized.sanitized_text(), expected);
}

/// YAML is parsed without key spans, so a key cannot be located byte-exactly
/// and the document stays withheld.
#[test]
fn yaml_secret_keys_still_quarantine() {
    for secret in PASSPHRASES {
        let raw = format!("users:\n  {secret}: admin-user\nregion: us-east\n");

        let error = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
            .map(|_| ())
            .expect_err("unlocatable key quarantines");

        assert_eq!(error, DetectionError::CredentialKeyQuarantine);
    }
}

/// A TOML table named twice has one parsed key but two occurrences. The
/// second is only raw-scanned, which breaks the header, so the redacted
/// document no longer parses and the file stays withheld.
#[test]
fn toml_secret_keys_that_cannot_be_rewritten_validly_still_quarantine() {
    let secret = PASSPHRASES[0];
    let raw = format!("[{secret}]\nname = \"a\"\n\n[{secret}.child]\nname = \"b\"\n");

    let error = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
        .map(|_| ())
        .expect_err("unrewritable key quarantines");

    assert_eq!(error, DetectionError::CredentialKeyQuarantine);
}

#[test]
fn passphrases_in_values_and_free_text_stay_redacted() {
    for secret in PASSPHRASES {
        for raw in [
            format!("{{\"note\": \"{secret}\"}}"),
            format!("{{\"url\": \"https://example.test/hooks/{secret}/run\"}}"),
        ] {
            let text = sanitize_data(&raw);
            assert!(!text.contains(secret), "{text}");
        }
        for raw in [
            format!("let label = \"{secret}\";\n"),
            format!("Remember to use {secret} when you type the password.\n"),
            format!("type Alias = {secret};\n"),
        ] {
            let sanitized =
                sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::CodeOrProse)
                    .expect("prose sanitizes");
            let text = String::from_utf8(sanitized.into_parts().0).expect("UTF-8");
            assert!(!text.contains(secret), "{text}");
        }
    }
}
