use tracedecay::sessions::lcm::security::should_externalize;
use tracedecay::sessions::lcm::security::{heartbeat_noise_reason, quarantine_reason};
use tracedecay::sessions::lcm::security::{ignore_message_reason, pattern_matches};

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

// Mirrors hermes-lcm `_GENERIC_BASE64_MIN_CHARS = 4096` and
// `looks_like_long_base64` (ingest_protection.py:103,525-547): a run from the
// base64/base64url alphabet externalizes at 4096 chars when its length mod 4
// is not 1 and it has at least 8 distinct non-padding characters.
#[test]
fn long_base64_runs_externalize_at_hermes_4096_threshold() {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let run_4096 = alphabet.repeat(64);
    assert_eq!(run_4096.len(), 4096);
    assert!(should_externalize("assistant", Some("message"), &run_4096));
    assert!(should_externalize(
        "assistant",
        Some("message"),
        &format!("log prefix {} log suffix", alphabet.repeat(80))
    ));

    // base64url runs (`-`/`_`) qualify like in Hermes' `_BASE64_RUN_RE`.
    let urlsafe = "abcdefgh0123-_".repeat(300);
    assert_eq!(urlsafe.len() % 4, 0);
    assert!(should_externalize("assistant", Some("message"), &urlsafe));

    // Below the Hermes minimum stays inline.
    assert!(!should_externalize(
        "assistant",
        Some("message"),
        &run_4096[..4092]
    ));

    // Hermes rejects runs whose length mod 4 == 1 (cannot be base64).
    let run_4097 = format!("{run_4096}A");
    assert_eq!(run_4097.len() % 4, 1);
    assert!(!should_externalize("assistant", Some("message"), &run_4097));

    // Low-diversity runs (a repeated character) are not base64 payloads.
    assert!(!should_externalize(
        "assistant",
        Some("message"),
        &"Q".repeat(8_192)
    ));
}

// Mirrors hermes-lcm `_DATA_URI_BASE64_RE` (ingest_protection.py:82-87): a
// data URI only externalizes when it carries `;base64,` plus at least 256
// payload characters; JSON-escaped slashes still match.
#[test]
fn data_uri_externalization_requires_base64_payload_minimum() {
    let tiny = format!("data:image/png;base64,{}", "A".repeat(255));
    assert!(!should_externalize("assistant", Some("message"), &tiny));

    let minimum = format!("data:image/png;base64,{}", "A".repeat(256));
    assert!(should_externalize("assistant", Some("message"), &minimum));

    // Non-base64 data URIs (comma but no `;base64,`) stay inline.
    let plain = format!("data:text/plain,{}", "hello%20world".repeat(40));
    assert!(!should_externalize("assistant", Some("message"), &plain));

    // JSON-escaped slashes in the mime type still match the Hermes regex.
    let escaped = format!("data:image\\/png;base64,{}", "A".repeat(300));
    assert!(should_externalize("assistant", Some("message"), &escaped));
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
    assert!(pattern_matches("tmp-session-*", "tmp-session-a"));
    assert!(!pattern_matches("cron-*", "interactive-20260414"));
    assert!(pattern_matches("cron:*", "cron:nightly"));
    assert!(!pattern_matches("cron:*", "cron:nightly:run-1"));
    assert!(pattern_matches("cron:**", "cron:nightly:run-1"));
}

#[test]
fn ignore_message_reason_classifies_only_configured_noise_without_body_leakage() {
    assert_eq!(
        ignore_message_reason("assistant", "Still working...", &Vec::<String>::new()),
        None
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
fn ignore_message_patterns_use_regex_search_with_anchors_and_inline_flags() {
    assert_eq!(
        ignore_message_reason(
            "user",
            "Cronjob Response: noisy heartbeat",
            &["^Cronjob Response:"]
        ),
        Some("ignore_message_pattern")
    );
    assert_eq!(
        ignore_message_reason(
            "user",
            "prefix Cronjob Response: quoted text",
            &["^Cronjob Response:"]
        ),
        None
    );
    assert_eq!(
        ignore_message_reason(
            "user",
            "  >>> Cronjob Response: noisy heartbeat",
            &[r"(?is)^\s*(>>>\s*)?Cronjob Response"]
        ),
        Some("ignore_message_pattern")
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
