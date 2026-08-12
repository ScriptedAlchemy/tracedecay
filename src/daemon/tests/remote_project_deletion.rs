#![cfg(unix)]

use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;

use super::bootstrap::{enroll_project_on_disk_only, run_git};
use super::{enter_test_daemon_database_scope, test_daemon_engine_for_profile};
use crate::daemon::remote_deletion::{
    RemoteDeletionFailure, RemoteDeletionFailureCode, RemoteDeletionPhase,
    RemoteDeletionReceiptTarget, RemoteDeletionRuntimeOwners, RemoteDeletionStatus,
};
use crate::errors::TraceDecayError;

#[tokio::test]
async fn remote_project_deletion_removes_only_its_profile_shard_and_fences_replay() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let project = root.join("repository");
    let unrelated_project = root.join("unrelated-repository");
    std::fs::create_dir_all(&project).expect("create repository");
    std::fs::create_dir_all(&unrelated_project).expect("create unrelated repository");
    run_git(&project, &["init", "--quiet"]);
    run_git(&unrelated_project, &["init", "--quiet"]);
    let layout = enroll_project_on_disk_only(&project, &profile_root, "proj_remote_deleted");
    let unrelated_layout =
        enroll_project_on_disk_only(&unrelated_project, &profile_root, "proj_remote_live");
    // This fixture is intentionally a durable non-SQLite payload: deletion
    // must remove the exact profile shard without attempting any operator
    // profile path, while the retained marker must fence replay afterwards.
    std::fs::remove_file(&layout.graph_db_path).expect("remove synthetic graph file");
    std::fs::remove_file(&unrelated_layout.graph_db_path)
        .expect("remove unrelated synthetic graph file");
    std::fs::write(layout.data_root.join("payload.txt"), "remote payload")
        .expect("write isolated profile payload");
    std::fs::write(
        unrelated_layout.data_root.join("payload.txt"),
        "unrelated payload",
    )
    .expect("write unrelated profile payload");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "remote project deletion");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let deletion_owners = RemoteDeletionRuntimeOwners {
        administration: engine.store_administration.clone(),
        invocation: engine.invocation.clone(),
        project_open_gates: Arc::clone(&engine.project_open_gates),
    };
    let receipt = engine
        .store_administration
        .execute_remote_deletion(
            &deletion_owners,
            RemoteDeletionReceiptTarget::Project,
            Some("proj_remote_deleted".to_owned()),
            "tombstone.remote-deleted".to_owned(),
        )
        .await
        .expect("delete isolated remote project");
    assert_eq!(
        receipt.removed_project_ids,
        ["proj_remote_deleted".to_owned()]
    );
    assert!(receipt.tombstone_recorded);
    assert!(receipt.pending_project_ids.is_empty());
    assert!(!layout.data_root.exists(), "exact profile shard is removed");
    assert!(project.exists(), "source checkout is never removed");
    assert!(unrelated_layout.data_root.exists());
    let recovery_lifecycle = engine
        .store_administration
        .remote_recovery_project_lifecycle()
        .expect("recovery lifecycle lookup")
        .expect("installed recovery lifecycle");
    let recovery_error = match recovery_lifecycle
        .authorize_project_recovery(
            &ProjectId::new("proj_remote_deleted").expect("typed deleted project"),
        )
        .await
    {
        Ok(_) => panic!("the durable tombstone must fence remote recovery"),
        Err(error) => error,
    };
    assert!(matches!(
        recovery_error,
        TraceDecayError::ProjectRoute { ref reason_code, retryable: false, .. }
            if reason_code == "remote_deleted"
    ));

    let replayed = engine
        .store_administration
        .execute_remote_deletion(
            &deletion_owners,
            RemoteDeletionReceiptTarget::Project,
            Some("proj_remote_deleted".to_owned()),
            "tombstone.remote-deleted".to_owned(),
        )
        .await
        .expect("the same tombstone remains idempotent after exact shard removal");
    assert_eq!(replayed.status, RemoteDeletionStatus::Deleted);
    assert!(replayed.pending_project_ids.is_empty());
    assert!(!layout.data_root.exists());
    assert!(project.exists(), "idempotent replay preserves the checkout");
    engine
        .ensure_registered_project_route(&unrelated_project, false)
        .await
        .expect("unrelated project remains routable after repeated tombstone");
    assert_eq!(
        std::fs::read_to_string(unrelated_layout.data_root.join("payload.txt"))
            .expect("read unrelated payload"),
        "unrelated payload"
    );

    let conflict = engine
        .store_administration
        .execute_remote_deletion(
            &deletion_owners,
            RemoteDeletionReceiptTarget::Project,
            Some("proj_remote_deleted".to_owned()),
            "tombstone.replacement".to_owned(),
        )
        .await
        .expect_err("a different tombstone id must not replace the durable deletion fact");
    assert_eq!(conflict.receipt.status, RemoteDeletionStatus::Failed);
    assert!(!conflict.receipt.tombstone_recorded);
    assert_eq!(
        conflict.receipt.failure,
        Some(RemoteDeletionFailure {
            code: RemoteDeletionFailureCode::TombstoneConflict,
            phase: RemoteDeletionPhase::PersistTombstone,
            retryable: false,
        })
    );

    let error = engine
        .ensure_registered_project_route(&project, false)
        .await
        .expect_err("retained enrollment marker must be fenced by the tombstone");
    assert!(matches!(
        error,
        TraceDecayError::ProjectRoute { ref reason_code, retryable: false, .. }
            if reason_code == "remote_deleted"
    ));
}
