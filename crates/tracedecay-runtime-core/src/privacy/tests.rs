use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalWorkflowSemanticKindV1,
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceIdentityV1, ComponentVersion, ObservationContractError, ObservationId,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1,
    PayloadReferenceV1, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId,
};

use super::detect::{
    DetectionError, SanitizationDetectorOriginV1, SanitizationDetectorRevisionV1,
    SanitizationRemediationClassV1, SanitizationScanBoundaryV1, SanitizationScannedCoverageV1,
};
use super::sanitize::{CLAUDE_SANITIZER_VERSION_V1, OBSERVATION_SANITIZER_VERSION_V1};
use super::{
    CODE_SOURCE_SANITIZER_VERSION_V1, ClaudeRecordParseErrorV1, ClaudeRecordSanitizerV1,
    ClaudeSanitizationOutcomeV1, ClaudeSanitizerPolicyV1, CodeSourceShapeV1, DetectionConfidenceV1,
    LcmSensitiveRedactionPolicyV1, MAX_OBSERVATION_RECORD_BYTES, MEMORY_FACT_SANITIZER_VERSION_V1,
    MemoryFactSanitizationV1, PrivacyDetectorV1, PrivacySanitizerError, SanitizationActionV1,
    SanitizationFindingV1, SanitizedPayloadVerificationError, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
    redact_lcm_sensitive_payload, sanitize_code_source_bytes, sanitize_memory_fact_payload,
    sanitize_provider_metadata_json, verify_memory_fact_sanitization,
    verify_sanitized_json_payload,
};

fn identity_for(record: &[u8]) -> ClaudeObservationIdentityMaterialV1 {
    identity_for_session(record, "session.privacy-test")
}

fn identity_for_session(record: &[u8], session_id: &str) -> ClaudeObservationIdentityMaterialV1 {
    let source =
        ClaudeSourceIdentityV1::new(SessionId::new(session_id).expect("valid test session ID"))
            .expect("valid Claude source identity");
    let end = u64::try_from(record.len().max(1)).expect("test record length fits in u64");
    ClaudeObservationIdentityMaterialV1::new(
        source,
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).expect("non-zero test generation"),
        ClaudeByteRangeV1::new(0, end).expect("non-empty test byte range"),
    )
    .expect("valid observation identity")
}

fn retention_class() -> RetentionClass {
    RetentionClass::new("retention.privacy-test").expect("valid test retention class")
}

fn sanitize(sanitizer: &ClaudeRecordSanitizerV1, record: &[u8]) -> ClaudeSanitizationOutcomeV1 {
    sanitize_with_identity(sanitizer, record, identity_for(record))
}

fn sanitize_with_identity(
    sanitizer: &ClaudeRecordSanitizerV1,
    record: &[u8],
    identity: ClaudeObservationIdentityMaterialV1,
) -> ClaudeSanitizationOutcomeV1 {
    let parsed = parse_claude_record_v1(record, identity.position())
        .expect("parse bounded sanitizer fixture");
    sanitizer
        .sanitize_parsed(parsed, identity, retention_class())
        .expect("sanitizer should produce an outcome")
}

fn assert_non_durable(
    outcome: &ClaudeSanitizationOutcomeV1,
    disposition: SanitizerDispositionV1,
    detector: PrivacyDetectorV1,
    action: SanitizationActionV1,
) {
    assert!(outcome.durable_observation().is_none());
    assert_eq!(outcome.receipt().disposition(), disposition);
    assert_eq!(outcome.receipt().sensitivity(), SensitivityV1::Sensitive);
    assert!(outcome.receipt().payload().is_none());
    assert_eq!(outcome.findings().len(), 1);
    assert_eq!(outcome.findings()[0].detector(), detector);
    assert_eq!(outcome.findings()[0].action(), action);
}

fn generated_high_entropy_token() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    (0..48)
        .map(|index| char::from(ALPHABET[(index * 17) % ALPHABET.len()]))
        .collect()
}

#[test]
fn code_source_sanitizer_redacts_and_issues_raw_bound_receipts() {
    let first_secret = ["sk", "-test-", "1234567890abcdef"].concat();
    let second_secret = ["sk", "-test-", "abcdef1234567890"].concat();
    assert_eq!(first_secret.len(), second_secret.len());
    let source = |secret: &str| format!("pub const TOKEN: &str = \"{secret}\";\n");

    let first = sanitize_code_source_bytes(
        source(&first_secret).as_bytes(),
        CodeSourceShapeV1::CodeOrProse,
    )
    .expect("sanitize first code source");
    let replay = sanitize_code_source_bytes(
        source(&first_secret).as_bytes(),
        CodeSourceShapeV1::CodeOrProse,
    )
    .expect("replay first code source");
    let second = sanitize_code_source_bytes(
        source(&second_secret).as_bytes(),
        CodeSourceShapeV1::CodeOrProse,
    )
    .expect("sanitize second code source");

    assert!(!String::from_utf8_lossy(first.sanitized_bytes()).contains(&first_secret));
    assert_eq!(first.sanitized_bytes(), second.sanitized_bytes());
    assert_eq!(
        first.receipt().receipt().sanitizer_version().as_str(),
        CODE_SOURCE_SANITIZER_VERSION_V1
    );
    assert!(
        first
            .receipt()
            .receipt()
            .receipt_id()
            .as_str()
            .starts_with("privacy.code-source.v1.")
    );
    assert_eq!(
        first.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    assert_eq!(first.receipt().sensitivity(), SensitivityV1::Secret);
    assert_eq!(
        first.receipt().payload(),
        Some(
            &PayloadReferenceV1::for_payload(&Value::String(
                String::from_utf8(first.sanitized_bytes().to_vec()).expect("sanitized UTF-8"),
            ))
            .expect("sanitized payload reference"),
        )
    );
    assert_eq!(
        first.receipt().receipt().receipt_id(),
        replay.receipt().receipt().receipt_id()
    );
    assert_ne!(
        first.receipt().receipt().receipt_id(),
        second.receipt().receipt().receipt_id(),
        "equal sanitized output from different raw secrets needs distinct scan evidence"
    );
}

#[test]
fn payload_verifier_rejects_stale_receipts_and_exact_content_mismatch() {
    let sanitized =
        sanitize_code_source_bytes(b"let safe = true;\n", CodeSourceShapeV1::CodeOrProse)
            .expect("sanitize source");
    let (bytes, receipt) = sanitized.into_parts();
    let payload = Value::String(String::from_utf8(bytes).expect("sanitized UTF-8"));
    let revision = ComponentVersion::new(CODE_SOURCE_SANITIZER_VERSION_V1).expect("valid revision");

    assert!(verify_sanitized_json_payload(&payload, &receipt, &revision).is_ok());
    assert_eq!(
        verify_sanitized_json_payload(
            &payload,
            &receipt,
            &ComponentVersion::new("privacy.code-source.future").expect("valid revision"),
        ),
        Err(SanitizedPayloadVerificationError::StaleRevision)
    );
    assert_eq!(
        verify_sanitized_json_payload(&json!({"different": true}), &receipt, &revision),
        Err(SanitizedPayloadVerificationError::PayloadMismatch)
    );
}

#[test]
fn memory_fact_verifier_rejects_a_self_authored_receipt() {
    let MemoryFactSanitizationV1::Durable { payload, receipt } =
        sanitize_memory_fact_payload(json!({"content": "safe"})).expect("sanitize fact")
    else {
        panic!("safe fact should be durable");
    };
    let forged = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("memory-fact-receipt.v1.forged").unwrap(),
            ComponentVersion::new(MEMORY_FACT_SANITIZER_VERSION_V1).unwrap(),
        )
        .unwrap(),
        receipt.disposition(),
        receipt.sensitivity(),
        receipt.payload().cloned(),
    )
    .unwrap();

    assert_eq!(
        verify_memory_fact_sanitization(&payload, &forged),
        Err(SanitizedPayloadVerificationError::ReceiptAuthorityMismatch)
    );
    assert!(verify_memory_fact_sanitization(&payload, &receipt).is_ok());
}

