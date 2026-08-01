//! Equivalence proofs for lazily memoized projection output digests.
//!
//! Each test recomputes the digest with the exact pre-memoization expression —
//! the same canonical JSON shape fed through `PayloadReferenceV1::for_payload`
//! — and asserts the memoized accessor returns identical bytes.

use serde_json::json;
use tracedecay_domain::{
    ComponentVersion, ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
    ObservationScopeV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, SanitizationReceiptId, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId,
};

use super::*;

fn observation(seed: &str) -> DurableObservationV1 {
    let provider = ProviderId::new("provider.fixture").unwrap();
    let session_id = SessionId::new(format!("session.{seed}")).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id).unwrap();
    let payload = json!({"kind": "assistant_message", "body": seed});
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.{seed}")).unwrap(),
            ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(7).unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new(format!("record.{seed}")).unwrap(),
        )
        .unwrap(),
        receipt,
        tracedecay_domain::RetentionClass::new("retention.fixture").unwrap(),
        payload,
    )
    .unwrap()
}

fn session_record(seed: &str) -> SessionRecord {
    SessionRecord {
        provider: "provider.fixture".to_owned(),
        session_id: format!("session.{seed}"),
        project_key: "project.fixture".to_owned(),
        project_path: "/fixture/project".to_owned(),
        title: Some(format!("title {seed}")),
        started_at: Some(11),
        ended_at: Some(29),
        transcript_path: Some("/fixture/transcript.jsonl".to_owned()),
        metadata_json: Some(r#"{"zeta":1,"alpha":2}"#.to_owned()),
        parent_session_id: Some("session.parent".to_owned()),
        is_subagent: true,
        agent_id: Some("agent.fixture".to_owned()),
        parent_tool_use_id: Some("tool.parent".to_owned()),
    }
}

fn message_record(seed: &str) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "provider.fixture".to_owned(),
        message_id: format!("message.{seed}"),
        session_id: format!("session.{seed}"),
        role: "assistant".to_owned(),
        timestamp: Some(17),
        ordinal: 3,
        text: format!("body {seed}"),
        kind: Some("message".to_owned()),
        model: Some("model.fixture".to_owned()),
        tool_names: Some("read,write".to_owned()),
        source_path: Some("/fixture/source.jsonl".to_owned()),
        source_offset: Some(64),
        metadata_json: Some(r#"{"zeta":3,"alpha":4}"#.to_owned()),
    }
}

fn workflow_fact_record() -> WorkflowFactRecord {
    WorkflowFactRecord {
        fact_ordinal: 2,
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        provider_reference: Some("provider.reference".to_owned()),
        item_id: Some("item.fixture".to_owned()),
        parent_reference: Some("parent.fixture".to_owned()),
        list_reference: Some("list.fixture".to_owned()),
        state: Some("open".to_owned()),
        status: Some("in_progress".to_owned()),
        item_order: Some(5),
        native_revision: Some("rev.7".to_owned()),
        event_sequence: Some(13),
        source_sequence: Some(19),
        native_timestamp: Some(23),
        ordering_domain: "ordering.fixture".to_owned(),
        content: Some(json!({"zeta": 1, "alpha": {"nested": true}})),
        content_text: "goal text".to_owned(),
    }
}

/// The pre-memoization message-digest expression, byte for byte.
fn eager_message_digest(
    session: &SessionRecord,
    message: &SessionMessageRecord,
    output_ordinal: u32,
) -> PayloadDigestV1 {
    let digest_value = serde_json::json!({
        "projector_version": SESSION_MESSAGE_PROJECTOR_VERSION,
        "output_ordinal": output_ordinal,
        "session": session,
        "message": message,
    });
    PayloadReferenceV1::for_payload(&digest_value)
        .unwrap()
        .digest()
        .clone()
}

/// The pre-memoization workflow-fact-digest expression, byte for byte.
fn eager_workflow_digest(session: &SessionRecord, fact: &WorkflowFactRecord) -> PayloadDigestV1 {
    let digest_value = serde_json::json!({
        "projector_version": SESSION_MESSAGE_PROJECTOR_VERSION,
        "session": session,
        "fact": {
            "fact_ordinal": fact.fact_ordinal,
            "semantic_kind": fact.semantic_kind,
            "provider_reference": fact.provider_reference,
            "item_id": fact.item_id,
            "parent_reference": fact.parent_reference,
            "list_reference": fact.list_reference,
            "state": fact.state,
            "status": fact.status,
            "item_order": fact.item_order,
            "native_revision": fact.native_revision,
            "event_sequence": fact.event_sequence,
            "source_sequence": fact.source_sequence,
            "native_timestamp": fact.native_timestamp,
            "ordering_domain": fact.ordering_domain,
            "content": fact.content,
            "content_text": fact.content_text,
        },
    });
    PayloadReferenceV1::for_payload(&digest_value)
        .unwrap()
        .digest()
        .clone()
}

