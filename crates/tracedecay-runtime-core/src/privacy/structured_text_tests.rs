//! Parse-before-scan and typed-assessment contract tests.
//!
//! Every format case here uses the same placeholder value under a
//! semantically sensitive key. The value is deliberately ordinary — no
//! credential prefix, no digits, below the entropy floor — and every case first
//! asserts that the pre-existing raw sweep leaves it untouched. A case only
//! passes because the payload was parsed and the *field* was understood.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::ParseLimits;
use super::assessment::{
    SanitizationAssessmentV1, SanitizationComparisonSetV1, SanitizationHeuristicScaleV1,
    SanitizationRankComponentV1, SanitizationScaleRevisionV1,
};
use super::detect::{
    DetectionConfidenceV1, DetectionError, PrivacyDetectorV1, SanitizationActionV1,
    SanitizationFindingV1, redact_sensitive_values,
};
use super::structured::StructuredTextFormatV1;
use super::structured_text::{
    CodeSourceShapeV1, sanitize_code_source_bytes, sanitize_lcm_payload_text,
    sanitize_provider_metadata_text, sanitize_structured_text,
};

mod code_shape;

/// Ordinary words: no credential prefix, no digits, shorter than the entropy
/// floor, and containing no substring the credential patterns know.
const PLACEHOLDER: &str = "unit-test-placeholder-value";

fn raw_sweep(text: &str) -> String {
    redact_sensitive_values(Value::String(text.to_owned()), &BTreeSet::new())
        .expect("raw sweep runs")
        .payload
        .as_str()
        .expect("raw sweep keeps text")
        .to_owned()
}

fn assert_detected_only_by_parsing(raw: &str, expected: StructuredTextFormatV1) {
    assert!(
        raw_sweep(raw).contains(PLACEHOLDER),
        "the raw sweep already caught this value, so the parse proves nothing"
    );

    let scanned = sanitize_structured_text(raw).expect("structured scan runs");
    assert_eq!(scanned.format(), Some(expected));
    assert!(
        !scanned.sanitized_text().contains(PLACEHOLDER),
        "parsed sensitive field survived sanitization: {}",
        scanned.sanitized_text()
    );
    assert!(
        scanned
            .findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField),
        "no sensitive-field finding was reported"
    );
    assert!(
        !scanned
            .findings()
            .iter()
            .any(|finding| serde_json::to_string(finding)
                .expect("serialize finding")
                .contains(PLACEHOLDER)),
        "a finding reproduced the detected value"
    );

    let replayed = sanitize_structured_text(scanned.sanitized_text()).expect("replay scan runs");
    assert_eq!(
        replayed.sanitized_text(),
        scanned.sanitized_text(),
        "sanitizing the sanitized payload must be a fixed point"
    );
}

#[test]
fn yaml_sensitive_field_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!("vault:\n  vault_passphrase: {PLACEHOLDER}\n  region: us-east\n"),
        StructuredTextFormatV1::Yaml,
    );
}

#[test]
fn yaml_document_markers_do_not_hide_sensitive_fields_from_the_format_probe() {
    assert_detected_only_by_parsing(
        &format!("---\nvault_passphrase: {PLACEHOLDER}\n...\n"),
        StructuredTextFormatV1::Yaml,
    );
}

#[test]
fn yaml_tags_reach_the_canonical_parser_before_sensitive_field_scanning() {
    assert_detected_only_by_parsing(
        &format!("---\nprovider: !ProviderConfig\n  vault_passphrase: {PLACEHOLDER}\n"),
        StructuredTextFormatV1::Yaml,
    );
}

#[test]
fn toml_sensitive_field_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!("[vault]\nregion = \"us-east\"\nvault_passphrase = \"{PLACEHOLDER}\"\n"),
        StructuredTextFormatV1::Toml,
    );
}

#[test]
fn dotenv_sensitive_field_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!("# deployment\nREGION=us-east\nVAULT_PASSPHRASE={PLACEHOLDER}\n"),
        StructuredTextFormatV1::Dotenv,
    );
}

#[test]
fn url_query_sensitive_field_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!("https://example.test/callback?region=us-east&vault_passphrase={PLACEHOLDER}"),
        StructuredTextFormatV1::Url,
    );
}

#[test]
fn http_header_sensitive_field_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!(
            "GET /v1/session HTTP/1.1\nHost: example.test\nX-Vault_Passphrase: {PLACEHOLDER}\n"
        ),
        StructuredTextFormatV1::HttpHeaders,
    );
}

