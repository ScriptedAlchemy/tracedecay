use tracedecay_sessions::lcm::contracts::{
    LcmError, LcmExpandSourcePagination, validate_payload_ref,
};

#[test]
fn payload_ref_validation_rejects_traversal_and_separators() {
    assert!(validate_payload_ref("payload_abc.payload").is_ok());

    for rejected in [
        "",
        ".",
        "..",
        "../escape",
        "../secret",
        "nested/payload",
        "nested/file",
        "nested\\payload",
        "/absolute",
        "/tmp/secret",
    ] {
        assert_eq!(
            validate_payload_ref(rejected),
            Err(LcmError::InvalidPayloadRef),
            "payload ref {rejected:?} must not pass containment"
        );
    }
}

#[test]
fn source_pagination_never_exposes_a_numeric_continuation() {
    let pagination = LcmExpandSourcePagination {
        source_offset: 2,
        source_limit: 3,
        returned_sources: 3,
        total_sources: 8,
        next_source_offset: Some(5),
        has_more: true,
        remaining_sources: 3,
    };

    let wire = serde_json::to_value(pagination).expect("pagination wire");
    assert!(
        wire.get("source_offset").is_none() && wire.get("next_source_offset").is_none(),
        "numeric cursor state must remain private; continuation is only next_cursor: {wire}"
    );
}
