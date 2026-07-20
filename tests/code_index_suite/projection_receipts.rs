use std::fmt::Debug;

use tracedecay::code_index::projection::{
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