#[test]
fn lcm_payload_sanitization_is_idempotent() {
    let raw = "api_key=sk-lcm-canonical-detector-1234567890abcdef";
    let first = super::sanitize_lcm_payload_text(raw).expect("first sanitization");
    let second =
        super::sanitize_lcm_payload_text(first.sanitized_text()).expect("second sanitization");

    assert_eq!(second.sanitized_text(), first.sanitized_text());
}

#[test]
fn lcm_receipt_binding_preserves_findings_from_the_raw_payload() {
    let raw = "api_key=sk-lcm-binding-raw-secret-1234567890abcdef";
    let sanitized = super::sanitize_lcm_payload_text(raw).expect("sanitize raw payload");
    let bound = super::bind_sanitized_lcm_payload_text(raw, sanitized.sanitized_text())
        .expect("bind protected candidate");

    assert_eq!(
        bound.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    assert_eq!(bound.receipt().sensitivity(), SensitivityV1::Secret);
    assert!(
        !bound.findings().is_empty(),
        "receipt binding discarded raw-input findings"
    );
}

#[test]
fn parsed_record_token_preserves_verified_source_evidence() {
    let record = serde_json::to_vec(&json!({
        "type": "assistant",
        "message": {"content": "scope-readable"}
    }))
    .expect("serialize parsed-token fixture");
    let start = 41;
    let range = ClaudeByteRangeV1::new(start, start + record.len() as u64)
        .expect("valid parsed-token range");

    let parsed = parse_claude_record_v1(&record, range).expect("parse bounded Claude record");

    assert_eq!(parsed.encoded_len(), record.len());
    assert_eq!(*parsed.source_range(), range);
    assert_eq!(parsed.value()["message"]["content"], "scope-readable");
}

#[test]
fn generic_parser_preserves_native_ordering_domain() {
    let record = br#"{"type":"message"}"#;
    let range = ClaudeByteRangeV1::new(7, 7 + record.len() as u64).unwrap();

    let parsed =
        parse_observation_record_v1(record, range, ObservationOrderingDomainV1::SqliteRowId)
            .expect("parse provider record");

    assert_eq!(parsed.source_range(), &range);
    assert_eq!(
        parsed.ordering_domain(),
        ObservationOrderingDomainV1::SqliteRowId
    );
}

#[test]
fn parsed_record_rejects_mismatched_range_and_canonical_oversize() {
    let record = br#"{"type":"assistant"}"#;
    let mismatched =
        ClaudeByteRangeV1::new(0, record.len() as u64 + 1).expect("non-empty mismatched range");
    assert_eq!(
        parse_claude_record_v1(record, mismatched).err(),
        Some(ClaudeRecordParseErrorV1::RangeLengthMismatch)
    );

    let oversized = vec![b' '; MAX_OBSERVATION_RECORD_BYTES + 1];
    let oversized_range =
        ClaudeByteRangeV1::new(0, oversized.len() as u64).expect("non-empty oversized range");
    assert_eq!(
        parse_claude_record_v1(&oversized, oversized_range).err(),
        Some(ClaudeRecordParseErrorV1::TooLarge)
    );
}

#[test]
fn sanitize_parsed_consumes_token_without_reparsing_raw_bytes() {
    let mut record = serde_json::to_vec(&json!({"message": "ordinary parsed fixture"}))
        .expect("serialize parsed sanitizer fixture");
    let identity = identity_for(&record);
    let parsed =
        parse_claude_record_v1(&record, identity.position()).expect("parse sanitizer fixture once");
    record.fill(b'!');

    let outcome = ClaudeRecordSanitizerV1::claude_v1()
        .expect("valid Claude V1 sanitizer")
        .sanitize_parsed(parsed, identity, retention_class())
        .expect("sanitize parser-issued token");

    assert_eq!(
        outcome
            .durable_observation()
            .expect("parsed record remains durable")
            .payload()["message"],
        "ordinary parsed fixture"
    );
}

#[test]
fn sanitize_parsed_rejects_identity_range_mismatch() {
    let record = serde_json::to_vec(&json!({"message": "ordinary range fixture"}))
        .expect("serialize range fixture");
    let shifted_range =
        ClaudeByteRangeV1::new(1, record.len() as u64 + 1).expect("valid shifted range");
    let parsed = parse_claude_record_v1(&record, shifted_range).expect("parse shifted fixture");

    let error = ClaudeRecordSanitizerV1::claude_v1()
        .expect("valid Claude V1 sanitizer")
        .sanitize_parsed(parsed, identity_for(&record), retention_class())
        .expect_err("mismatched identity range must fail");

    assert!(matches!(error, PrivacySanitizerError::SourceRangeMismatch));
}

#[test]
fn sanitize_parsed_rejects_ordering_domain_mismatch() {
    let record = serde_json::to_vec(&json!({"message": "ordinary ordering fixture"}))
        .expect("serialize ordering fixture");
    let identity = identity_for(&record);
    let parsed = parse_observation_record_v1(
        &record,
        identity.position(),
        ObservationOrderingDomainV1::SqliteRowId,
    )
    .expect("parse row-ordered fixture");

    let error = ClaudeRecordSanitizerV1::claude_v1()
        .expect("valid Claude V1 sanitizer")
        .sanitize_parsed(parsed, identity, retention_class())
        .expect_err("mismatched ordering domain must fail");

    assert!(matches!(
        error,
        PrivacySanitizerError::OrderingDomainMismatch
    ));
}

#[test]
fn provider_sanitizer_uses_provider_neutral_policy_and_receipt_domain() {
    let record = serde_json::to_vec(&json!({"message": "ordinary provider fixture"}))
        .expect("serialize provider fixture");
    let range = ClaudeByteRangeV1::new(10, 11).expect("valid row range");
    let parsed = parse_normalized_observation_record_v1(
        &record,
        range,
        ObservationOrderingDomainV1::SqliteRowId,
        |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("hermes").unwrap(),
                "message",
                ObservationId::new("message.provider-fixture").unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new("session.provider-fixture").unwrap(),
                )
                .with_message_id(ObservationId::new("message.provider-fixture").unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::SqliteRowId,
                    range,
                ),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        },
    )
    .expect("parse provider fixture");
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("hermes").unwrap(),
        SessionId::new("session.provider-fixture").unwrap(),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(3).unwrap(),
        range,
        ObservationOrderingDomainV1::SqliteRowId,
        ObservationId::new("message.provider-fixture").unwrap(),
    )
    .unwrap();

    let outcome = ClaudeRecordSanitizerV1::observation_v1()
        .expect("valid provider sanitizer")
        .sanitize_parsed(parsed, identity, retention_class())
        .expect("sanitize provider fixture");

    assert_eq!(
        outcome.receipt().receipt().sanitizer_version().as_str(),
        OBSERVATION_SANITIZER_VERSION_V1
    );
    assert!(
        outcome
            .receipt()
            .receipt()
            .receipt_id()
            .as_str()
            .starts_with("privacy.observation.v1.")
    );
    assert!(outcome.durable_observation().is_some());
}

