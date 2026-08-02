use std::fmt::Debug;

use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionPublicationErrorV1,
    ProjectionReceiptErrorV1, ProjectionSinkErrorV1, batch_proves_zero_work, build_batch_receipt,
    expected_request_digest, project_for_publication, verify_batch_receipt,
};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn generation(sequence: u64) -> CodeGenerationId {
    id(&format!("generation.v1.aaaaaaaa.{sequence:08}"))
}

fn chunk(name: &str) -> CodeSearchChunkId {
    id(&format!("chunk.v1.{name}"))
}

fn projection_key() -> ProjectionKeyV1 {
    ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.v1".to_owned(),
        profile_digest: digest::<ManifestDigest>('e'),
    }
}

fn changeset() -> ChangedCodeChunkSetV1 {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(generation(1)),
        to_generation: generation(2),
        manifest_digest: digest::<ManifestDigest>('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("updated"),
            prior_digest: Some(digest::<ContentDigest>('a')),
            current_digest: Some(digest::<ContentDigest>('b')),
        }],
        deleted: vec![],
        reused: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("reused"),
            prior_digest: Some(digest::<ContentDigest>('c')),
            current_digest: Some(digest::<ContentDigest>('c')),
        }],
    };
    changes.manifest_digest = changes.compute_digest().expect("changeset digest");
    changes
}

fn request() -> ProjectionBatchRequestV1 {
    request_for(changeset())
}

fn request_for(changes: ChangedCodeChunkSetV1) -> ProjectionBatchRequestV1 {
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest::<ManifestDigest>('0'),
        changes,
        previous_projection_key: Some(projection_key()),
        target_projection_key: projection_key(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = expected_request_digest(&request).expect("request digest");
    request
}

fn decisions() -> Vec<ChunkProjectionDecisionV1> {
    vec![
        ChunkProjectionDecisionV1 {
            chunk_id: chunk("updated"),
            prior_chunk_digest: Some(digest::<ContentDigest>('a')),
            current_chunk_digest: Some(digest::<ContentDigest>('b')),
            operation: ProjectionOperationV1::Updated,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(digest::<ContentDigest>('d')),
        },
        ChunkProjectionDecisionV1 {
            chunk_id: chunk("reused"),
            prior_chunk_digest: Some(digest::<ContentDigest>('c')),
            current_chunk_digest: Some(digest::<ContentDigest>('c')),
            operation: ProjectionOperationV1::Reused,
            outcome: ProjectionOutcomeV1::Reused,
            output_digest: None,
        },
    ]
}

struct FixedSink {
    receipt: ProjectionBatchReceiptV1,
    seen_request: Option<ProjectionBatchRequestV1>,
}

impl CodeChunkProjectionSink for FixedSink {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        self.seen_request = Some(request);
        Ok(self.receipt.clone())
    }
}

#[derive(Default)]
struct ApplyingReplaySink {
    seen_request: Option<ProjectionBatchRequestV1>,
}

impl CodeChunkProjectionSink for ApplyingReplaySink {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        self.seen_request = Some(request.clone());
        let decisions = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(digest::<ContentDigest>('d')),
            })
            .collect::<Vec<_>>();
        build_batch_receipt(&request, &decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

#[test]
fn noop_publication_bypasses_the_projector_and_proves_zero_work() {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(generation(1)),
        to_generation: generation(2),
        manifest_digest: digest::<ManifestDigest>('0'),
        added_or_changed: vec![],
        deleted: vec![],
        reused: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("reused"),
            prior_digest: Some(digest::<ContentDigest>('c')),
            current_digest: Some(digest::<ContentDigest>('c')),
        }],
    };
    changes.manifest_digest = changes.compute_digest().expect("changeset digest");
    let request = request_for(changes);
    let unrelated_request = request_for(changeset());
    let unrelated_receipt =
        build_batch_receipt(&unrelated_request, &decisions()).expect("unrelated receipt");
    let mut sink = FixedSink {
        receipt: unrelated_receipt,
        seen_request: None,
    };

    let handoff =
        project_for_publication(&mut sink, request.clone()).expect("no-op publication handoff");

    assert!(sink.seen_request.is_none(), "no-op must not call projector");
    assert_eq!(handoff.request(), &request);
    assert!(batch_proves_zero_work(handoff.receipt()));
}

