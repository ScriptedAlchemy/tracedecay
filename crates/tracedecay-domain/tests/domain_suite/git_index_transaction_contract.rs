use schemars::schema_for;
use tracedecay_domain::git::repository_state::{
    RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
};
use tracedecay_domain::{
    GitBlobExpectationV1, GitCoverageV1, GitFileModeV1, GitHeadStateV1, GitIndexEntryExpectationV1,
    GitIndexJournalPhaseV1, GitIndexPreviewDispositionV1, GitIndexPreviewId,
    GitIndexPreviewInputV1, GitIndexPreviewV1, GitIndexReceiptId, GitIndexReceiptOutcomeV1,
    GitIndexTransactionId, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitObjectFormatV1, GitOidV1, GitOperationStateV1, HunkDirectionV1, HunkRefV1,
    MAX_GIT_INDEX_PREVIEW_INPUT_HUNKS, ManifestDigest, ProjectId, RepositoryId, UtcMicros,
    WorktreeId,
};

use tracedecay_domain::test_fixtures::id;

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture oid is canonical")
}

use tracedecay_domain::test_fixtures::digest;

#[test]
fn receipt_outcome_schema_preserves_the_exact_wire_states() {
    let schema =
        serde_json::to_value(schema_for!(GitIndexReceiptOutcomeV1)).expect("outcome schema");
    assert_eq!(
        schema["enum"],
        serde_json::json!(["committed", "aborted_no_change", "needs_inspection"])
    );
}

fn snapshot() -> RepositoryStateSnapshotV1 {
    RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        Some(id::<WorktreeId>("worktree.fixture")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::TrackedDirty,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        GitOperationStateV1::None,
        Some(digest('0')),
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("fixture snapshot is valid")
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('7'),
    )
    .expect("fixture native identity is valid")
}

fn hunk(preview_id: &GitIndexPreviewId, snapshot_digest: ManifestDigest) -> HunkRefV1 {
    HunkRefV1 {
        repository: id("repository.fixture"),
        worktree: id("worktree.fixture"),
        direction: HunkDirectionV1::WorkingTreeToIndex,
        path: "src/lib.rs".to_owned(),
        original_path: None,
        expected_base_blob: GitBlobExpectationV1::Present(oid('c')),
        expected_index_entry: GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::Present(oid('c')),
            mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).expect("regular mode")),
            unmerged_stage: None,
        },
        expected_worktree_blob: Some(GitBlobExpectationV1::Present(oid('e'))),
        expected_worktree_mode: Some(
            GitFileModeV1::new(GitFileModeV1::REGULAR).expect("regular mode"),
        ),
        hunk_header: "@@ -1,1 +1,1 @@".to_owned(),
        context_digest: digest('f'),
        patch_digest: digest('0'),
        selected_line_bitmap: vec![1],
        attributes_digest: None,
        preview_id: preview_id.as_str().to_owned(),
        schema_version: "hunkref.v1".to_owned(),
        snapshot_digest,
    }
}

#[test]
fn durable_preview_inputs_bind_bounded_hunks() {
    let repository_snapshot = snapshot();
    let snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&repository_snapshot)
        .expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new("git-preview.input.fixture").expect("preview id");
    let reference = hunk(&preview_id, snapshot_digest.clone());
    let input = GitIndexPreviewInputV1::new_hunk_selection(
        preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        repository_snapshot.clone(),
        vec![reference],
        UtcMicros(10),
        UtcMicros(30_000_010),
    )
    .expect("bounded hunk input");
    input.validate().expect("input remains canonical");
    assert!(!input.is_expired_at(UtcMicros(30_000_009)));
    assert!(input.is_expired_at(UtcMicros(30_000_010)));

    let too_many_preview_id =
        GitIndexPreviewId::new("git-preview.too-many-hunks").expect("preview id");
    assert!(
        GitIndexPreviewInputV1::new_hunk_selection(
            too_many_preview_id.clone(),
            GitIndexTransactionOperationV1::StageHunks,
            repository_snapshot.clone(),
            vec![
                hunk(
                    &too_many_preview_id,
                    GitIndexPreviewV1::repository_snapshot_digest(&repository_snapshot)
                        .expect("snapshot digest")
                );
                MAX_GIT_INDEX_PREVIEW_INPUT_HUNKS + 1
            ],
            UtcMicros(10),
            UtcMicros(30_000_010),
        )
        .is_err(),
        "preview inputs must not turn a bounded hunk read into an unbounded durable payload"
    );
    assert!(
        GitIndexPreviewInputV1::new_hunk_selection(
            GitIndexPreviewId::new("git-preview.long-lived-input").expect("preview id"),
            GitIndexTransactionOperationV1::StageHunks,
            repository_snapshot.clone(),
            vec![hunk(&preview_id, snapshot_digest)],
            UtcMicros(10),
            UtcMicros(30_000_011),
        )
        .is_err(),
        "preview inputs must expire within the fixed handoff lifetime"
    );
}

