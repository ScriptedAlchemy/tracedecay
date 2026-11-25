//! One relivened completed text artifact must not stall the others.
//!
//! Retention proves liveness before collection, but a sibling publication can
//! land between the mark phase and committed cleanup. That artifact is now a
//! durable descriptor target and must be restored; the transaction's other
//! candidates are still superseded and must be removed. Refusing the whole
//! transaction left every later generation in the store uncollectable.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;

use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::{
    CodeTextArtifactRetentionTransactionV1, DurableSealedCodeGenerationIdentityV1,
    TEXT_ARTIFACT_TRANSACTION_SCHEMA, attach_verified_text_artifact_under_lock,
    build_text_artifact_receipt, code_text_artifacts_root, execute_code_generation_retention,
    fixture_store, plan_code_generation_retention, read_active_pointer,
    recover_pending_text_artifact_transaction_unlocked, stage_collectable_text_artifacts,
    text_artifact_for_bytes, text_artifact_transaction_path, write_text_artifact,
};
use super::{journal, receipt_store};
use crate::code_index_generations::CodeGenerationRetentionModeV1;
use crate::code_index_generations::text_artifacts::{
    TEXT_ARTIFACT_RECEIPT_STORE, TEXT_ARTIFACT_TRANSACTION_JOURNAL,
};

#[test]
fn committed_receipt_with_relivened_artifact_restores_it_and_finishes_recovery() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let candidates = [
        text_artifact_for_bytes(&active.id, b"superseded one"),
        text_artifact_for_bytes(&active.id, b"relivened two"),
        text_artifact_for_bytes(&active.id, b"superseded three"),
    ];
    let mut candidate_paths = Vec::new();
    for (descriptor, bytes) in candidates.iter().zip([
        b"superseded one".as_slice(),
        b"relivened two".as_slice(),
        b"superseded three".as_slice(),
    ]) {
        candidate_paths.push(write_text_artifact(&store, descriptor, bytes));
    }

    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new())
        .expect("plan three superseded text artifacts");
    assert_eq!(plan.collectable_text_artifacts.len(), 3);
    let receipt = build_text_artifact_receipt(
        &plan,
        plan.active_pointer.as_ref(),
        plan.collectable_text_artifacts.clone(),
        UtcMicros(21),
    )
    .expect("build artifact receipt");
    let transaction = CodeTextArtifactRetentionTransactionV1 {
        schema: TEXT_ARTIFACT_TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };
    journal::persist_journal(
        store.path(),
        &TEXT_ARTIFACT_TRANSACTION_JOURNAL,
        &transaction,
    )
    .expect("journal artifact retention");
    stage_collectable_text_artifacts(store.path(), &transaction)
        .expect("quarantine the three candidates");
    receipt_store::write_receipt(
        store.path(),
        &TEXT_ARTIFACT_RECEIPT_STORE,
        &receipt.receipt_digest,
        &receipt,
    )
    .expect("durably commit the artifact receipt");
    for path in &candidate_paths {
        assert!(
            !path.exists(),
            "staged candidate must not be in the source directory"
        );
    }

    // A sibling publish relivens candidate two after the receipt is durable.
    // Its descriptor now names a durable target in this store's pointer.
    let expected = read_active_pointer(store.path()).expect("current pointer");
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: active.file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("sealed digest"),
        size_bytes: active.size_bytes,
    };
    let lock =
        super::acquire_code_generation_store_lock(store.path()).expect("generation store lock");
    attach_verified_text_artifact_under_lock(
        &lock,
        &expected,
        &sealed_identity,
        candidates[1].clone(),
    )
    .expect("reliven the second candidate");
    drop(lock);

    let cleanup = recover_pending_text_artifact_transaction_unlocked(store.path())
        .expect("one relivened artifact must not refuse recovery");
    assert_eq!(
        cleanup.restored_live,
        vec![candidates[1].artifact_file.clone()]
    );
    assert_eq!(cleanup.removed.len(), 2);
    assert_eq!(
        std::fs::read(&candidate_paths[1]).expect("relivened artifact restored"),
        b"relivened two"
    );
    assert!(!candidate_paths[0].exists());
    assert!(!candidate_paths[2].exists());
    assert!(!text_artifact_transaction_path(store.path()).exists());

    // The next retention still collects what is superseded: recovery did not
    // leave the store unable to collect later generations.
    let later = [
        text_artifact_for_bytes(&active.id, b"later one"),
        text_artifact_for_bytes(&active.id, b"later two"),
    ];
    let mut later_paths = Vec::new();
    for (descriptor, bytes) in later
        .iter()
        .zip([b"later one".as_slice(), b"later two".as_slice()])
    {
        later_paths.push(write_text_artifact(&store, descriptor, bytes));
    }
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new())
        .expect("plan the later superseded artifacts");
    assert_eq!(
        plan.collectable_text_artifacts.len(),
        2,
        "the relivened artifact must stay marked live while later artifacts collect"
    );
    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(22),
        None,
    )
    .expect("collect the later superseded artifacts");
    assert_eq!(report.deleted_text_artifacts.len(), 2);
    assert!(report.restored_live_text_artifacts.is_empty());
    for path in &later_paths {
        assert!(!path.exists());
    }
    assert!(
        candidate_paths[1].is_file(),
        "the relivened artifact must keep serving across later collection"
    );
    assert!(code_text_artifacts_root(store.path()).is_dir());
}