#[test]
fn provider_sanitizer_allows_only_legacy_claude_to_omit_native_record_identity() {
    let record = serde_json::to_vec(&json!({"message": "legacy identity fixture"})).unwrap();
    let range = ClaudeByteRangeV1::new(0, u64::try_from(record.len()).unwrap()).unwrap();
    let fixture = |provider: &str| {
        let provider_id = ProviderId::new(provider).unwrap();
        let session_id = SessionId::new("session.legacy-identity").unwrap();
        let stable_record_id = ObservationId::new(format!("message.{provider}.legacy")).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &record,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |_| {
                CanonicalObservationEnvelopeV1::new(
                    provider_id.clone(),
                    "message",
                    stable_record_id.clone(),
                    CanonicalObservationRelationsV1::new(session_id.clone())
                        .with_message_id(stable_record_id.clone()),
                    vec![CanonicalObservationFactV1::Message {
                        role: CanonicalMessageRoleV1::Assistant,
                        content: json!({"text": "legacy identity fixture"}),
                        model: None,
                        timestamp: None,
                    }],
                    CanonicalObservationEvidenceV1::new(
                        ObservationOrderingDomainV1::FileBytes,
                        range,
                    ),
                )
                .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
            },
        )
        .unwrap();
        let source = ObservationSourceIdentityV1::for_provider(provider_id, session_id).unwrap();
        let identity = ClaudeObservationIdentityMaterialV1::new(
            source,
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(1).unwrap(),
            range,
        )
        .unwrap();
        (parsed, identity)
    };

    let (parsed, identity) = fixture("claude");
    let outcome = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .expect("legacy Claude range identity remains compatible");
    assert!(
        outcome
            .durable_observation()
            .unwrap()
            .identity()
            .native_record_id()
            .is_none()
    );

    let (parsed, identity) = fixture("hermes");
    let error = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .expect_err("non-Claude canonical observations require native record identity");
    assert!(matches!(
        error,
        PrivacySanitizerError::DomainContract(ObservationContractError::InvalidCanonicalPayload)
    ));
}

#[test]
fn provider_sanitizer_preserves_stable_public_structural_ids() {
    let record = serde_json::to_vec(&json!({"message": "public identity fixture"})).unwrap();
    let range = ClaudeByteRangeV1::new(40, 41).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &record,
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        |_| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("provider-neutral-fixture").unwrap(),
                "message",
                ObservationId::new("message.public-123").unwrap(),
                CanonicalObservationRelationsV1::new(SessionId::new("session.public-123").unwrap())
                    .with_message_id(ObservationId::new("message.public-123").unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: Value::String("safe".to_owned()),
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::DaemonSequence,
                    range,
                ),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        },
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_provider(
            ProviderId::new("provider-neutral-fixture").unwrap(),
            SessionId::new("session.public-123").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        ObservationId::new("message.public-123").unwrap(),
    )
    .unwrap();

    let outcome = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .unwrap();
    let durable = outcome.durable_observation().expect("durable observation");
    assert_eq!(durable.source().session_id().as_str(), "session.public-123");
    assert_eq!(
        durable
            .identity()
            .native_record_id()
            .map(ObservationId::as_str),
        Some("message.public-123")
    );
    assert_eq!(
        durable.payload()["relations"]["session_id"].as_str(),
        Some(durable.source().session_id().as_str())
    );
    assert_eq!(
        durable.payload()["relations"]["message_id"].as_str(),
        Some("message.public-123")
    );
}

