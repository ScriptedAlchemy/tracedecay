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
