use crate::{
    GitOidV1, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationApprovalV1,
    NativeIntegrationCapabilityV1, NativeIntegrationDelegatedAgentId,
    NativeIntegrationJournalPhaseV1, NativeIntegrationJournalV1, NativeIntegrationMechanicalModeV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationPrincipalId, NativeIntegrationReceiptId, NativeIntegrationReceiptOutcomeV1,
    NativeIntegrationReceiptV1, NativeIntegrationTransactionId, RepositoryId, UtcMicros,
    WorktreeId,
};

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn oid(fill: char) -> GitOidV1 {
    GitOidV1::new(fill.to_string().repeat(40)).expect("oid")
}

fn prepared() -> NativeIntegrationJournalV1 {
    NativeIntegrationJournalV1::prepared(
        NativeIntegrationTransactionId::new("native-integration.transaction.1")
            .expect("transaction"),
        NativeIntegrationPreviewId::new("native-integration.preview.1").expect("preview"),
        digest('a'),
        RepositoryId::new("repository.1").expect("repository"),
        WorktreeId::new("worktree.source").expect("source"),
        WorktreeId::new("worktree.destination").expect("destination"),
        NativeIntegrationMechanicalModeV1::TwoParentMerge,
        oid('1'),
        oid('2'),
        oid('5'),
        oid('3'),
        digest('b'),
        oid('4'),
        UtcMicros(10),
    )
    .expect("prepared journal")
}

fn preview() -> NativeIntegrationPreviewV1 {
    let mut preview = NativeIntegrationPreviewV1 {
        preview_id: NativeIntegrationPreviewId::new("native-integration.preview.1")
            .expect("preview"),
        repository_id: RepositoryId::new("repository.1").expect("repository"),
        source_worktree_id: WorktreeId::new("worktree.source").expect("source"),
        destination_worktree_id: WorktreeId::new("worktree.destination").expect("destination"),
        source_ref: "refs/heads/source".to_owned(),
        destination_ref: "refs/heads/destination".to_owned(),
        destination_checked_out: false,
        mode: NativeIntegrationMechanicalModeV1::TwoParentMerge,
        source_tip: oid('1'),
        destination_tip: oid('2'),
        destination_tree: oid('5'),
        merge_base: oid('6'),
        ordered_source_commits: vec![oid('1')],
        expected_created_commits: vec![oid('3')],
        candidate_destination_tip: oid('3'),
        candidate_tree: oid('4'),
        repository_snapshot_digest: digest('b'),
        selection_digest: digest('c'),
        topology_digest: digest('d'),
        configuration_digest: digest('e'),
        attributes_digest: digest('f'),
        hook_policy_digest: digest('7'),
        signing_policy_digest: digest('8'),
        message_policy_digest: digest('9'),
        semantic_evidence_digest: digest('0'),
        disposition: NativeIntegrationPreviewDispositionV1::Eligible,
        created_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        preview_digest: digest('a'),
    };
    preview.preview_digest = preview.compute_preview_digest().expect("preview digest");
    preview.validate().expect("preview");
    preview
}

#[test]
fn cancellation_before_ref_commit_is_durable_and_can_abort_unchanged() {
    let mut journal = prepared();
    journal.request_cancellation(UtcMicros(11)).expect("cancel");
    assert_eq!(journal.revision, 2);
    assert_eq!(journal.cancellation_requested_at, Some(UtcMicros(11)));
    assert!(journal.should_abort_before_commit());

    journal
        .advance(
            NativeIntegrationJournalPhaseV1::AbortedNoChange,
            UtcMicros(12),
        )
        .expect("abort without mutation");
    assert!(journal.phase.is_terminal());
    assert!(!journal.should_abort_before_commit());
}

#[test]
fn cancellation_after_ref_commit_never_claims_cancelled() {
    let mut journal = prepared();
    for (phase, at) in [
        (NativeIntegrationJournalPhaseV1::NativeApplyStarted, 11),
        (NativeIntegrationJournalPhaseV1::ObjectsWritten, 12),
        (NativeIntegrationJournalPhaseV1::RefCommitted, 13),
    ] {
        journal.advance(phase, UtcMicros(at)).expect("advance");
    }
    journal.request_cancellation(UtcMicros(14)).expect("cancel");
    assert!(!journal.should_abort_before_commit());
    assert!(journal.commit_point_crossed());
    assert_eq!(journal.phase, NativeIntegrationJournalPhaseV1::RefCommitted);
}

#[test]
fn journal_rejects_skipped_commit_boundaries_and_false_rollback() {
    let mut journal = prepared();
    assert!(
        journal
            .advance(NativeIntegrationJournalPhaseV1::RefCommitted, UtcMicros(11))
            .is_err()
    );
    journal
        .advance(
            NativeIntegrationJournalPhaseV1::NativeApplyStarted,
            UtcMicros(11),
        )
        .expect("start");
    assert!(
        journal
            .advance(NativeIntegrationJournalPhaseV1::RolledBack, UtcMicros(12))
            .is_err(),
        "rollback is truthful only after a durable boundary"
    );
}

#[test]
fn checked_out_destination_requires_materialization_before_ref_commit() {
    let mut journal = prepared();
    journal
        .mark_destination_checked_out()
        .expect("checked-out destination");
    journal
        .advance(
            NativeIntegrationJournalPhaseV1::NativeApplyStarted,
            UtcMicros(11),
        )
        .expect("start");
    journal
        .advance(
            NativeIntegrationJournalPhaseV1::ObjectsWritten,
            UtcMicros(12),
        )
        .expect("objects");
    assert!(
        journal
            .advance(NativeIntegrationJournalPhaseV1::RefCommitted, UtcMicros(13))
            .is_err()
    );
    journal
        .advance(
            NativeIntegrationJournalPhaseV1::DestinationMaterialized,
            UtcMicros(13),
        )
        .expect("materialized");
    journal
        .advance(NativeIntegrationJournalPhaseV1::RefCommitted, UtcMicros(14))
        .expect("ref commit");
}