#[test]
fn url_userinfo_password_is_detected_only_after_parsing() {
    assert_detected_only_by_parsing(
        &format!("postgres://service:{PLACEHOLDER}@db.example.test/app"),
        StructuredTextFormatV1::Url,
    );
}

#[test]
fn decoded_url_values_are_inspected_as_well_as_raw_bytes() {
    // Percent encoding hides the separator the bearer pattern needs, so the raw
    // bytes carry no match. Only the decoded value does, and the key is not
    // sensitive, so nothing but decoding can find this.
    let encoded = "Bearer%20AAAABBBBCCCCDDDD";
    let raw = format!("https://example.test/callback?state={encoded}");
    assert!(raw_sweep(&raw).contains(encoded));

    let scanned = sanitize_structured_text(&raw).expect("structured scan runs");
    assert_eq!(scanned.format(), Some(StructuredTextFormatV1::Url));
    assert!(!scanned.sanitized_text().contains(encoded));
    assert!(
        scanned
            .findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField)
    );
}

#[test]
fn percent_encoded_url_query_keys_are_classified_before_redaction() {
    let raw =
        format!("https://example.test/callback?vault%5Fpassphrase={PLACEHOLDER}&region=us-east");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    let scanned = sanitize_structured_text(&raw).expect("structured scan runs");

    assert_eq!(scanned.format(), Some(StructuredTextFormatV1::Url));
    assert!(!scanned.sanitized_text().contains(PLACEHOLDER));
    assert!(scanned.sanitized_text().contains("region=us-east"));
}

#[test]
fn malformed_structured_input_is_quarantined_after_a_best_effort_raw_redaction() {
    let credential = ["sk", "-test-", "1234567890abcdef"].concat();
    let raw = format!("vault:\n  rotation: {credential}\n  broken: [unclosed\n");

    let scanned = sanitize_structured_text(&raw).expect("structured scan runs");

    assert_eq!(
        scanned.format(),
        None,
        "input that does not parse whole must not claim a structural parse"
    );
    assert!(!scanned.sanitized_text().contains(&credential));
    assert!(
        scanned
            .quarantine_findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::MalformedRecord),
        "malformed structured input must not downgrade to a raw-only scan"
    );
    assert!(
        scanned
            .findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::ExactCredential)
    );
}

fn assert_structured_input_is_quarantined(raw: &str) {
    let scanned = sanitize_structured_text(raw).expect("structured scan returns quarantine");
    assert!(
        scanned.quarantine_findings().iter().any(|finding| {
            finding.detector() == PrivacyDetectorV1::MalformedRecord
                && finding.action() == SanitizationActionV1::Quarantined
        }),
        "structured-looking input must not downgrade to a raw-only scan"
    );
    assert!(
        sanitize_provider_metadata_text(raw).is_none(),
        "quarantined provider metadata must not be persisted"
    );
}

