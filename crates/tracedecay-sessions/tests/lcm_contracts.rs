use tracedecay_sessions::lcm::contracts::{LcmError, validate_payload_ref};

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
