use tokensave::sessions::lcm::security::should_externalize;

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