#[test]
fn malformed_yaml_with_an_ordinary_sensitive_value_is_quarantined() {
    let raw = format!("vault_passphrase: {PLACEHOLDER}\n  broken: [unclosed\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn malformed_toml_table_with_an_ordinary_sensitive_value_is_quarantined() {
    let raw = format!("[vault]\nvault_passphrase = {PLACEHOLDER}\nbroken = [\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn malformed_dotenv_assignment_with_an_ordinary_sensitive_value_is_quarantined() {
    let raw = format!("VAULT_PASSPHRASE={PLACEHOLDER}\nthis is not an assignment\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn malformed_toml_assignment_with_an_ordinary_sensitive_value_is_quarantined() {
    let raw = format!("vault_passphrase = \"{PLACEHOLDER}\"\nthis is not TOML\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn duplicate_json_sensitive_keys_are_quarantined_before_value_materialization() {
    let raw =
        format!(r#"{{"vault_passphrase":"{PLACEHOLDER}","vault_passphrase":"safe-replacement"}}"#);
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn duplicate_yaml_sensitive_keys_are_quarantined_before_value_materialization() {
    let raw = format!("vault_passphrase: {PLACEHOLDER}\nvault_passphrase: safe-replacement\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    assert_structured_input_is_quarantined(&raw);
}

#[test]
fn yaml_decoy_comment_mentioning_the_key_does_not_redirect_the_redaction_span() {
    // Line 1 is a decoy: it mentions the key name in a comment, well before
    // the real `vault_passphrase:` assignment. An unanchored `raw.find(key)`
    // would match the decoy and redact its (harmless) comment line while the
    // real value — a YAML folded scalar spread across the following two
    // lines — sails through untouched. A folded scalar's decoded value
    // differs from its raw bytes (folding turns newlines into spaces), so
    // byte-for-byte value location also fails and the key-line-tail
    // fallback is the only thing standing between this secret and the
    // sanitized output. The fallback cannot redact content that lives on
    // lines after the key's own line, so the correct outcome is quarantine,
    // never a silent redaction of the wrong (decoy) span.
    let raw = "# rotate the vault_passphrase monthly\nvault_passphrase: >\n  line-one-of-secret\n  line-two-of-secret\nregion: us-east\n";

    let scanned = sanitize_structured_text(raw).expect("structured scan runs");
    assert!(
        scanned
            .quarantine_findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField),
        "an unlocatable sensitive field must be quarantined, not silently marked redacted"
    );
    assert!(
        !scanned
            .findings()
            .iter()
            .any(|finding| finding.action() == SanitizationActionV1::Redacted
                && finding.detector() == PrivacyDetectorV1::SensitiveField),
        "the decoy must never be reported as a successful redaction of the real field"
    );
    assert!(
        sanitize_provider_metadata_text(raw).is_none(),
        "a field the parser cannot precisely locate must fail closed, not leak the folded secret"
    );
}

#[test]
fn json_decoy_key_mentioned_in_an_earlier_string_value_does_not_redirect_the_redaction_span() {
    // "note" is a decoy: it mentions the key name inside an earlier string
    // value, well before the real `"vault_passphrase"` assignment. JSON
    // escapes the real value's embedded newline as `\n` (two ASCII bytes),
    // so the decoded value never matches the raw bytes byte-for-byte and
    // location falls back to the key-line-tail heuristic. An unanchored
    // search would match the decoy inside "note" and redact only that line,
    // leaving the real secret on the next line completely untouched.
    let raw = "{\n  \"note\": \"remember to rotate the vault_passphrase weekly\",\n  \"vault_passphrase\": \"line-one-marker\\nline-two-marker\"\n}\n";

    let scanned = sanitize_structured_text(raw).expect("structured scan runs");
    assert_eq!(scanned.format(), Some(StructuredTextFormatV1::Json));
    assert!(
        !scanned.sanitized_text().contains("line-one-marker"),
        "the decoy key mention must not redirect redaction away from the real secret: {}",
        scanned.sanitized_text()
    );
    assert!(
        scanned
            .sanitized_text()
            .contains("rotate the vault_passphrase weekly"),
        "the decoy string is not itself sensitive and must survive sanitization: {}",
        scanned.sanitized_text()
    );
    assert!(
        scanned
            .findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField),
        "the real field must still be reported as a sensitive-field finding"
    );
}

#[test]
fn lcm_json_duplicate_keys_are_rejected_before_value_materialization() {
    let raw =
        format!(r#"{{"vault_passphrase":"{PLACEHOLDER}","vault_passphrase":"safe-replacement"}}"#);

    assert!(matches!(
        sanitize_lcm_payload_text(&raw),
        Err(DetectionError::StructuredQuarantine)
    ));
}

#[test]
fn json_preflight_rejects_depth_beyond_the_canonical_parse_limit() {
    let limits = ParseLimits::default_policy();
    let mut raw = String::new();
    for _ in 0..=limits.depth {
        raw.push_str(r#"{"node":"#);
    }
    raw.push_str("true");
    for _ in 0..=limits.depth {
        raw.push('}');
    }

    assert!(matches!(
        sanitize_structured_text(&raw),
        Err(DetectionError::ScanLimitExceeded)
    ));
    assert!(sanitize_provider_metadata_text(&raw).is_none());
}

#[test]
fn json_preflight_rejects_item_counts_beyond_the_canonical_parse_limit() {
    let limits = ParseLimits::default_policy();
    let mut raw = String::with_capacity(limits.values.saturating_mul(2));
    raw.push('[');
    for index in 0..limits.values {
        if index != 0 {
            raw.push(',');
        }
        raw.push('0');
    }
    raw.push_str(",0]");

    assert!(matches!(
        sanitize_structured_text(&raw),
        Err(DetectionError::ScanLimitExceeded)
    ));
    assert!(sanitize_provider_metadata_text(&raw).is_none());
}

#[test]
fn parsed_documents_without_sensitive_fields_are_left_intact() {
    let raw = "Note: this line is ordinary prose\nSubject: nothing sensitive here\n";

    let scanned = sanitize_structured_text(raw).expect("structured scan runs");

    assert_eq!(scanned.sanitized_text(), raw);
    assert!(scanned.findings().is_empty());
}

#[test]
fn code_source_sanitizer_parses_environment_files_before_scanning() {
    let raw = format!("# service env\nREGION=us-east\nVAULT_PASSPHRASE={PLACEHOLDER}\n");
    assert!(raw_sweep(&raw).contains(PLACEHOLDER));

    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
        .expect("sanitize code source");
    let text = String::from_utf8(sanitized.sanitized_bytes().to_vec()).expect("sanitized UTF-8");

    assert!(!text.contains(PLACEHOLDER));
    assert!(
        text.contains("REGION=us-east"),
        "in-place redaction must leave the rest of the file alone: {text}"
    );
}

#[test]
fn provider_metadata_text_parses_yaml_before_sanitizing() {
    let raw = format!("provider:\n  vault_passphrase: {PLACEHOLDER}\n  region: us-east\n");
    assert!(
        raw_sweep(&raw).contains(PLACEHOLDER),
        "the raw sweep already caught this value, so the parse proves nothing"
    );

    let sanitized =
        sanitize_provider_metadata_text(&raw).expect("provider metadata sanitizer accepts YAML");

    assert!(!sanitized.contains(PLACEHOLDER));
    assert!(sanitized.contains("region: us-east"));
}

#[test]
fn structured_findings_carry_a_ranked_typed_assessment() {
    let raw = format!("vault_passphrase: {PLACEHOLDER}\nservice_token: short\nregion: us-east\n");

    let scanned = sanitize_structured_text(&raw).expect("structured scan runs");
    let ranks: Vec<_> = scanned
        .findings()
        .iter()
        .filter_map(SanitizationFindingV1::assessment)
        .cloned()
        .collect();

    assert_eq!(ranks.len(), 2, "both sensitive fields need an assessment");
    for assessment in &ranks {
        let SanitizationAssessmentV1::OrdinalRank {
            comparison_set,
            components,
            rank,
            of,
        } = assessment
        else {
            panic!("structured sensitive fields rank, they do not score");
        };
        assert_eq!(
            *comparison_set,
            SanitizationComparisonSetV1::StructuredDocumentFields
        );
        assert!(components.contains(&SanitizationRankComponentV1::KeySemantics));
        assert!(components.contains(&SanitizationRankComponentV1::ValueLength));
        assert_eq!(*of, 2);
        assert!((1..=*of).contains(rank));
    }
    assert_ne!(
        ranks[0], ranks[1],
        "a comparison set must order its members"
    );
}

#[test]
fn entropy_findings_carry_a_named_versioned_heuristic_scale() {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let token: String = (0..48)
        .map(|index| char::from(ALPHABET[(index * 17) % ALPHABET.len()]))
        .collect();

    let scanned = sanitize_structured_text(&token).expect("structured scan runs");
    let finding = scanned
        .findings()
        .iter()
        .find(|finding| finding.detector() == PrivacyDetectorV1::HighEntropyToken)
        .expect("entropy finding");

    let Some(SanitizationAssessmentV1::HeuristicScore {
        scale,
        scale_revision,
        score_per_mille,
    }) = finding.assessment()
    else {
        panic!("a heuristic detector must name the scale it scored on");
    };
    assert_eq!(
        *scale,
        SanitizationHeuristicScaleV1::ShannonEntropyBitsPerCharacter
    );
    assert_eq!(*scale_revision, SanitizationScaleRevisionV1::V1);
    assert!(
        *score_per_mille >= 4_200,
        "the score must clear the detector's own threshold: {score_per_mille}"
    );
    assert!(*score_per_mille <= 8_000);
}

fn finding_wire(confidence: DetectionConfidenceV1) -> Value {
    serde_json::to_value(SanitizationFindingV1::new(
        PrivacyDetectorV1::SensitiveField,
        "$",
        confidence,
        SanitizationActionV1::Redacted,
    ))
    .expect("serialize finding")
}

fn with_assessment(confidence: DetectionConfidenceV1, assessment: Value) -> Value {
    let mut wire = finding_wire(confidence);
    wire["assessment"] = assessment;
    wire
}

fn accepts(wire: Value) -> bool {
    serde_json::from_value::<SanitizationFindingV1>(wire).is_ok()
}

#[test]
fn wire_contract_rejects_assessments_that_outrun_their_evidence() {
    let valid_profile = json!({
        "cohort": "sensitive_field",
        "horizon_days": 30,
        "support": 512,
        "error_per_mille": 40,
        "drift": "valid",
    });

    assert!(accepts(with_assessment(
        DetectionConfidenceV1::Contextual,
        json!({"calibrated_probability": {
            "profile": valid_profile,
            "probability_per_mille": 900,
        }}),
    )));

    for (label, profile) in [
        (
            "under-supported",
            json!({"cohort": "sensitive_field", "horizon_days": 30, "support": 8, "error_per_mille": 40, "drift": "valid"}),
        ),
        (
            "stale",
            json!({"cohort": "sensitive_field", "horizon_days": 30, "support": 512, "error_per_mille": 40, "drift": "stale"}),
        ),
        (
            "shifted",
            json!({"cohort": "sensitive_field", "horizon_days": 30, "support": 512, "error_per_mille": 40, "drift": "shifted"}),
        ),
        (
            "horizonless",
            json!({"cohort": "sensitive_field", "horizon_days": 0, "support": 512, "error_per_mille": 40, "drift": "valid"}),
        ),
        (
            "over-error",
            json!({"cohort": "sensitive_field", "horizon_days": 30, "support": 512, "error_per_mille": 400, "drift": "valid"}),
        ),
    ] {
        assert!(
            !accepts(with_assessment(
                DetectionConfidenceV1::Contextual,
                json!({"calibrated_probability": {
                    "profile": profile,
                    "probability_per_mille": 900,
                }}),
            )),
            "{label} calibration must not produce a probability"
        );
    }

    assert!(
        !accepts(with_assessment(
            DetectionConfidenceV1::Contextual,
            json!({"calibrated_interval": {
                "profile": valid_profile,
                "low_per_mille": 800,
                "high_per_mille": 200,
            }}),
        )),
        "an inverted interval is not an interval"
    );
}

#[test]
fn wire_contract_rejects_heuristic_and_rank_claims_without_their_scale_or_set() {
    assert!(accepts(with_assessment(
        DetectionConfidenceV1::Heuristic,
        json!({"heuristic_score": {
            "scale": "shannon_entropy_bits_per_character",
            "scale_revision": "v1",
            "score_per_mille": 5_000,
        }}),
    )));

    assert!(
        !accepts(with_assessment(
            DetectionConfidenceV1::Exact,
            json!({"heuristic_score": {
                "scale": "shannon_entropy_bits_per_character",
                "scale_revision": "v1",
                "score_per_mille": 5_000,
            }}),
        )),
        "an exact detector may not publish a heuristic score"
    );

    assert!(
        !accepts(with_assessment(
            DetectionConfidenceV1::Heuristic,
            json!({"heuristic_score": {
                "scale": "shannon_entropy_bits_per_character",
                "scale_revision": "v1",
                "score_per_mille": 9_000,
            }}),
        )),
        "a score outside its named scale is not on that scale"
    );

    assert!(
        !accepts(with_assessment(
            DetectionConfidenceV1::Heuristic,
            json!({"heuristic_score": {
                "scale": "shannon_entropy_bits_per_character",
                "score_per_mille": 5_000,
            }}),
        )),
        "a heuristic score must name its scale revision"
    );

    assert!(accepts(with_assessment(
        DetectionConfidenceV1::Contextual,
        json!({"ordinal_rank": {
            "comparison_set": "structured_document_fields",
            "components": ["key_semantics", "value_length"],
            "rank": 1,
            "of": 3,
        }}),
    )));

    for (label, rank) in [
        (
            "componentless",
            json!({"comparison_set": "structured_document_fields", "components": [], "rank": 1, "of": 3}),
        ),
        (
            "out of set",
            json!({"comparison_set": "structured_document_fields", "components": ["key_semantics"], "rank": 4, "of": 3}),
        ),
        (
            "non-canonical components",
            json!({"comparison_set": "structured_document_fields", "components": ["value_length", "key_semantics"], "rank": 1, "of": 3}),
        ),
        (
            "duplicated components",
            json!({"comparison_set": "structured_document_fields", "components": ["key_semantics", "key_semantics"], "rank": 1, "of": 3}),
        ),
    ] {
        assert!(
            !accepts(with_assessment(
                DetectionConfidenceV1::Contextual,
                json!({ "ordinal_rank": rank }),
            )),
            "{label} rank must fail closed"
        );
    }
}