#[test]
fn initial_projection_without_a_prior_key_does_not_replay_profile_changes() {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: generation(1),
        manifest_digest: digest::<ManifestDigest>('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("initial"),
            prior_digest: None,
            current_digest: Some(digest::<ContentDigest>('a')),
        }],
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("initial changeset digest");
    let mut request = request_for(changes);
    request.previous_projection_key = None;
    request.replay_reason = ProjectionReplayReasonV1::InitialProjection;
    request.request_digest = expected_request_digest(&request).expect("initial request digest");

    let mut sink = ApplyingReplaySink::default();
    let handoff = project_for_publication(&mut sink, request.clone())
        .expect("initial projection publication handoff");

    assert_eq!(sink.seen_request.as_ref(), Some(&request));
    assert_eq!(handoff.request(), &request);
    assert_eq!(handoff.request().changes.added_or_changed.len(), 1);
}

#[test]
fn request_without_a_prior_key_requires_the_initial_replay_reason() {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: generation(1),
        manifest_digest: digest::<ManifestDigest>('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("initial"),
            prior_digest: None,
            current_digest: Some(digest::<ContentDigest>('a')),
        }],
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("initial changeset digest");
    let mut request = request_for(changes);
    request.previous_projection_key = None;
    request.replay_reason = ProjectionReplayReasonV1::SourceEdit;
    request.request_digest = expected_request_digest(&request).expect("source-edit request digest");

    let mut sink = ApplyingReplaySink::default();
    assert!(matches!(
        project_for_publication(&mut sink, request),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::Contract(_)
        ))
    ));
}

#[test]
fn projection_key_change_replays_reused_chunks_through_the_projector() {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(generation(1)),
        to_generation: generation(2),
        manifest_digest: digest::<ManifestDigest>('0'),
        added_or_changed: vec![],
        deleted: vec![],
        reused: vec![ChangedCodeChunkV1 {
            chunk_id: chunk("reused"),
            prior_digest: Some(digest::<ContentDigest>('c')),
            current_digest: Some(digest::<ContentDigest>('c')),
        }],
    };
    changes.manifest_digest = changes.compute_digest().expect("changeset digest");
    let mut request = request_for(changes);
    request.target_projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.v2".to_owned(),
        profile_digest: digest::<ManifestDigest>('f'),
    };
    request.replay_reason = ProjectionReplayReasonV1::ProjectionProfileChange;
    request.request_digest = expected_request_digest(&request).expect("request digest");
    assert!(
        build_batch_receipt(
            &request,
            &[ChunkProjectionDecisionV1 {
                chunk_id: chunk("reused"),
                prior_chunk_digest: Some(digest::<ContentDigest>('c')),
                current_chunk_digest: Some(digest::<ContentDigest>('c')),
                operation: ProjectionOperationV1::Reused,
                outcome: ProjectionOutcomeV1::Reused,
                output_digest: None,
            }],
        )
        .is_err()
    );
    let mut sink = ApplyingReplaySink::default();

    let handoff = project_for_publication(&mut sink, request.clone())
        .expect("projection-key replay publication handoff");

    let projected = sink.seen_request.as_ref().expect("projector request");
    assert!(projected.changes.reused.is_empty());
    assert!(projected.changes.deleted.is_empty());
    assert_eq!(projected.changes.added_or_changed.len(), 1);
    assert_eq!(
        projected.changes.added_or_changed[0].chunk_id,
        chunk("reused")
    );
    assert!(projected.changes.added_or_changed[0].prior_digest.is_none());
    assert_eq!(handoff.request(), projected);
    assert!(!batch_proves_zero_work(handoff.receipt()));
    assert!(handoff.receipt().receipts.iter().all(|receipt| {
        receipt.operation == ProjectionOperationV1::Added
            && receipt.outcome == ProjectionOutcomeV1::Applied
            && receipt.output_digest.is_some()
    }));
}

