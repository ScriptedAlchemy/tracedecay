use std::fmt::Debug;

use super::{
    ChunkProjectionDecisionV1, ProjectionPublicationErrorV1, ProjectionPublicationHandoffV1,
    ProjectionReceiptErrorV1, build_batch_receipt, expected_request_digest,
    project_for_publication, verify_batch_receipt,
};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1,
};

use tracedecay_domain::test_fixtures::id;

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
    let reused = [(chunk("reused"), digest::<ContentDigest>('c'))];
    let (reused_count, reused_digest) =
        ChangedCodeChunkSetV1::seal_reused_partition(&reused).expect("reused seal");
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
        reused_count,
        reused_digest,
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
    vec![ChunkProjectionDecisionV1 {
        chunk_id: chunk("updated"),
        prior_chunk_digest: Some(digest::<ContentDigest>('a')),
        current_chunk_digest: Some(digest::<ContentDigest>('b')),
        operation: ProjectionOperationV1::Updated,
        outcome: ProjectionOutcomeV1::Applied,
        output_digest: Some(digest::<ContentDigest>('b')),
    }]
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
        reused_count: 0,
        reused_digest: ChangedCodeChunkSetV1::seal_reused_partition(&[])
            .expect("empty reused seal")
            .1,
    };
    changes.manifest_digest = changes.compute_digest().expect("initial changeset digest");
    let mut request = request_for(changes);
    request.previous_projection_key = None;
    request.replay_reason = ProjectionReplayReasonV1::SourceEdit;
    request.request_digest = expected_request_digest(&request).expect("source-edit request digest");

    assert!(matches!(
        project_for_publication(request),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::Contract(_)
        ))
    ));
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
fn source_edit_receipt_authenticates_reuse_without_per_chunk_rows() {
    let request = request();
    let expected = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    let handoff = project_for_publication(request.clone()).expect("publication prepared");
    let receipt = handoff.receipt();
    assert_eq!(receipt, &expected);
    assert_eq!(
        ProjectionPublicationHandoffV1::restore(request.clone(), receipt.clone())
            .expect("durable handoff restores"),
        handoff
    );

    assert_eq!(receipt.reused_count, 1);
    assert_eq!(receipt.receipts.len(), 1);
    assert_eq!(receipt.receipts[0].chunk_id, chunk("updated"));
    verify_batch_receipt(&request, receipt).expect("compact reuse remains verified");
}

#[test]
fn invalid_or_failed_receipts_cannot_restore_a_publication_handoff() {
    let request = request();
    let valid = build_batch_receipt(&request, &decisions()).expect("receipt builds");

    let mut wrong_key = valid.clone();
    wrong_key.target_projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Graph,
        ..projection_key()
    };
    assert_eq!(
        ProjectionPublicationHandoffV1::restore(request.clone(), wrong_key),
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
    assert_eq!(
        ProjectionPublicationHandoffV1::restore(request, failed),
        Err(ProjectionPublicationErrorV1::NotActivatable)
    );
}

/// Restored evidence must authenticate its own publication seal.
#[test]
fn restored_receipt_with_a_tampered_publication_seal_is_rejected() {
    let request = request();
    let mut tampered = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    tampered.publication_digest = digest::<ManifestDigest>('9');

    assert_eq!(
        ProjectionPublicationHandoffV1::restore(request, tampered),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::DigestMismatch
        ))
    );
}

/// Restored evidence must bind the exact publication request.
#[test]
fn restored_receipt_with_a_foreign_request_digest_is_rejected() {
    let request = request();
    let mut tampered = build_batch_receipt(&request, &decisions()).expect("receipt builds");
    tampered.request_digest = digest::<ManifestDigest>('9');

    assert_eq!(
        ProjectionPublicationHandoffV1::restore(request, tampered),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::DigestMismatch
        ))
    );
}

#[test]
fn canonical_receipts_preserve_additions_deletions_and_compact_noops() {
    let mut changes = changeset();
    changes.added_or_changed.insert(
        0,
        ChangedCodeChunkV1 {
            chunk_id: chunk("added"),
            prior_digest: None,
            current_digest: Some(digest::<ContentDigest>('a')),
        },
    );
    changes.deleted.push(ChangedCodeChunkV1 {
        chunk_id: chunk("deleted"),
        prior_digest: Some(digest::<ContentDigest>('f')),
        current_digest: None,
    });
    changes.manifest_digest = changes.compute_digest().expect("changeset digest");
    let request = request_for(changes);
    let handoff = project_for_publication(request.clone()).expect("mixed publication");
    let receipt = handoff.receipt();
    verify_batch_receipt(&request, receipt).expect("canonical receipt verifies");
    let operations: Vec<_> = receipt
        .receipts
        .iter()
        .map(|receipt| {
            (
                receipt.chunk_id.clone(),
                receipt.operation,
                receipt.output_digest.clone(),
            )
        })
        .collect();
    assert_eq!(
        operations,
        vec![
            (
                chunk("added"),
                ProjectionOperationV1::Added,
                Some(digest('a'))
            ),
            (chunk("deleted"), ProjectionOperationV1::Deleted, None),
            (
                chunk("updated"),
                ProjectionOperationV1::Updated,
                Some(digest('b'))
            ),
        ]
    );
    assert_eq!(receipt.reused_count, 1);

    let mut noop = changeset();
    noop.added_or_changed.clear();
    noop.manifest_digest = noop.compute_digest().expect("noop digest");
    let request = request_for(noop);
    let first = project_for_publication(request.clone()).expect("noop publication");
    assert_eq!(first.receipt().reused_count, 1);
    assert!(first.receipt().receipts.is_empty());
    assert_eq!(
        project_for_publication(request).expect("repeat noop publication"),
        first,
    );
}

#[test]
fn projection_key_replay_requires_expanded_chunks_and_binds_the_new_key() {
    let mut request = request();
    request.target_projection_key.schema_revision = "lexical.v2".to_owned();
    request.replay_reason = ProjectionReplayReasonV1::ProjectionProfileChange;
    request.request_digest = expected_request_digest(&request).expect("request digest");
    assert!(matches!(
        project_for_publication(request.clone()),
        Err(ProjectionPublicationErrorV1::Receipt(
            ProjectionReceiptErrorV1::Contract(_)
        ))
    ));

    let (count, digest) = ChangedCodeChunkSetV1::seal_reused_partition(&[]).expect("empty reuse");
    request.changes.reused_count = count;
    request.changes.reused_digest = digest;
    request.changes.manifest_digest = request.changes.compute_digest().expect("expanded digest");
    request.request_digest = expected_request_digest(&request).expect("expanded request digest");
    let handoff = project_for_publication(request.clone()).expect("expanded replay");
    assert_eq!(
        handoff.receipt().target_projection_key,
        request.target_projection_key
    );
    assert_eq!(handoff.receipt().receipts.len(), 1);
    verify_batch_receipt(&request, handoff.receipt()).expect("expanded replay verifies");
}
