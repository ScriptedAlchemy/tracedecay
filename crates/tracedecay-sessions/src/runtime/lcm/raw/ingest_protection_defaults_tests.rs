use super::{BUILT_IN_SENSITIVE_PATTERNS, IngestProtectionDefaults, ingest_config};
use crate::host_ports::LcmRedactionPolicy;

fn profile(enabled: bool, patterns: &[&str]) -> IngestProtectionDefaults {
    IngestProtectionDefaults::from_policy(&LcmRedactionPolicy {
        enabled,
        patterns: patterns
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect(),
    })
}

#[test]
fn default_profile_leaves_redaction_off() {
    let config = ingest_config(None, &IngestProtectionDefaults::default()).unwrap();
    assert!(!config.sensitive_patterns_enabled);
}

#[test]
fn profile_setting_enables_redaction_without_a_metadata_key() {
    let config = ingest_config(None, &profile(true, &[])).unwrap();
    assert!(config.sensitive_patterns_enabled);
    assert_eq!(config.sensitive_patterns, BUILT_IN_SENSITIVE_PATTERNS);
}

#[test]
fn profile_patterns_restrict_the_redactor_set() {
    let config = ingest_config(None, &profile(true, &["API_KEY"])).unwrap();
    assert_eq!(config.sensitive_patterns, vec!["api_key".to_string()]);
}

#[test]
fn unsupported_profile_pattern_is_a_typed_sanitization_failure() {
    assert!(matches!(
        ingest_config(None, &profile(true, &["private_kye"])),
        Err(super::LcmError::Sanitization(_))
    ));
}

#[test]
fn message_metadata_still_overrides_the_profile_in_both_directions() {
    let off = ingest_config(
        Some(r#"{"lcm_ingest":{"sensitive_patterns_enabled":false}}"#),
        &profile(true, &[]),
    )
    .unwrap();
    assert!(!off.sensitive_patterns_enabled);
    let on = ingest_config(
        Some(r#"{"lcm_ingest":{"sensitive_patterns_enabled":true}}"#),
        &profile(false, &[]),
    )
    .unwrap();
    assert!(on.sensitive_patterns_enabled);
}

#[test]
fn enabled_profile_redacts_an_api_key_assignment() {
    let config = ingest_config(None, &profile(true, &[])).unwrap();
    let outcome = super::redact_sensitive_text("api_key=sk-liveSECRETVALUE123", &config);
    assert!(outcome.redacted, "profile-enabled redaction must fire");
    assert!(
        !outcome.text.contains("sk-liveSECRETVALUE123"),
        "secret survived redaction: {}",
        outcome.text
    );
}

#[test]
fn enabled_profile_redacts_an_unterminated_private_key() {
    let config = ingest_config(None, &profile(true, &["private_key"])).unwrap();
    let secret = "-----BEGIN PRIVATE KEY-----\nUNTERMINATEDPRIVATEKEYCANARY";
    let outcome = super::redact_sensitive_text(secret, &config);
    assert!(outcome.redacted);
    assert!(!outcome.text.contains("BEGIN PRIVATE KEY"));
    assert!(!outcome.text.contains("UNTERMINATEDPRIVATEKEYCANARY"));
    assert_eq!(outcome.patterns, vec!["private_key"]);
}
