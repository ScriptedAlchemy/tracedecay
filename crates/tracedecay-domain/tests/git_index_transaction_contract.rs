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
    assert!(preview.commit_intent_digest.is_none());
    assert!(
        GitIndexPreviewV1::new_with_commit_intent(
            preview_id.clone(),
            GitIndexTransactionOperationV1::StageHunks,
            snapshot.clone(),
            snapshot_digest.clone(),
            vec![reference.clone()],
            Some(oid('e')),
            Some(&commit_intent("must not bind to stage\n")),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(10),
            UtcMicros(20),
        )
        .is_err(),
        "stage previews must reject commit-intent commitments"
    );

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

    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let intent = commit_intent("phase evidence\n");
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        GitIndexPreviewId::new("git-preview.phase-evidence").expect("preview id"),
        GitIndexTransactionOperationV1::CommitIndex,
        snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        snapshot.index.tree_id.clone(),
        Some(&intent),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("commit preview");
    let mut forged = tracedecay_domain::GitIndexTransactionJournalV1::prepared(
        GitIndexTransactionId::new("git-index-transaction.forged-phase").expect("transaction id"),
        &preview,
        UtcMicros(10),
    )
    .expect("prepared journal");
    forged.phase = GitIndexJournalPhaseV1::RefCommitted;
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
        assert!(phase.permits_recovered_outcome(
            GitIndexTransactionOperationV1::StageHunks,
            GitIndexReceiptOutcomeV1::AbortedNoChange,
        ));
        assert!(phase.permits_recovered_outcome(
            GitIndexTransactionOperationV1::StageHunks,
            GitIndexReceiptOutcomeV1::NeedsInspection,
        ));
        assert!(
            !phase.permits_recovered_outcome(
                GitIndexTransactionOperationV1::StageHunks,
                GitIndexReceiptOutcomeV1::Committed,
            ),
            "a candidate tree observed before a durable index phase is coincidence, not proof"
        );
    }

    assert!(
        GitIndexJournalPhaseV1::IndexCommitted.permits_recovered_outcome(
            GitIndexTransactionOperationV1::StageHunks,
            GitIndexReceiptOutcomeV1::Committed,
        )
    );
    assert!(
        !GitIndexJournalPhaseV1::IndexCommitted.permits_recovered_outcome(
            GitIndexTransactionOperationV1::CommitIndex,
            GitIndexReceiptOutcomeV1::Committed,
        ),
        "a commit recovery needs durable ref-boundary evidence"
    );
    assert!(
        GitIndexJournalPhaseV1::RefCommitted.permits_recovered_outcome(
            GitIndexTransactionOperationV1::CommitIndex,
            GitIndexReceiptOutcomeV1::Committed,
        )
    );
    assert!(
        !GitIndexJournalPhaseV1::NeedsInspection.permits_recovered_outcome(
            GitIndexTransactionOperationV1::CommitIndex,
            GitIndexReceiptOutcomeV1::Committed,
        ),
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
        None,
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
            None,
            GitIndexReceiptOutcomeV1::Committed,
            UtcMicros(11),
        )
        .is_err(),
        "a committed receipt must contain a captured final snapshot"
    );
}