#[test]
fn provider_sanitizer_protects_credential_shaped_structural_ids_consistently() {
    let raw = ["AKIA", "STRUCTURAL", "234567"].concat();
    let record = serde_json::to_vec(&json!({"message": "protected identity fixture"})).unwrap();
    let range = ClaudeByteRangeV1::new(50, 51).unwrap();
    let provider = ProviderId::new("provider-neutral-fixture").unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &record,
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        |_| {
            CanonicalObservationEnvelopeV1::new(
                provider.clone(),
                "message",
                ObservationId::new(raw.clone()).unwrap(),
                CanonicalObservationRelationsV1::new(SessionId::new(raw.clone()).unwrap())
                    .with_message_id(ObservationId::new(raw.clone()).unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: Value::String("safe".to_owned()),
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::DaemonSequence,
                    range,
                ),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        },
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_provider(provider, SessionId::new(raw.clone()).unwrap())
            .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        ObservationId::new(raw.clone()).unwrap(),
    )
    .unwrap();

    let outcome = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .unwrap();
    let durable = outcome.durable_observation().unwrap();
    let protected = durable.source().session_id().as_str();
    assert!(protected.starts_with("privacy.structural-id.v1."));
    assert_eq!(
        durable
            .identity()
            .native_record_id()
            .map(ObservationId::as_str),
        Some(protected)
    );
    assert_eq!(
        durable.payload()["stable_record_id"].as_str(),
        Some(protected)
    );
    assert_eq!(
        durable.payload()["relations"]["session_id"].as_str(),
        Some(protected)
    );
    assert_eq!(
        durable.payload()["relations"]["message_id"].as_str(),
        Some(protected)
    );
    assert!(!serde_json::to_string(durable).unwrap().contains(&raw));
    assert!(
        outcome
            .findings()
            .iter()
            .all(|finding| !finding.location().contains(&raw))
    );
}

#[test]
fn provider_neutral_workflow_fact_redaction_leaks_no_raw_secret() {
    const SECRET: &str = "workflow-secret-canary-must-not-persist";
    let record = serde_json::to_vec(&json!({
        "text": "publish release",
        "api_key": SECRET
    }))
    .unwrap();
    let range = ClaudeByteRangeV1::new(20, 21).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &record,
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("provider-neutral-fixture").unwrap(),
                "workflow_fixture",
                ObservationId::new("workflow.privacy-fixture").unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new("session.workflow-privacy-fixture").unwrap(),
                ),
                vec![CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                    provider_reference: Some("task.native.privacy-fixture".to_owned()),
                    item_id: Some("task.stable.privacy-fixture".to_owned()),
                    parent_reference: None,
                    list_reference: None,
                    state: None,
                    status: Some("pending".to_owned()),
                    item_order: None,
                    revision: None,
                    event_sequence: Some(1),
                    content: Some(native),
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::DaemonSequence,
                    range,
                ),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        },
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_provider(
            ProviderId::new("provider-neutral-fixture").unwrap(),
            SessionId::new("session.workflow-privacy-fixture").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        ObservationId::new("workflow.privacy-fixture").unwrap(),
    )
    .unwrap();
    let outcome = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .unwrap();
    let durable = outcome
        .durable_observation()
        .expect("redacted workflow fact remains durable");
    let durable_json = serde_json::to_string(durable).unwrap();
    let receipt_json = serde_json::to_string(outcome.receipt()).unwrap();

    assert!(!durable_json.contains(SECRET));
    assert!(!receipt_json.contains(SECRET));
    assert!(durable_json.contains("[TraceDecay redacted:"));
    assert!(outcome.findings().iter().all(|finding| {
        !format!("{finding:?}").contains(SECRET)
            && finding.action() == SanitizationActionV1::Redacted
    }));
}

#[test]
fn provider_sanitizer_rejects_raw_provider_json_without_normalization() {
    let record = br#"{"message":"must normalize"}"#;
    let range = ClaudeByteRangeV1::new(1, 2).unwrap();
    let parsed =
        parse_observation_record_v1(record, range, ObservationOrderingDomainV1::SqliteRowId)
            .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_provider(
            ProviderId::new("hermes").unwrap(),
            SessionId::new("session.raw-provider").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SqliteRowId,
        ObservationId::new("message.raw-provider").unwrap(),
    )
    .unwrap();

    let error = ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(parsed, identity, retention_class())
        .unwrap_err();

    assert!(matches!(
        error,
        PrivacySanitizerError::CanonicalEnvelopeRequired
    ));
}

