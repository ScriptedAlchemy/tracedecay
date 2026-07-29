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

    let compatibility =
        std::fs::read_to_string("src/application/session/compatibility.rs").unwrap();
    for contract in [
        "MAX_DERIVED_TEXT_CHARS",
        "MAX_DERIVED_SNIPPET_CHARS",
        "DERIVED_TRUNCATION_MARKER",
    ] {
        assert!(
            compatibility.contains(&format!("pub const {contract}")),
            "the application session layer must own the derived-text cap contract: {contract}"
        );
    }

    let lcm_types = std::fs::read_to_string("src/sessions/lcm/types.rs").unwrap();
    assert!(
        !lcm_types.contains("pub const MAX_DERIVED_TEXT_CHARS"),
        "LCM types must re-export the derived-text caps, never redeclare them"
    );

    let lcm_raw = std::fs::read_to_string("src/sessions/lcm/raw.rs").unwrap();
    assert!(
        lcm_raw.contains("crate::application::session::compatibility::derived_text_for_index"),
        "LCM raw ingest must derive index text through the application session contract"
    );
}