#[test]
fn commit_preview_persists_only_a_digest_bound_to_full_canonical_intent() {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let make_preview = |intent: &GitIndexCommitIntentV1| {
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
    assert!(
        GitIndexPreviewV1::new(
            GitIndexPreviewId::new("git-preview.commit-without-intent").expect("preview id"),
            GitIndexTransactionOperationV1::CommitIndex,
            snapshot.clone(),
            snapshot_digest.clone(),
            Vec::new(),
            snapshot.index.tree_id.clone(),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(10),
            UtcMicros(20),
        )
        .is_err(),
        "applicable commit previews must bind one intent commitment"
    );
    let mut sensitive_intent = commit_intent("first sensitive message\n");
    sensitive_intent.author.name = "Sensitive Author".to_owned();
    sensitive_intent.author.email = "sensitive-author@example.com".to_owned();
    sensitive_intent.committer.name = "Sensitive Committer".to_owned();
    sensitive_intent.committer.email = "sensitive-committer@example.com".to_owned();
    sensitive_intent.signing_policy = GitIndexSigningPolicyV1::SignatureRequired {
        key_reference: "sensitive-signing-key".to_owned(),
    };
    sensitive_intent
        .validate()
        .expect("sensitive intent is valid");
    let expected_intent_digest = sensitive_intent.compute_digest().expect("intent digest");
    let first = make_preview(&sensitive_intent);
    let second_intent = commit_intent("second message\n");
    let second = make_preview(&second_intent);
    assert_ne!(first.preview_digest, second.preview_digest);
    assert_ne!(first, second);
    assert_eq!(
        first.commit_intent_digest.as_ref(),
        Some(&expected_intent_digest)
    );

    let base = commit_intent("canonical intent\n");
    let base_digest = base.compute_digest().expect("base intent digest");
    let mut changed_author = base.clone();
    changed_author.author.at = UtcMicros(2_000_000);
    let mut changed_committer = base.clone();
    changed_committer.committer.email = "other-committer@example.com".to_owned();
    let mut changed_signing = base;
    changed_signing.signing_policy = GitIndexSigningPolicyV1::SignatureRequired {
        key_reference: "other-signing-key".to_owned(),
    };
    for changed in [changed_author, changed_committer, changed_signing] {
        assert_ne!(
            changed.compute_digest().expect("changed intent digest"),
            base_digest,
            "every executable commit-intent field must affect the commitment"
        );
    }

    let encoded = serde_json::to_string(&first).expect("serialize preview");
    for sensitive in [
        "first sensitive message",
        "Sensitive Author",
        "sensitive-author@example.com",
        "Sensitive Committer",
        "sensitive-committer@example.com",
        "sensitive-signing-key",
    ] {
        assert!(
            !encoded.contains(sensitive),
            "serialized preview leaked {sensitive:?}"
        );
    }
    let decoded: GitIndexPreviewV1 =
        serde_json::from_str(&encoded).expect("digest-only preview round trip");
    assert_eq!(decoded, first);

    let mut missing_digest: serde_json::Value =
        serde_json::from_str(&encoded).expect("preview JSON");
    missing_digest
        .as_object_mut()
        .expect("preview object")
        .remove("commit_intent_digest");
    assert!(serde_json::from_value::<GitIndexPreviewV1>(missing_digest).is_err());

    let mut plaintext_legacy: serde_json::Value =
        serde_json::from_str(&encoded).expect("preview JSON");
    plaintext_legacy["commit_intent"] =
        serde_json::to_value(commit_intent("must not deserialize\n")).expect("legacy intent");
    assert!(serde_json::from_value::<GitIndexPreviewV1>(plaintext_legacy).is_err());

    let mut tampered: serde_json::Value = serde_json::from_str(&encoded).expect("preview JSON");
    assert!(tampered.get("commit_intent").is_none());
    tampered["commit_intent_digest"] = serde_json::json!(digest('9'));
    assert!(serde_json::from_value::<GitIndexPreviewV1>(tampered).is_err());
}

#[test]
fn commit_intent_digest_uses_git_second_precision_without_changing_wire_values() {
    let make_intent = |author_at: i64, committer_at: i64| {
        GitIndexCommitIntentV1::new(
            "canonical timestamp intent\n".to_owned(),
            GitCommitIdentityV1 {
                name: "TraceDecay Author".to_owned(),
                email: "author@example.com".to_owned(),
                at: UtcMicros(author_at),
            },
            GitCommitIdentityV1 {
                name: "TraceDecay Committer".to_owned(),
                email: "committer@example.com".to_owned(),
                at: UtcMicros(committer_at),
            },
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .expect("commit intent")
    };

    let unaligned = make_intent(1_234_567, 2_999_999);
    let aligned = make_intent(1_000_000, 2_000_000);
    assert_eq!(unaligned.author.at, UtcMicros(1_234_567));
    assert_eq!(unaligned.committer.at, UtcMicros(2_999_999));
    assert_eq!(
        unaligned.compute_digest().expect("unaligned digest"),
        aligned.compute_digest().expect("aligned digest")
    );

    // Whole-second V1 intents retain their historical digest. Inputs with
    // subsecond timestamps were already unrecoverable because Git persisted
    // only whole seconds, so the V1 domain remains the maximal compatibility
    // surface while newly created intents reconcile correctly.
    assert_eq!(
        aligned.compute_digest().expect("legacy aligned digest"),
        digest("sha256:3fcfb47cf5fe4965337c4dfe33b23a84d11394c072e9491e85219bcc950f5b33")
    );
    assert_eq!(
        make_intent(i64::MIN, i64::MAX).compute_digest(),
        Err(tracedecay_domain::research::DomainError::NonCanonical {
            field: "git commit identity timestamp",
        }),
        "the lower Git second cannot be represented in domain microseconds"
    );
    let lowest_exact_seconds = i64::MIN / 1_000_000;
    let lowest_exact_micros = lowest_exact_seconds
        .checked_mul(1_000_000)
        .expect("lowest whole second remains representable");
    assert!(
        make_intent(lowest_exact_micros, lowest_exact_micros)
            .compute_digest()
            .is_ok()
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
