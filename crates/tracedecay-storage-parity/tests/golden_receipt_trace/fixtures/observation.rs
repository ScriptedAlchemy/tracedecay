//! Observation, cursor and remote replay payloads for a project-sessions shard.

use serde_json::json;
use tracedecay_domain::test_fixtures::digest;
use tracedecay_domain::{
    BrainNodeId, ComponentVersion, DurableObservationV1, EntityId, ManifestDigest, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId,
    RemoteWriterFenceV1, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
    canonical_sha256,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationWrite, RemoteObservationReplayWriteV1, RemoteWriterFenceInstallV1,
    StoreRuntimeBindingV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor,
};

fn source(session: &str) -> ObservationSourceIdentityV1 {
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new("provider.parity").unwrap(),
        SessionId::new(session).unwrap(),
    )
    .unwrap()
}

fn scope(project_id: &ProjectId) -> ObservationScopeV1 {
    ObservationScopeV1::Project {
        project_id: project_id.clone(),
    }
}

/// Identity is (session, generation 1, range, record id); body and receipt
/// are the evidence.
pub struct ObservationSpec<'a> {
    pub session: &'a str,
    pub record: &'a str,
    pub body: &'a str,
    pub receipt: &'a str,
    pub range: (u64, u64),
    pub expected_cursor: Option<ObservationSourceCursorV1>,
}

pub fn observation(project_id: &ProjectId, spec: ObservationSpec<'_>) -> AnchoredObservationWrite {
    let source = source(spec.session);
    let scope = scope(project_id);
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(spec.range.0, spec.range.1).unwrap();
    let payload = json!({ "kind": "assistant_message", "body": spec.body });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(spec.receipt).unwrap(),
            ComponentVersion::new("sanitizer.parity.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new(spec.record).unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.parity").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        source,
        scope,
        generation,
        ObservationOrderingDomainV1::SqliteRowId,
        range.end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, spec.expected_cursor, next_cursor).unwrap();
    let projection_generation = ProjectionGenerationId::new("projection.parity.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "runtime.parity.v1")
            .unwrap();
    let anchor = build_observation_retrieval_anchor(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

pub fn single(project_id: &ProjectId, body: &str, receipt: &str) -> AnchoredObservationWrite {
    observation(
        project_id,
        ObservationSpec {
            session: "session.parity",
            record: "record.parity",
            body,
            receipt,
            range: (0, 1),
            expected_cursor: None,
        },
    )
}

pub fn batch(project_id: &ProjectId) -> Box<[AnchoredObservationWrite]> {
    let first = observation(
        project_id,
        ObservationSpec {
            session: "session.batch",
            record: "record.batch-1",
            body: "batch one",
            receipt: "receipt.batch-1",
            range: (0, 1),
            expected_cursor: None,
        },
    );
    let second = observation(
        project_id,
        ObservationSpec {
            session: "session.batch",
            record: "record.batch-2",
            body: "batch two",
            receipt: "receipt.batch-2",
            range: (1, 2),
            expected_cursor: Some(first.next_cursor().clone()),
        },
    );
    vec![first, second].into_boxed_slice()
}

pub fn cursor_advance(project_id: &ProjectId) -> ObservationCursorAdvance {
    ObservationCursorAdvance::for_ordering(
        source("session.cursor"),
        scope(project_id),
        ObservationSourceGenerationV1::new(1).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        None,
        ObservationSourceRangeV1::new(0, 1).unwrap(),
        ObservationCoverageReason::BlankFrame,
    )
    .unwrap()
}

fn writer_fence(epoch: u64, placement_revision: u64, node: &str) -> RemoteWriterFenceV1 {
    serde_json::from_value(json!({
        "brain_id": "brain.remote",
        "shard_id": "shard.remote",
        "generation_id": "generation.remote",
        "placement_revision": placement_revision,
        "authority_epoch": epoch,
        "authority_node_id": node,
    }))
    .unwrap()
}

fn authority_key(fence: &RemoteWriterFenceV1) -> ManifestDigest {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &fence.brain_id,
        &fence.shard_id,
        &fence.generation_id,
    ))
    .unwrap()
}

/// Seeds the fence (`expected: None`).
pub fn fence_install(
    binding: &StoreRuntimeBindingV1,
    project_id: &ProjectId,
) -> RemoteWriterFenceInstallV1 {
    let replacement = writer_fence(1, 1, "node.remote");
    RemoteWriterFenceInstallV1 {
        project_id: project_id.clone(),
        target_binding: binding.clone(),
        authority_key: authority_key(&replacement),
        expected: None,
        replacement,
        installed_at: UtcMicros(10),
    }
}

/// Compare-and-swap promotion over the seeded fence.
pub fn fence_promotion(
    binding: &StoreRuntimeBindingV1,
    project_id: &ProjectId,
) -> RemoteWriterFenceInstallV1 {
    let replacement = writer_fence(2, 2, "node.promoted");
    RemoteWriterFenceInstallV1 {
        project_id: project_id.clone(),
        target_binding: binding.clone(),
        authority_key: authority_key(&replacement),
        expected: Some(writer_fence(1, 1, "node.remote")),
        replacement,
        installed_at: UtcMicros(11),
    }
}

pub fn remote_replay(project_id: &ProjectId) -> RemoteObservationReplayWriteV1 {
    let writer_fence = writer_fence(1, 1, "node.remote");
    RemoteObservationReplayWriteV1 {
        event_id: "event.remote.parity.0001".to_owned(),
        authority_key: authority_key(&writer_fence),
        frame_digest: digest('a'),
        enrollment_id: EntityId::new("enrollment.remote").unwrap(),
        enrollment_revision: 1,
        node_id: BrainNodeId::new("node.remote").unwrap(),
        policy_revision: 1,
        capture_sequence: 1,
        previous_event_id: None,
        project_id: project_id.clone(),
        writer_fence,
        captured_at: UtcMicros(20),
        command_digest: digest('b'),
        observation: observation(
            project_id,
            ObservationSpec {
                session: "session.remote",
                record: "record.remote-1",
                body: "remote one",
                receipt: "receipt.remote-1",
                range: (0, 1),
                expected_cursor: None,
            },
        ),
    }
}
