use crate::{
    GitOidV1, ManifestDigest, NativeIntegrationJournalPhaseV1, NativeIntegrationJournalV1,
    NativeIntegrationMechanicalModeV1, NativeIntegrationPreviewId, NativeIntegrationReceiptId,
    NativeIntegrationReceiptOutcomeV1, NativeIntegrationReceiptV1, NativeIntegrationTransactionId,
    RepositoryId, UtcMicros, WorktreeId,
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
