use tracedecay_domain::git::repository_state::{
    RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
};
use tracedecay_domain::{
    GitBlobExpectationV1, GitCommitIdentityV1, GitCoverageV1, GitFileModeV1, GitHeadStateV1,
    GitIndexCommitIntentV1, GitIndexEntryExpectationV1, GitIndexJournalPhaseV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, HunkDirectionV1, HunkRefV1, ManifestDigest, ProjectId, RepositoryId,
    UtcMicros, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture oid is canonical")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
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

fn commit_intent(message: &str) -> GitIndexCommitIntentV1 {
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: UtcMicros(1_000_000),
    };
    GitIndexCommitIntentV1::new(
        message.to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .expect("commit intent")
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
    assert!(
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
        .is_err(),
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
        None,
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
fn commit_preview_digest_binds_full_canonical_intent() {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let make_preview = |intent| {
        GitIndexPreviewV1::new_with_commit_intent(
            GitIndexPreviewId::new("git-preview.commit-intent.fixture").expect("preview id"),
            GitIndexTransactionOperationV1::CommitIndex,
            snapshot.clone(),
            snapshot_digest.clone(),
            Vec::new(),
            snapshot.index.tree_id.clone(),
            Some(intent),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(10),
            UtcMicros(20),
        )
        .expect("commit preview")
    };
    let first = make_preview(commit_intent("first message\n"));
    let second = make_preview(commit_intent("second message\n"));
    assert_ne!(first.preview_digest, second.preview_digest);
    assert_ne!(first, second);

    let encoded = serde_json::to_value(&first).expect("serialize preview");
    let mut tampered = encoded;
    tampered["commit_intent"]["author"]["name"] = serde_json::json!("Other Author");
    assert!(serde_json::from_value::<GitIndexPreviewV1>(tampered).is_err());
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
