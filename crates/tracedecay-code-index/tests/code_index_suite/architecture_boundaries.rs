use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::projection::{
    CodeChunkProjectionSink, ProjectionSinkErrorV1, batch_proves_zero_work,
    expected_request_digest, project_for_publication,
};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, CodeGenerationId, CodeSearchChunkId, CommitId, ContentDigest,
    FileOccurrenceId, LanguageId, ManifestDigest, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionReplayReasonV1, RefId,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeSnapshotV1, WorktreeId,
};

use crate::support::{id, registry};

struct RejectingSink;

impl CodeChunkProjectionSink for RejectingSink {
    fn project_changed_chunks(
        &mut self,
        _request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        Err(ProjectionSinkErrorV1::Rejected(
            "no-op must bypass the publication adapter".to_owned(),
        ))
    }
}

fn admit<I: CodeIndexIntake>(
    intake: &I,
    snapshot: SanitizedCodeSnapshotV1,
) -> ValidatedCodeSnapshotV1 {
    intake
        .validate(snapshot)
        .expect("admit through intake port")
}

fn no_op_request() -> ProjectionBatchRequestV1 {
    let generation = id::<CodeGenerationId>("generation.v1.aaaaaaaa.00000001");
    let chunk = id::<CodeSearchChunkId>("chunk.v1.boundary");
    let digest = id::<ContentDigest>(&format!("sha256:{}", "a".repeat(64)));
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(generation.clone()),
        to_generation: id::<CodeGenerationId>("generation.v1.aaaaaaaa.00000002"),
        manifest_digest: id::<ManifestDigest>(&format!("sha256:{}", "0".repeat(64))),
        added_or_changed: vec![],
        deleted: vec![],
        reused: vec![tracedecay_domain::ChangedCodeChunkV1 {
            chunk_id: chunk,
            prior_digest: Some(digest.clone()),
            current_digest: Some(digest),
        }],
    };
    changes.manifest_digest = changes.compute_digest().expect("changes digest");
    let projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "b".repeat(64))),
    };
    let mut request = ProjectionBatchRequestV1 {
        request_digest: id::<ManifestDigest>(&format!("sha256:{}", "0".repeat(64))),
        changes,
        previous_projection_key: Some(projection_key.clone()),
        target_projection_key: projection_key,
        replay_reason: ProjectionReplayReasonV1::VerificationReplay,
    };
    request.request_digest = expected_request_digest(&request).expect("request digest");
    request
}

#[test]
fn indexer_entry_and_exit_are_only_the_intake_and_projection_ports() {
    let source = b"pub fn accepted() {}\n";
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repo.boundary"),
        worktree: Some(id::<WorktreeId>("worktree.boundary")),
        reference: Some(id::<RefId>("refs/heads/main")),
        source_revision: Some(id::<CommitId>("commit.boundary")),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.boundary")],
        content_identity: tracedecay_code_index::chunks::content_digest(source),
        captured_at: UtcMicros(1_000_000),
        files: vec![SanitizedCodeFileV1 {
            file_occurrence_id: id::<FileOccurrenceId>("file.boundary"),
            logical_path: "src/lib.rs".to_owned(),
            language: Some(id::<LanguageId>("rust")),
            content_digest: tracedecay_code_index::chunks::content_digest(source),
            disposition: SnapshotFileDispositionV1::Present,
        }],
    };
    let intake = SanitizedCodeIntake::new(
        registry(),
        id::<SanitizerRevision>("sanitizer.v1"),
        UtcMicros(1_000_000),
    );

    let admitted = admit(&intake, snapshot);
    assert_eq!(admitted.snapshot.files.len(), 1);

    let request = no_op_request();
    let mut sink = RejectingSink;
    let handoff = project_for_publication(&mut sink, request.clone())
        .expect("publish through projection port");
    assert_eq!(handoff.request(), &request);
    assert!(batch_proves_zero_work(handoff.receipt()));
}