#[test]
fn applicable_preview_binds_each_hunk_to_one_immutable_snapshot() {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new("git-preview.fixture").expect("preview id");
    let reference = hunk(&preview_id, snapshot_digest.clone());

    let preview = GitIndexPreviewV1::new(
        preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        snapshot.clone(),
        snapshot_digest.clone(),
        vec![reference.clone()],
        Some(oid('e')),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview is valid");
    preview.validate().expect("preview remains immutable");

    let mut stale = reference;
    stale.snapshot_digest = digest('9');
    assert_eq!(
        GitIndexPreviewV1::new(
            preview_id,
            GitIndexTransactionOperationV1::StageHunks,
            snapshot,
            snapshot_digest,
            vec![stale],
            Some(oid('e')),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(10),
            UtcMicros(20),
        )
        .unwrap_err()
        .to_string(),
        "git index preview hunk compare-and-swap binding is not pinned to the required snapshot",
        "a HunkRef from a different repository snapshot must never become applicable"
    );
}

#[test]
fn journal_never_skips_from_prepared_to_committed_or_replays_inspection() {
    assert!(
        GitIndexJournalPhaseV1::Prepared
            .permits_successor(GitIndexJournalPhaseV1::NativeApplyStarted)
    );
    assert!(!GitIndexJournalPhaseV1::Prepared.permits_successor(GitIndexJournalPhaseV1::Committed));
    assert!(
        !GitIndexJournalPhaseV1::NeedsInspection
            .permits_successor(GitIndexJournalPhaseV1::NativeApplyStarted)
    );

    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new("git-preview.phase-evidence").expect("preview id");
    let preview = GitIndexPreviewV1::new(
        preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        snapshot.clone(),
        snapshot_digest.clone(),
        vec![hunk(&preview_id, snapshot_digest)],
        Some(oid('e')),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("stage preview");
    let mut forged = tracedecay_domain::GitIndexTransactionJournalV1::prepared(
        GitIndexTransactionId::new("git-index-transaction.forged-phase").expect("transaction id"),
        &preview,
        UtcMicros(10),
    )
    .expect("prepared journal");
    forged.phase = GitIndexJournalPhaseV1::Verifying;
    assert!(
        forged.validate().is_err(),
        "a phase label without its complete durable epoch chain is not recovery evidence"
    );
}

#[test]
fn restart_recovery_requires_post_boundary_phase_evidence() {
    for phase in [
        GitIndexJournalPhaseV1::Prepared,
        GitIndexJournalPhaseV1::NativeApplyStarted,
    ] {
        assert!(phase.permits_recovered_outcome(GitIndexReceiptOutcomeV1::AbortedNoChange));
        assert!(phase.permits_recovered_outcome(GitIndexReceiptOutcomeV1::NeedsInspection));
        assert!(
            !phase.permits_recovered_outcome(GitIndexReceiptOutcomeV1::Committed),
            "a candidate tree observed before a durable index phase is coincidence, not proof"
        );
    }

    assert!(
        GitIndexJournalPhaseV1::IndexCommitted
            .permits_recovered_outcome(GitIndexReceiptOutcomeV1::Committed,)
    );
    assert!(
        !GitIndexJournalPhaseV1::NeedsInspection
            .permits_recovered_outcome(GitIndexReceiptOutcomeV1::Committed,),
        "inspection records must be reconciled under a separate proven-clear path"
    );
}

#[test]
fn committed_receipt_is_integrity_bound_to_its_preview() {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new("git-preview.receipt.fixture").expect("preview id");
    let reference = hunk(&preview_id, snapshot_digest.clone());
    let preview = GitIndexPreviewV1::new(
        preview_id,
        GitIndexTransactionOperationV1::StageHunks,
        snapshot,
        snapshot_digest,
        vec![reference],
        Some(oid('e')),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview is valid");
    let receipt = GitIndexTransactionReceiptV1::new(
        GitIndexReceiptId::new("git-index-receipt.fixture").expect("receipt id"),
        GitIndexTransactionId::new("git-index-transaction.fixture").expect("transaction id"),
        &preview,
        digest('1'),
        Some(oid('e')),
        Some(oid('a')),
        GitIndexReceiptOutcomeV1::Committed,
        UtcMicros(11),
    )
    .expect("committed receipt is valid");

    receipt.validate().expect("receipt digest is stable");
    let encoded = serde_json::to_string(&receipt).expect("serialize receipt");
    let decoded: GitIndexTransactionReceiptV1 =
        serde_json::from_str(&encoded).expect("deserialize receipt");
    assert_eq!(decoded.receipt_digest, receipt.receipt_digest);
}

#[test]
fn unavailable_terminal_snapshot_is_explicit_and_cannot_claim_commit() {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new("git-preview.unobserved.fixture").expect("preview id");
    let reference = hunk(&preview_id, snapshot_digest.clone());
    let preview = GitIndexPreviewV1::new(
        preview_id,
        GitIndexTransactionOperationV1::StageHunks,
        snapshot,
        snapshot_digest,
        vec![reference],
        Some(oid('e')),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview is valid");
    let transaction_id =
        GitIndexTransactionId::new("git-index-transaction.unobserved").expect("transaction id");

    let receipt = GitIndexTransactionReceiptV1::new_with_final_snapshot(
        GitIndexReceiptId::new("git-index-receipt.unobserved").expect("receipt id"),
        transaction_id.clone(),
        &preview,
        None,
        preview.repository_snapshot.index.tree_id.clone(),
        preview.repository_snapshot.head.commit().cloned(),
        GitIndexReceiptOutcomeV1::NeedsInspection,
        UtcMicros(11),
    )
    .expect("inspection receipt may report an unavailable final snapshot");
    assert!(!receipt.final_snapshot_captured);
    let decoded: GitIndexTransactionReceiptV1 =
        serde_json::from_str(&serde_json::to_string(&receipt).expect("serialize receipt"))
            .expect("deserialize receipt");
    assert_eq!(decoded, receipt);

    assert!(
        GitIndexTransactionReceiptV1::new_with_final_snapshot(
            GitIndexReceiptId::new("git-index-receipt.false-commit").expect("receipt id"),
            transaction_id,
            &preview,
            None,
            Some(oid('e')),
            Some(oid('a')),
            GitIndexReceiptOutcomeV1::Committed,
            UtcMicros(11),
        )
        .is_err(),
        "a committed receipt must contain a captured final snapshot"
    );
}

#[test]
fn snapshot_without_complete_native_identity_is_read_only() {
    let mut value = serde_json::to_value(snapshot()).expect("serialize snapshot");
    value["git_version"] = serde_json::Value::Null;
    value["adapter_revision"] = serde_json::Value::Null;
    value["refs_digest"] = serde_json::Value::Null;
    value["snapshot_id"] = serde_json::json!("repository.state.v1.invalid");
    assert!(serde_json::from_value::<RepositoryStateSnapshotV1>(value).is_err());

    let state = RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.read-only"),
        id::<RepositoryId>("repository.read-only"),
        Some(id::<WorktreeId>("worktree.read-only")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        GitOperationStateV1::None,
        Some(digest('0')),
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("read-only snapshot");
    assert!(!state.is_mutation_eligible());
}