#[test]
fn receipt_construction_rejects_tampered_request_digest_and_invalid_reuse_outcome() {
    let mut tampered_request = request();
    tampered_request.request_digest = digest::<ManifestDigest>('9');
    assert_eq!(
        build_batch_receipt(&tampered_request, &decisions()),
        Err(ProjectionReceiptErrorV1::DigestMismatch)
    );

    let request = request();
    let mut invalid_decisions = decisions();
    invalid_decisions[0].outcome = ProjectionOutcomeV1::Reused;
    invalid_decisions[0].output_digest = None;
    assert_eq!(
        build_batch_receipt(&request, &invalid_decisions),
        Err(ProjectionReceiptErrorV1::InconsistentOperation(chunk(
            "updated"
        )))
    );
}

#[test]
fn valid_receipt_is_deterministic_and_becomes_an_atomic_publication_handoff() {
    let request = request();
    let receipt = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    let replay = build_batch_receipt(&request, &decisions()).expect("receipt replays");
    assert_eq!(receipt, replay);
    verify_batch_receipt(&request, &receipt).expect("receipt validates");

    let mut sink = FixedSink {
        receipt: receipt.clone(),
        seen_request: None,
    };
    let handoff = project_for_publication(&mut sink, request.clone()).expect("publication handoff");

    assert_eq!(sink.seen_request.as_ref(), Some(&request));
    assert_eq!(handoff.request(), &request);
    assert_eq!(handoff.receipt(), &receipt);
    assert_eq!(handoff.publication_digest(), &receipt.publication_digest);
    assert_eq!(handoff.source_generation(), &generation(2));
}

#[test]
fn invalid_or_failed_receipts_never_cross_the_publication_handoff() {
    let request = request();
    let valid = build_batch_receipt(&request, &decisions()).expect("receipt builds");

    let mut wrong_key = valid.clone();
    wrong_key.target_projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Graph,
        ..projection_key()
    };
    let mut sink = FixedSink {
        receipt: wrong_key,
        seen_request: None,
    };
    assert_eq!(
        project_for_publication(&mut sink, request.clone()),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::WrongProjectionKey
        ))
    );

    let mut failed_decisions = decisions();
    failed_decisions[0].outcome = ProjectionOutcomeV1::Failed {
        reason: "projector unavailable".to_owned(),
    };
    failed_decisions[0].output_digest = None;
    let failed =
        build_batch_receipt(&request, &failed_decisions).expect("failed receipt is inspectable");
    verify_batch_receipt(&request, &failed).expect("failed receipt remains valid evidence");
    let mut sink = FixedSink {
        receipt: failed,
        seen_request: None,
    };
    assert_eq!(
        project_for_publication(&mut sink, request),
        Err(ProjectionPublicationErrorV1::NotActivatable)
    );
}

/// A receipt returned by a projection sink crosses a trust boundary: even
/// though the request digest was already recomputed earlier in the same
/// publication chain, the receipt's own publication seal must still be
/// recomputed before it can publish.
#[test]
fn sink_receipt_with_a_tampered_publication_seal_never_publishes() {
    let request = request();
    let mut tampered = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    tampered.publication_digest = digest::<ManifestDigest>('9');

    let mut sink = FixedSink {
        receipt: tampered,
        seen_request: None,
    };
    assert_eq!(
        project_for_publication(&mut sink, request),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::DigestMismatch
        ))
    );
}

/// A sink is likewise not trusted about which request its receipt answers.
#[test]
fn sink_receipt_with_a_foreign_request_digest_never_publishes() {
    let request = request();
    let mut tampered = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    tampered.request_digest = digest::<ManifestDigest>('9');

    let mut sink = FixedSink {
        receipt: tampered,
        seen_request: None,
    };
    assert_eq!(
        project_for_publication(&mut sink, request),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::DigestMismatch
        ))
    );
}