#[test]
fn committed_receipt_requires_exact_final_ref_tree_and_created_commit() {
    let mut journal = prepared();
    for (phase, at) in [
        (NativeIntegrationJournalPhaseV1::NativeApplyStarted, 11),
        (NativeIntegrationJournalPhaseV1::ObjectsWritten, 12),
        (NativeIntegrationJournalPhaseV1::RefCommitted, 13),
        (NativeIntegrationJournalPhaseV1::Verifying, 14),
        (NativeIntegrationJournalPhaseV1::Committed, 15),
    ] {
        journal.advance(phase, UtcMicros(at)).expect("advance");
    }

    NativeIntegrationReceiptV1::new(
        NativeIntegrationReceiptId::new("native-integration.receipt.1").expect("receipt"),
        &journal,
        Some(digest('c')),
        Some(journal.expected_new_destination_tip.clone()),
        Some(journal.candidate_tree.clone()),
        vec![journal.expected_new_destination_tip.clone()],
        NativeIntegrationReceiptOutcomeV1::Committed,
        UtcMicros(15),
    )
    .expect("exact committed receipt");

    assert!(
        NativeIntegrationReceiptV1::new(
            NativeIntegrationReceiptId::new("native-integration.receipt.2").expect("receipt"),
            &journal,
            Some(digest('c')),
            Some(journal.expected_destination_tip.clone()),
            Some(journal.candidate_tree.clone()),
            vec![journal.expected_new_destination_tip.clone()],
            NativeIntegrationReceiptOutcomeV1::Committed,
            UtcMicros(15),
        )
        .is_err()
    );
}

#[test]
fn rollback_receipt_proves_exact_old_ref_tree_and_snapshot() {
    let mut journal = prepared();
    for (phase, at) in [
        (NativeIntegrationJournalPhaseV1::NativeApplyStarted, 11),
        (NativeIntegrationJournalPhaseV1::ObjectsWritten, 12),
        (NativeIntegrationJournalPhaseV1::RefCommitted, 13),
        (NativeIntegrationJournalPhaseV1::RolledBack, 14),
    ] {
        journal.advance(phase, UtcMicros(at)).expect("advance");
    }

    NativeIntegrationReceiptV1::new(
        NativeIntegrationReceiptId::new("native-integration.receipt.3").expect("receipt"),
        &journal,
        Some(journal.expected_repository_snapshot_digest.clone()),
        Some(journal.expected_destination_tip.clone()),
        Some(journal.expected_destination_tree.clone()),
        vec![journal.expected_new_destination_tip.clone()],
        NativeIntegrationReceiptOutcomeV1::RolledBack,
        UtcMicros(14),
    )
    .expect("exact rollback receipt");

    assert!(
        NativeIntegrationReceiptV1::new(
            NativeIntegrationReceiptId::new("native-integration.receipt.4").expect("receipt"),
            &journal,
            Some(digest('d')),
            Some(journal.expected_destination_tip.clone()),
            Some(journal.expected_destination_tree.clone()),
            vec![journal.expected_new_destination_tip.clone()],
            NativeIntegrationReceiptOutcomeV1::RolledBack,
            UtcMicros(14),
        )
        .is_err()
    );
}

#[test]
fn immutable_preview_binds_ordered_commits_refs_and_all_drift_evidence() {
    let preview = preview();
    NativeIntegrationJournalV1::prepared_from_preview(
        NativeIntegrationTransactionId::new("native-integration.transaction.preview")
            .expect("transaction"),
        &preview,
        UtcMicros(10),
    )
    .expect("preview-bound journal");

    let mut tampered = preview.clone();
    tampered.ordered_source_commits.push(oid('7'));
    assert!(tampered.validate().is_err());

    let mut unqualified_ref = preview;
    unqualified_ref.source_ref = "source".to_owned();
    assert!(unqualified_ref.validate().is_err());
}

#[test]
fn approval_is_exact_one_use_capability_material_for_one_preview() {
    let preview = preview();
    let approval = NativeIntegrationApprovalV1 {
        approval_id: NativeIntegrationApprovalId::new("native-integration.approval.1")
            .expect("approval"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        repository_id: preview.repository_id.clone(),
        source_worktree_id: preview.source_worktree_id.clone(),
        destination_worktree_id: preview.destination_worktree_id.clone(),
        mode: preview.mode,
        selection_digest: preview.selection_digest.clone(),
        scope_digest: digest('a'),
        analysis_digest: digest('b'),
        principal_id: NativeIntegrationPrincipalId::new("principal.operator").expect("principal"),
        delegated_agent_id: Some(
            NativeIntegrationDelegatedAgentId::new("agent.codex").expect("agent"),
        ),
        capability: NativeIntegrationCapabilityV1::NativeIntegrationApply,
        issued_at: UtcMicros(2),
        expires_at: UtcMicros(90),
        approval_digest: digest('c'),
    }
    .seal()
    .expect("sealed approval");

    approval
        .validate_against(&preview, UtcMicros(50))
        .expect("exact approval");

    let mut changed_selection = approval.clone();
    changed_selection.selection_digest = digest('d');
    assert!(
        changed_selection
            .validate_against(&preview, UtcMicros(50))
            .is_err()
    );
    assert!(approval.validate_against(&preview, UtcMicros(91)).is_err());
}
