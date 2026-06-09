use tokensave::sessions::lcm::security::should_externalize;
use tokensave::sessions::lcm::security::{heartbeat_noise_reason, quarantine_reason};
use tokensave::sessions::lcm::security::{ignore_message_reason, pattern_matches};

#[test]
fn classifies_data_uri_and_long_base64_for_externalization() {
    let data_uri = format!("data:image/png;base64,{}", "A".repeat(20_000));
    assert!(should_externalize(
        "assistant",
        Some("tool_result"),
        &data_uri
    ));
    assert!(should_externalize(
        "assistant",
        Some("message"),
        &format!("prefix data:image/png;base64,{} suffix", "A".repeat(20_000))
    ));

    let base64_run = "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(4_000);
    assert!(should_externalize(
        "assistant",
        Some("message"),
        &base64_run
    ));
    assert!(!should_externalize(
        "assistant",
        Some("message"),
        &"Q".repeat(80_000)
    ));

    assert!(!should_externalize(
        "assistant",
        Some("message"),
        "short useful text"
    ));
}

#[test]
fn classifies_repetitive_assistant_output_for_quarantine() {
    let repeated =
        "same repeated assistant diagnostic segment with very low novelty.\n".repeat(1_200);
    assert_eq!(
        quarantine_reason("assistant", Some("message"), &repeated),
        Some("high_repetition")
    );
    assert!(should_externalize("assistant", Some("message"), &repeated));

    let varied_report = (0..1_200)
        .map(|idx| format!("line {idx}: varied diagnostic identifier {idx:x}\n"))
        .collect::<String>();
    assert_eq!(
        quarantine_reason("assistant", Some("message"), &varied_report),
        None
    );
}

#[test]
fn heartbeat_noise_is_diagnostic_only() {
    assert_eq!(
        heartbeat_noise_reason("assistant", "Still working..."),
        Some("heartbeat_progress")
    );
    assert!(!should_externalize(
        "assistant",
        Some("message"),
        "Still working..."
    ));
    assert_eq!(heartbeat_noise_reason("user", "Still working..."), None);
}

#[test]
fn ignore_and_stateless_patterns_use_hermes_style_globs() {
    assert!(pattern_matches("cron-*", "cron-20260414"));
    assert!(pattern_matches("tmp-session-?", "tmp-session-a"));
    assert!(!pattern_matches("cron-*", "interactive-20260414"));
}

#[test]
fn ignore_message_reason_classifies_heartbeat_and_configured_noise_without_body_leakage() {
    assert_eq!(
        ignore_message_reason("assistant", "Still working...", &Vec::<String>::new()),
        Some("heartbeat_progress")
    );
    assert_eq!(
        ignore_message_reason(
            "user",
            "Cronjob Response: noisy heartbeat",
            &["Cronjob Response:*"]
        ),
        Some("ignore_message_pattern")
    );
    assert_eq!(
        ignore_message_reason("user", "real user request", &["Cronjob Response:*"]),
        None
    );
}

#[test]
fn no_authoritative_session_write_uses_legacy_text_cap() {
    let global_db = std::fs::read_to_string("src/global_db.rs").unwrap();
    assert!(
        !global_db.contains("MAX_SESSION_MESSAGE_TEXT_BYTES"),
        "authoritative session writes must not use the legacy text byte cap"
    );
    assert!(
        !global_db.contains("SESSION_MESSAGE_TRUNCATION_MARKER"),
        "authoritative session writes must not use the legacy truncation marker"
    );

    let lcm_raw = std::fs::read_to_string("src/sessions/lcm/raw.rs").unwrap();
    assert!(lcm_raw.contains("MAX_DERIVED_TEXT_CHARS"));
    assert!(lcm_raw.contains("derived_text_for_index"));
}