#[test]
fn clean_record_is_accepted_and_receipt_binds_the_payload() {
    let expected_payload = json!({
        "type": "assistant",
        "message": { "content": "ordinary fixture text" },
        "future_provider_metadata": { "attempt": 3, "enabled": true }
    });
    let record = serde_json::to_vec(&expected_payload).expect("serialize clean fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let observation = outcome
        .durable_observation()
        .expect("clean record should cross the durable boundary");

    assert!(outcome.findings().is_empty());
    assert_eq!(
        outcome.receipt().disposition(),
        SanitizerDispositionV1::Accepted
    );
    assert_eq!(outcome.receipt().sensitivity(), SensitivityV1::NonSensitive);
    assert_eq!(observation.payload(), &expected_payload);
    let sanitized_record = outcome
        .sanitized_record()
        .expect("durable outcome issues an opaque sanitized record");
    assert_eq!(sanitized_record.payload(), &expected_payload);
    assert_eq!(sanitized_record.receipt(), outcome.receipt());
    let expected_reference =
        PayloadReferenceV1::for_payload(&expected_payload).expect("reference clean payload");
    assert_eq!(observation.payload_reference(), &expected_reference);
    assert_eq!(outcome.receipt().payload(), Some(&expected_reference));
}

#[test]
fn json_is_parsed_before_unknown_fields_are_scanned() {
    let bearer_keyword = ["Bear", "er"].concat();
    let token = "Aa0".repeat(5);
    let payload = json!({
        "type": "future_record_kind",
        "future_provider_field": format!("{bearer_keyword} {token}"),
        "another_unknown_field": { "kept": "ordinary" }
    });
    let valid_record = serde_json::to_vec(&payload).expect("serialize unknown-field fixture");

    let sanitizer = ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer");
    let valid_outcome = sanitize(&sanitizer, &valid_record);
    let valid_observation = valid_outcome
        .durable_observation()
        .expect("valid objects with unknown fields remain durable");
    assert_eq!(
        valid_outcome.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    assert!(
        valid_outcome
            .findings()
            .iter()
            .any(|finding| finding.detector() == PrivacyDetectorV1::BearerToken)
    );
    assert_eq!(
        valid_observation.payload()["another_unknown_field"]["kept"],
        json!("ordinary")
    );
    assert!(
        !valid_observation.payload()["future_provider_field"]
            .as_str()
            .expect("redacted field remains text")
            .contains(&token)
    );

    let mut malformed_record = valid_record;
    assert_eq!(malformed_record.pop(), Some(b'}'));
    let malformed_range = identity_for(&malformed_record).position();
    assert_eq!(
        parse_claude_record_v1(&malformed_record, malformed_range).err(),
        Some(ClaudeRecordParseErrorV1::Malformed)
    );
}

#[test]
fn default_exact_formats_are_detected_and_redacted() {
    let bearer_keyword = ["Bear", "er"].concat();
    let bearer_token = "Aa0".repeat(5);
    let bearer_value = format!("{bearer_keyword} {bearer_token}");

    let private_key_label = ["PRIVATE", "KEY"].join(" ");
    let private_key = format!(
        "-----BEGIN {private_key_label}-----\nfixture-body\n-----END {private_key_label}-----"
    );

    let credential_prefix = ["s", "k", "-"].concat();
    let prefixed_credential = format!("{credential_prefix}{}", "A0".repeat(10));
    let assignment = format!("{} = {}", ["creden", "tial"].concat(), "Abc123".repeat(2));

    let payload = json!({
        "events": [bearer_value, private_key, prefixed_credential, assignment]
    });
    let record = serde_json::to_vec(&payload).expect("serialize exact-format fixture");
    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let observation = outcome
        .durable_observation()
        .expect("redacted record should remain durable");

    assert_eq!(
        outcome.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    for detector in [
        PrivacyDetectorV1::BearerToken,
        PrivacyDetectorV1::PrivateKey,
        PrivacyDetectorV1::ExactCredential,
        PrivacyDetectorV1::CredentialAssignment,
    ] {
        assert!(
            outcome
                .findings()
                .iter()
                .any(|finding| finding.detector() == detector),
            "missing finding for {detector:?}"
        );
    }

    let sanitized = observation.payload().to_string();
    for detected_text in [
        bearer_token,
        private_key_label,
        prefixed_credential,
        assignment,
    ] {
        assert!(!sanitized.contains(&detected_text));
    }
}

#[test]
fn configured_sensitive_keys_redact_nested_values() {
    let policy = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid Claude V1 policy")
        .with_sensitive_keys(["custom credential"]);
    let sanitizer = ClaudeRecordSanitizerV1::new(policy);
    let payload = json!({
        "outer": {
            "custom_credential": {
                "future": "ordinary nested value",
                "items": [1, 2, 3]
            }
        },
        "items": [
            { "custom-credential": ["nested", "array", "value"] }
        ]
    });
    let record = serde_json::to_vec(&payload).expect("serialize sensitive-key fixture");

    let outcome = sanitize(&sanitizer, &record);
    let observation = outcome
        .durable_observation()
        .expect("redacted sensitive fields should remain durable");

    let findings: Vec<_> = outcome
        .findings()
        .iter()
        .filter(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField)
        .collect();
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|finding| {
        finding.confidence() == DetectionConfidenceV1::Contextual
            && finding.action() == SanitizationActionV1::Redacted
    }));
    for value in [
        &observation.payload()["outer"]["custom_credential"],
        &observation.payload()["items"][0]["custom-credential"],
    ] {
        assert!(
            value
                .as_str()
                .is_some_and(|text| text.starts_with("[TraceDecay redacted:"))
        );
    }
}

#[test]
fn normalized_secret_key_variants_and_semantic_suffixes_are_redacted() {
    let payload = json!({
        "refreshToken": "refresh-value",
        "session-token": "session-value",
        "id_token": "identity-value",
        "x-api-key": "api-value",
        "databasePassword": "password-value",
        "service_secret": "secret-value",
        "githubCredential": "credential-value",
        "oauthToken": "token-value",
        "vendorApiKey": "vendor-value",
        "vendorAPIKey": "vendor-acronym-value",
        "JWTToken": "jwt-value",
        "kmsPrivateKey": "private-key-value",
        "serviceSecretKey": "secret-key-value",
        "cloudAccessKey": "access-key-value",
        "dbPassphrase": "passphrase-value",
        "token_count": 42,
        "password_policy": "ordinary policy metadata",
        "credential_type": "ordinary type metadata",
        "api_key_hint": "ordinary hint metadata"
    });
    let record = serde_json::to_vec(&payload).expect("serialize semantic-key fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let observation = outcome
        .durable_observation()
        .expect("semantic sensitive fields remain durable after redaction");

    for key in [
        "refreshToken",
        "session-token",
        "id_token",
        "x-api-key",
        "databasePassword",
        "service_secret",
        "githubCredential",
        "oauthToken",
        "vendorApiKey",
        "vendorAPIKey",
        "JWTToken",
        "kmsPrivateKey",
        "serviceSecretKey",
        "cloudAccessKey",
        "dbPassphrase",
    ] {
        assert!(
            observation.payload()[key]
                .as_str()
                .is_some_and(|value| value.starts_with("[TraceDecay redacted:")),
            "expected {key} to be redacted"
        );
    }
    for key in [
        "token_count",
        "password_policy",
        "credential_type",
        "api_key_hint",
    ] {
        assert_eq!(observation.payload()[key], payload[key]);
    }
}

#[test]
fn quoted_assignments_with_punctuation_are_redacted() {
    let secrets = [
        r#"password = "p@ssw0rd!""#,
        "password = p@ssw0rd!",
        "password = \"truncated!",
        r#"password = "abcdef\"tailsecret""#,
    ];
    let payload = json!({"messages": secrets});
    let record = serde_json::to_vec(&payload).expect("serialize quoted-assignment fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let observation = outcome
        .durable_observation()
        .expect("redacted quoted assignment remains durable");

    assert!(outcome.findings().iter().any(|finding| {
        finding.detector() == PrivacyDetectorV1::CredentialAssignment
            && finding.action() == SanitizationActionV1::Redacted
    }));
    let sanitized = observation.payload().to_string();
    assert!(secrets.iter().all(|secret| !sanitized.contains(secret)));
    assert!(!sanitized.contains("p@ssw0rd!"));
    assert!(!sanitized.contains("truncated!"));
    assert!(!sanitized.contains("tailsecret"));
}

#[test]
fn credential_bearing_object_keys_quarantine_without_key_collisions() {
    let prefix = ["s", "k", "-test-"].concat();
    let first_key = format!("{prefix}123456");
    let second_key = format!("{prefix}654321");
    let payload = Value::Object(
        [
            (first_key, json!("first value")),
            (second_key, json!("second value")),
            ("ordinary".to_string(), json!("kept")),
        ]
        .into_iter()
        .collect(),
    );
    let record = serde_json::to_vec(&payload).expect("serialize sensitive-key-name fixture");
    let sanitizer = ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer");
    let identity = identity_for(&record);

    let first = sanitize_with_identity(&sanitizer, &record, identity.clone());
    let second = sanitize_with_identity(&sanitizer, &record, identity);

    assert_eq!(
        first.receipt().disposition(),
        SanitizerDispositionV1::Quarantined
    );
    assert!(first.durable_observation().is_none());
    assert!(first.receipt().payload().is_none());
    assert_eq!(first.findings().len(), 2);
    assert!(first.findings().iter().all(|finding| {
        finding.detector() == PrivacyDetectorV1::ExactCredential
            && finding.action() == SanitizationActionV1::Quarantined
            && !finding.location().contains(&prefix)
    }));
    assert_eq!(
        first.receipt().receipt().receipt_id(),
        second.receipt().receipt().receipt_id()
    );
    assert_eq!(first.findings(), second.findings());
}

#[test]
fn wholesale_sensitive_value_redaction_skips_nested_object_keys() {
    let prefix = ["s", "k", "-test-"].concat();
    let nested_key = format!("{prefix}123456");
    let nested = Value::Object([(nested_key, json!("nested value"))].into_iter().collect());
    let payload = json!({"password": nested});
    let record = serde_json::to_vec(&payload).expect("serialize wholesale-redaction fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );

    assert_eq!(
        outcome.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
    assert!(outcome.durable_observation().is_some());
    assert_eq!(outcome.findings().len(), 1);
    assert_eq!(
        outcome.findings()[0].detector(),
        PrivacyDetectorV1::SensitiveField
    );
}

#[test]
fn contextual_high_entropy_token_is_detected() {
    let token = generated_high_entropy_token();
    let payload = json!({
        "message": format!("opaque session value follows {token}")
    });
    let record = serde_json::to_vec(&payload).expect("serialize entropy fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let observation = outcome
        .durable_observation()
        .expect("redacted entropy record should remain durable");
    let finding = outcome
        .findings()
        .iter()
        .find(|finding| finding.detector() == PrivacyDetectorV1::HighEntropyToken)
        .expect("high-entropy detector should report a finding");

    assert_eq!(finding.confidence(), DetectionConfidenceV1::Heuristic);
    assert_eq!(finding.action(), SanitizationActionV1::Redacted);
    assert!(
        !observation.payload()["message"]
            .as_str()
            .expect("message remains text")
            .contains(&token)
    );
}

#[test]
fn finding_contract_serializes_complete_safe_detector_evidence() {
    let credential_prefix = ["s", "k", "-"].concat();
    let secret = format!("{credential_prefix}{}", "Z9".repeat(10));
    let record = serde_json::to_vec(&json!({ "payload": secret })).unwrap();
    let outcome = sanitize(&ClaudeRecordSanitizerV1::claude_v1().unwrap(), &record);
    let finding = outcome.findings().first().expect("credential finding");

    assert_eq!(
        finding.detector_origin(),
        SanitizationDetectorOriginV1::BuiltInDetectorKernel
    );
    assert_eq!(
        finding.detector_revision(),
        SanitizationDetectorRevisionV1::V1
    );
    assert_eq!(
        finding.remediation_class(),
        SanitizationRemediationClassV1::RotateOrRevokeCredential
    );
    assert_eq!(
        finding.scanned_coverage(),
        SanitizationScannedCoverageV1::Complete
    );
    assert_eq!(finding.evidence_anchors().len(), 1);
    assert_eq!(
        finding.evidence_anchors()[0].structural_location(),
        finding.location()
    );

    let serialized = serde_json::to_value(finding).unwrap();
    assert_eq!(serialized["detector_origin"], "built_in_detector_kernel");
    assert_eq!(serialized["detector_revision"], "v1");
    assert_eq!(
        serialized["remediation_class"],
        "rotate_or_revoke_credential"
    );
    assert_eq!(serialized["scanned_coverage"]["status"], "complete");
    assert!(!serialized.to_string().contains(&secret));
    let decoded: SanitizationFindingV1 = serde_json::from_value(serialized).unwrap();
    assert_eq!(&decoded, finding);
}

#[test]
fn finding_contract_rejects_missing_or_unsafe_evidence_metadata() {
    let record = serde_json::to_vec(&json!({
        "password": "finding-contract-secret"
    }))
    .unwrap();
    let outcome = sanitize(&ClaudeRecordSanitizerV1::claude_v1().unwrap(), &record);
    let finding = outcome.findings().first().unwrap();
    let serialized = serde_json::to_value(finding).unwrap();

    for required in [
        "detector_origin",
        "detector_revision",
        "remediation_class",
        "evidence_anchors",
        "scanned_coverage",
    ] {
        let mut incomplete = serialized.clone();
        incomplete.as_object_mut().unwrap().remove(required);
        assert!(
            serde_json::from_value::<SanitizationFindingV1>(incomplete).is_err(),
            "missing {required} must fail closed"
        );
    }

    let mut unsafe_anchor = serialized;
    unsafe_anchor["evidence_anchors"][0]["structural_location"] =
        Value::String("finding-contract-secret".to_owned());
    assert!(serde_json::from_value::<SanitizationFindingV1>(unsafe_anchor).is_err());

    let mut oversized_location = serde_json::to_value(finding).unwrap();
    let location = format!("$/{}", "1/".repeat(128));
    oversized_location["location"] = Value::String(location.clone());
    oversized_location["evidence_anchors"][0]["structural_location"] = Value::String(location);
    assert!(serde_json::from_value::<SanitizationFindingV1>(oversized_location).is_err());

    let mut duplicate_anchor = serde_json::to_value(finding).unwrap();
    let anchor = duplicate_anchor["evidence_anchors"][0].clone();
    duplicate_anchor["evidence_anchors"]
        .as_array_mut()
        .unwrap()
        .push(anchor);
    assert!(serde_json::from_value::<SanitizationFindingV1>(duplicate_anchor).is_err());
}

#[test]
fn policy_limit_findings_report_incomplete_scanned_coverage() {
    let policy = ClaudeSanitizerPolicyV1::claude_v1()
        .unwrap()
        .with_limits(32, 16, 100)
        .unwrap();
    let sanitizer = ClaudeRecordSanitizerV1::new(policy);
    let record = serde_json::to_vec(&json!({ "message": "x".repeat(64) })).unwrap();
    let outcome = sanitize(&sanitizer, &record);
    let finding = outcome.findings().first().unwrap();

    assert_eq!(
        finding.detector_origin(),
        SanitizationDetectorOriginV1::SanitizerPolicy
    );
    assert_eq!(
        finding.remediation_class(),
        SanitizationRemediationClassV1::ReduceInputAndRetry
    );
    assert_eq!(
        finding.scanned_coverage(),
        SanitizationScannedCoverageV1::Incomplete {
            boundary: SanitizationScanBoundaryV1::RecordBytes,
        }
    );
}

#[test]
fn findings_never_contain_detected_secret_text() {
    let credential_prefix = ["s", "k", "-"].concat();
    let detected_text = format!("{credential_prefix}{}", "Z9".repeat(10));
    let record = serde_json::to_vec(&json!({ "payload": detected_text }))
        .expect("serialize finding-safety fixture");

    let outcome = sanitize(
        &ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer"),
        &record,
    );
    let diagnostic = format!("{:?}", outcome.findings());
    let original: Value = serde_json::from_slice(&record).expect("parse fixture for assertion");
    let secret = original["payload"]
        .as_str()
        .expect("fixture payload is text");

    assert!(!diagnostic.contains(secret));
    assert!(
        outcome
            .findings()
            .iter()
            .all(|finding| !finding.location().contains(secret))
    );
}

#[test]
fn invalid_records_stop_at_the_parser_and_policy_limited_records_have_no_payload() {
    let malformed = br#"{"message":"#.to_vec();
    let scalar = serde_json::to_vec(&json!("ordinary scalar")).expect("serialize scalar fixture");

    for (record, expected) in [
        (Vec::new(), ClaudeRecordParseErrorV1::Empty),
        (malformed, ClaudeRecordParseErrorV1::Malformed),
        (scalar, ClaudeRecordParseErrorV1::NonObject),
    ] {
        let end = u64::try_from(record.len().max(1)).expect("test record length fits");
        let range = ClaudeByteRangeV1::new(0, end).expect("non-empty parser range");
        assert_eq!(parse_claude_record_v1(&record, range).err(), Some(expected));
    }

    let limited_policy = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid Claude V1 policy")
        .with_limits(32, 16, 100)
        .expect("valid small test limits");
    let limited_sanitizer = ClaudeRecordSanitizerV1::new(limited_policy);
    let oversized = serde_json::to_vec(&json!({ "message": "x".repeat(64) }))
        .expect("serialize oversized fixture");
    let identity = identity_for(&oversized);
    let parsed = parse_claude_record_v1(&oversized, identity.position())
        .expect("canonical parser accepts policy-limited fixture");
    let outcome = limited_sanitizer
        .sanitize_parsed(parsed, identity, retention_class())
        .expect("limited sanitizer returns a typed outcome");
    assert_non_durable(
        &outcome,
        SanitizerDispositionV1::Rejected,
        PrivacyDetectorV1::RecordSizeLimit,
        SanitizationActionV1::Rejected,
    );
}

#[test]
fn structure_bound_failures_are_quarantined_without_payloads() {
    let depth_policy = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid Claude V1 policy")
        .with_limits(1_024, 3, 100)
        .expect("valid depth test limits");
    let depth_record = serde_json::to_vec(&json!({ "a": { "b": { "c": "value" } } }))
        .expect("serialize depth fixture");
    let depth_identity = identity_for(&depth_record);
    let depth_parsed = parse_claude_record_v1(&depth_record, depth_identity.position())
        .expect("canonical parser accepts policy-limited depth fixture");
    let depth_outcome = ClaudeRecordSanitizerV1::new(depth_policy)
        .sanitize_parsed(depth_parsed, depth_identity, retention_class())
        .expect("limited sanitizer returns a typed outcome");
    assert_non_durable(
        &depth_outcome,
        SanitizerDispositionV1::Quarantined,
        PrivacyDetectorV1::StructureLimit,
        SanitizationActionV1::Quarantined,
    );

    let value_policy = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid Claude V1 policy")
        .with_limits(1_024, 16, 4)
        .expect("valid value-count test limits");
    let value_record =
        serde_json::to_vec(&json!({ "values": [1, 2, 3] })).expect("serialize value-count fixture");
    let value_identity = identity_for(&value_record);
    let value_parsed = parse_claude_record_v1(&value_record, value_identity.position())
        .expect("canonical parser accepts policy-limited value fixture");
    let value_outcome = ClaudeRecordSanitizerV1::new(value_policy)
        .sanitize_parsed(value_parsed, value_identity, retention_class())
        .expect("limited sanitizer returns a typed outcome");
    assert_non_durable(
        &value_outcome,
        SanitizerDispositionV1::Quarantined,
        PrivacyDetectorV1::StructureLimit,
        SanitizationActionV1::Quarantined,
    );
}

#[test]
fn receipt_ids_are_deterministic_and_use_the_fixed_sanitizer_version() {
    assert_eq!(CLAUDE_SANITIZER_VERSION_V1, "privacy.claude-record.v1");
    assert_eq!(MAX_OBSERVATION_RECORD_BYTES, 1_048_576);
    let sanitizer = ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer");
    assert_eq!(
        sanitizer.policy().version().as_str(),
        CLAUDE_SANITIZER_VERSION_V1
    );

    let record = serde_json::to_vec(&json!({ "message": "ordinary deterministic fixture" }))
        .expect("serialize deterministic fixture");
    let identity = identity_for(&record);
    let first = sanitize_with_identity(&sanitizer, &record, identity.clone());
    let second = sanitize_with_identity(&sanitizer, &record, identity.clone());

    assert_eq!(
        first.receipt().receipt().receipt_id(),
        second.receipt().receipt().receipt_id()
    );
    assert_eq!(
        first.receipt().receipt().sanitizer_version().as_str(),
        CLAUDE_SANITIZER_VERSION_V1
    );

    let changed_record =
        serde_json::to_vec(&json!({ "message": "altered! deterministic fixture" }))
            .expect("serialize changed fixture");
    let changed = sanitize_with_identity(&sanitizer, &changed_record, identity);
    assert_ne!(
        first.receipt().receipt().receipt_id(),
        changed.receipt().receipt().receipt_id()
    );

    let changed_identity = sanitize_with_identity(
        &sanitizer,
        &record,
        identity_for_session(&record, "session.privacy-test.changed"),
    );
    assert_ne!(
        first.receipt().receipt().receipt_id(),
        changed_identity.receipt().receipt().receipt_id()
    );
}

#[test]
fn equal_length_distinct_secrets_produce_distinct_raw_bound_receipts() {
    let first_record = serde_json::to_vec(&json!({"password": "alpha123!"})).unwrap();
    let second_record = serde_json::to_vec(&json!({"password": "bravo456?"})).unwrap();
    assert_eq!(first_record.len(), second_record.len());
    let identity = identity_for(&first_record);
    let sanitizer = ClaudeRecordSanitizerV1::claude_v1().expect("valid Claude V1 sanitizer");

    let first = sanitize_with_identity(&sanitizer, &first_record, identity.clone());
    let second = sanitize_with_identity(&sanitizer, &second_record, identity);
    let first_observation = first.durable_observation().expect("first redacted payload");
    let second_observation = second
        .durable_observation()
        .expect("second redacted payload");

    assert_eq!(
        first_observation.observation_id(),
        second_observation.observation_id()
    );
    assert_eq!(
        first_observation.payload_reference(),
        second_observation.payload_reference()
    );
    assert_ne!(
        first.receipt().receipt().receipt_id(),
        second.receipt().receipt().receipt_id()
    );
}

#[test]
fn custom_policy_behavior_has_a_deterministic_version_fingerprint() {
    let default = ClaudeSanitizerPolicyV1::claude_v1().expect("valid default policy");
    let first = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid default policy")
        .with_sensitive_keys(["custom_one", "custom_two"]);
    let reordered = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid default policy")
        .with_sensitive_keys(["custom_two", "custom_one"]);
    let limited = ClaudeSanitizerPolicyV1::claude_v1()
        .expect("valid default policy")
        .with_limits(1_024, 16, 100)
        .expect("fingerprint custom limits");

    assert_ne!(default.version(), first.version());
    assert_eq!(first.version(), reordered.version());
    assert_ne!(first.version(), limited.version());

    let record = serde_json::to_vec(&json!({"custom_one": "secret-value"})).unwrap();
    let outcome = sanitize(&ClaudeRecordSanitizerV1::new(first.clone()), &record);
    assert_eq!(
        outcome.receipt().receipt().sanitizer_version(),
        first.version()
    );
}

#[test]
fn lcm_sensitive_redaction_parses_structured_payload_before_scanning_values() {
    let policy = LcmSensitiveRedactionPolicyV1::enabled(["api_key"]);
    let raw = r#"{"nested":{"api_key":"short","password":"owner-kept-value"},"safe":"keep"}"#;

    let outcome =
        redact_lcm_sensitive_payload(raw, &policy).expect("structured LCM privacy scan succeeds");
    let sanitized: Value =
        serde_json::from_str(outcome.text()).expect("structured output remains JSON");

    assert_eq!(
        sanitized["nested"]["api_key"],
        "[TraceDecay redacted: sensitive field]"
    );
    assert_eq!(sanitized["nested"]["password"], "owner-kept-value");
    assert_eq!(sanitized["safe"], "keep");
    assert_eq!(outcome.patterns(), &["api_key".to_string()]);
}

#[test]
fn lcm_sensitive_redaction_fails_closed_for_unknown_configured_patterns() {
    let policy = LcmSensitiveRedactionPolicyV1::enabled(["api_keey"]);
    let secret = "sk-unknown-policy-1234567890";

    let outcome = redact_lcm_sensitive_payload(&format!("api_key={secret}"), &policy)
        .expect("fail-closed LCM privacy scan succeeds");

    assert!(!outcome.text().contains(secret));
    assert_eq!(outcome.patterns(), &["api_key".to_string()]);
}

#[test]
fn lcm_sensitive_redaction_rejects_duplicate_json_keys_before_materialization() {
    let policy = LcmSensitiveRedactionPolicyV1::enabled(["api_key"]);
    let raw = r#"{"api_key":"unit-test-placeholder-value","api_key":"safe-replacement"}"#;

    assert!(matches!(
        redact_lcm_sensitive_payload(raw, &policy),
        Err(DetectionError::StructuredQuarantine)
    ));
}

#[test]
fn lcm_sensitive_redaction_rejects_payloads_above_the_canonical_raw_byte_limit() {
    let policy = LcmSensitiveRedactionPolicyV1::enabled(["api_key"]);
    let raw = format!("{{{}", " ".repeat(MAX_OBSERVATION_RECORD_BYTES));

    assert!(matches!(
        redact_lcm_sensitive_payload(&raw, &policy),
        Err(DetectionError::ScanLimitExceeded)
    ));
}

#[test]
fn provider_metadata_json_is_structurally_sanitized_or_rejected() {
    let secret = ["s", "k", "-metadata-", "1234567890abcdef"].concat();
    let raw = json!({
        "nested": {
            "authorization": format!("Bearer {secret}"),
            "safe": "retained"
        }
    })
    .to_string();

    let sanitized =
        sanitize_provider_metadata_json(&raw, 4_096).expect("valid bounded metadata is sanitized");
    assert_eq!(sanitized["nested"]["safe"], "retained");
    assert!(
        sanitized["nested"]["authorization"]
            .as_str()
            .is_some_and(|value| value.starts_with("[TraceDecay redacted:"))
    );
    assert!(!sanitized.to_string().contains(&secret));

    assert!(sanitize_provider_metadata_json("{malformed", 4_096).is_none());
    assert!(sanitize_provider_metadata_json(&raw, 8).is_none());
}