#[test]
fn memoized_message_digest_equals_eager_derivation() {
    let observation = observation("alpha");
    let session = session_record("alpha");
    let message = message_record("alpha");
    let expected = eager_message_digest(&session, &message, 0);

    let projection = ObservationProjection::for_message(&observation, session, message).unwrap();
    let output = projection.message().unwrap();

    assert_eq!(output.output_digest().unwrap(), &expected);
    // Memoization is idempotent: a second read returns the same bytes.
    assert_eq!(output.output_digest().unwrap(), &expected);
}

#[test]
fn memoized_digests_match_eager_derivation_for_every_output_ordinal() {
    let observation = observation("beta");
    let outputs: Vec<_> = ["one", "two", "three"]
        .into_iter()
        .map(|seed| (session_record(seed), message_record(seed)))
        .collect();
    let expected: Vec<PayloadDigestV1> = outputs
        .iter()
        .enumerate()
        .map(|(ordinal, (session, message))| {
            eager_message_digest(session, message, u32::try_from(ordinal).unwrap())
        })
        .collect();

    let projection = ObservationProjection::for_outputs(&observation, outputs, Vec::new()).unwrap();
    let actual: Vec<PayloadDigestV1> = projection
        .messages()
        .map(|output| output.output_digest().unwrap().clone())
        .collect();

    assert_eq!(actual, expected);
}

#[test]
fn memoized_workflow_fact_digest_equals_eager_derivation() {
    let observation = observation("gamma");
    let session = session_record("gamma");
    let fact = workflow_fact_record();
    let expected = eager_workflow_digest(&session, &fact);

    let projection = ObservationProjection::for_outputs(
        &observation,
        vec![(session_record("gamma"), message_record("gamma"))],
        vec![(session, fact)],
    )
    .unwrap();
    let workflow_facts = projection.workflow_facts();

    assert_eq!(workflow_facts.len(), 1);
    assert_eq!(workflow_facts[0].output_digest().unwrap(), &expected);
    assert_eq!(workflow_facts[0].output_digest().unwrap(), &expected);
}

#[test]
fn equality_ignores_whether_the_digest_memo_is_materialized() {
    let observation = observation("delta");
    let left = ObservationProjection::for_message(
        &observation,
        session_record("delta"),
        message_record("delta"),
    )
    .unwrap();
    let right = ObservationProjection::for_message(
        &observation,
        session_record("delta"),
        message_record("delta"),
    )
    .unwrap();

    // Materialize only one side's memo.
    let _ = left.message().unwrap().output_digest().unwrap();

    assert_eq!(left, right);
    assert_eq!(
        left.message().unwrap().output_digest().unwrap(),
        right.message().unwrap().output_digest().unwrap()
    );
}

#[test]
fn cloning_a_projection_preserves_the_derived_digest() {
    let observation = observation("epsilon");
    let session = session_record("epsilon");
    let message = message_record("epsilon");
    let expected = eager_message_digest(&session, &message, 0);

    let projection = ObservationProjection::for_message(&observation, session, message).unwrap();
    let before_clone = projection.clone();
    let _ = projection.message().unwrap().output_digest().unwrap();
    let after_clone = projection.clone();

    assert_eq!(
        before_clone.message().unwrap().output_digest().unwrap(),
        &expected
    );
    assert_eq!(
        after_clone.message().unwrap().output_digest().unwrap(),
        &expected
    );
}

#[test]
fn provenance_is_shared_across_every_output_of_one_observation() {
    let observation = observation("zeta");
    let outputs: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|seed| (session_record(seed), message_record(seed)))
        .collect();
    let projection = ObservationProjection::for_outputs(
        &observation,
        outputs,
        vec![(session_record("zeta"), workflow_fact_record())],
    )
    .unwrap();

    let expected = ProjectionProvenance::for_observation(&observation).unwrap();
    for output in projection.messages() {
        assert_eq!(output.provenance(), &expected);
    }
    for fact in projection.workflow_facts() {
        assert_eq!(fact.provenance(), &expected);
    }
}