/// Cleanup restores a relivened artifact with its own durable rename but
/// clears the journal only after every candidate. If it is interrupted in
/// between and the artifact's descriptor is then dropped, the rerun sees an
/// unmarked artifact back in its source. That is the earlier restore, not an
/// anomaly: refusing it would keep the journal forever and block every later
/// retention pass for the store.
#[test]
fn rerun_after_interrupted_restore_keeps_the_restored_artifact_and_clears_the_journal() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let contents = [
        b"superseded one".as_slice(),
        b"restored two".as_slice(),
        b"superseded three".as_slice(),
    ];
    let candidates = contents
        .map(|bytes| text_artifact_for_bytes(&active.id, bytes))
        .to_vec();
    let candidate_paths = candidates
        .iter()
        .zip(contents)
        .map(|(descriptor, bytes)| write_text_artifact(&store, descriptor, bytes))
        .collect::<Vec<_>>();

    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new())
        .expect("plan three superseded text artifacts");
    assert_eq!(plan.collectable_text_artifacts.len(), 3);
    let receipt = build_text_artifact_receipt(
        &plan,
        plan.active_pointer.as_ref(),
        plan.collectable_text_artifacts.clone(),
        UtcMicros(31),
    )
    .expect("build artifact receipt");
    let transaction = CodeTextArtifactRetentionTransactionV1 {
        schema: TEXT_ARTIFACT_TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };
    journal::persist_journal(
        store.path(),
        &TEXT_ARTIFACT_TRANSACTION_JOURNAL,
        &transaction,
    )
    .expect("journal artifact retention");
    stage_collectable_text_artifacts(store.path(), &transaction)
        .expect("quarantine the three candidates");
    receipt_store::write_receipt(
        store.path(),
        &TEXT_ARTIFACT_RECEIPT_STORE,
        &receipt.receipt_digest,
        &receipt,
    )
    .expect("durably commit the artifact receipt");

    // An interrupted cleanup already restored candidate two (its rename is
    // durable) before the journal was cleared; its descriptor is gone again.
    let staged = store
        .path()
        .join(crate::code_index_generations::TEXT_ARTIFACT_QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
        .join(&candidates[1].artifact_file);
    assert!(
        staged.is_file(),
        "candidate two sits in the quarantine stage"
    );
    std::fs::rename(&staged, &candidate_paths[1]).expect("simulate the earlier restore");

    let cleanup = recover_pending_text_artifact_transaction_unlocked(store.path())
        .expect("an earlier restore must not refuse recovery");
    assert_eq!(
        cleanup.restored_live,
        vec![candidates[1].artifact_file.clone()]
    );
    assert_eq!(cleanup.removed.len(), 2);
    assert_eq!(
        std::fs::read(&candidate_paths[1]).expect("restored artifact stays in place"),
        b"restored two"
    );
    assert!(!candidate_paths[0].exists());
    assert!(!candidate_paths[2].exists());
    assert!(!text_artifact_transaction_path(store.path()).exists());

    // The restored artifact is ordinary again: the next pass collects it.
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new())
        .expect("plan after recovery");
    assert_eq!(plan.collectable_text_artifacts.len(), 1);
    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(32),
        None,
    )
    .expect("collect the formerly restored artifact");
    assert_eq!(report.deleted_text_artifacts.len(), 1);
    assert!(!candidate_paths[1].exists());
}
