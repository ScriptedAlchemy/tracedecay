use super::*;

fn run_git(project_root: &Path, args: &[&str]) {
    let output = std::process::Command::new(crate::git::git_program())
        .args(args)
        .current_dir(project_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let project_root = temp.path().join("repo");
    let tracedecay_dir = temp.path().join("store");
    std::fs::create_dir_all(&project_root).unwrap();
    run_git(&project_root, &["init", "-b", "main"]);
    run_git(&project_root, &["config", "user.email", "test@example.com"]);
    run_git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    std::fs::write(project_root.join("fixture"), b"fixture").unwrap();
    run_git(&project_root, &["add", "fixture"]);
    run_git(&project_root, &["commit", "-m", "fixture"]);
    std::fs::create_dir_all(tracedecay_dir.join("branches")).unwrap();
    std::fs::write(tracedecay_dir.join(crate::config::DB_FILENAME), b"main").unwrap();
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
    std::fs::write(tracedecay_dir.join("branches/feature.db"), b"feature").unwrap();
    (temp, project_root, tracedecay_dir)
}

#[test]
fn branch_admin_selection_does_not_mutate_before_commit() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        14,
        7,
    )
    .unwrap();
    assert_eq!(
        prepared.database_paths(),
        &[tracedecay_dir.join("branches/feature.db")]
    );
    assert!(tracedecay_dir.join("branches/feature.db").exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );

    let report = prepared.commit().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(!tracedecay_dir.join("branches/feature.db").exists());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn nonempty_metadata_only_finish_fails_closed_without_deleting() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared.finish_without_database_deletion().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires daemon store administration")
    );
    assert!(db.exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn compatibility_remove_fails_closed_without_deleting() {
    let (_temp, _project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");

    let error = remove_tracked_branch_store_checked(&tracedecay_dir, "feature").unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires daemon store administration")
    );
    assert!(db.exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn branch_admin_never_selects_default_branch_for_removal() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let error = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "main".to_string(),
        },
        14,
        7,
    )
    .err()
    .expect("default branch removal must fail closed");
    assert!(error.to_string().contains("cannot remove default branch"));
    assert!(tracedecay_dir.join(crate::config::DB_FILENAME).exists());
}

#[test]
fn branch_admin_refuses_corrupt_metadata_without_selecting_stores() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    std::fs::write(
        tracedecay_dir.join(crate::storage::BRANCH_META_FILENAME),
        b"{not-json",
    )
    .unwrap();

    let error =
        prepare_branch_admin_mutation(&project_root, &tracedecay_dir, BranchAdminAction::Gc, 0, 0)
            .err()
            .expect("corrupt branch metadata must fail closed");

    assert!(error.to_string().contains("corrupt or unreadable metadata"));
    assert!(tracedecay_dir.join("branches/feature.db").exists());
}

fn failpoint(message: &str) -> crate::errors::Result<()> {
    Err(crate::errors::TraceDecayError::Config {
        message: message.to_string(),
    })
}

fn quarantine_files(tracedecay_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(tracedecay_dir.join("branches"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".branch-delete-"))
        })
        .collect()
}

fn recover_without_fence(tracedecay_dir: &Path) {
    let recovery = prepare_pending_branch_admin_recovery(tracedecay_dir)
        .unwrap()
        .expect("pending branch deletion recovery");
    recovery.recover(|_| Ok(()), |_| Ok(())).unwrap();
}

fn recover_precommit_with_fence(
    tracedecay_dir: &Path,
    transaction_id: &str,
) -> crate::db::DatabaseDeletionStates {
    let recovery = prepare_pending_branch_admin_recovery(tracedecay_dir)
        .unwrap()
        .expect("pending branch deletion recovery");
    assert_eq!(
        recovery.disposition(),
        BranchAdminRecoveryDisposition::PreCommitRollback
    );
    let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
        recovery.database_paths(),
        transaction_id,
        "recover branch deletion test",
    )
    .unwrap();
    recovery
        .recover(
            |_| Ok(()),
            |disposition| {
                assert_eq!(
                    disposition,
                    BranchAdminRecoveryDisposition::PreCommitRollback
                );
                fence.rollback_deleting()
            },
        )
        .unwrap();
    states
}

fn recover_committed_with_fence(
    tracedecay_dir: &Path,
    transaction_id: &str,
) -> crate::db::DatabaseDeletionStates {
    let recovery = prepare_pending_branch_admin_recovery(tracedecay_dir)
        .unwrap()
        .expect("pending branch deletion recovery");
    assert_eq!(
        recovery.disposition(),
        BranchAdminRecoveryDisposition::CommittedCleanup
    );
    let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
        recovery.database_paths(),
        transaction_id,
        "complete branch deletion test",
    )
    .unwrap();
    recovery
        .recover(
            |_| Ok(()),
            |disposition| {
                assert_eq!(
                    disposition,
                    BranchAdminRecoveryDisposition::CommittedCleanup
                );
                fence.promote_deleted()
            },
        )
        .unwrap();
    states
}

#[test]
fn crash_after_journal_before_deleting_publication_recovers_missing_tombstone() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence =
        crate::db::DatabaseDeletionFence::acquire(std::slice::from_ref(&db), "delete branch test")
            .unwrap();
    let transaction_id = fence.transaction_id().to_string();
    let mut primary_failed = false;

    let error = prepared
        .commit_with_precommit_hook(
            Some(&transaction_id),
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            |phase| {
                if phase
                    == transaction::TransactionPhase::AfterJournalBeforeDeletingPublication
                {
                    primary_failed = true;
                    return failpoint("crash before deleting publication");
                }
                if primary_failed
                    && phase
                        == transaction::TransactionPhase::AfterPhysicalRollbackBeforeDeletingRollback
                {
                    return failpoint("crash before tombstone rollback");
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("crash before deleting publication")
    );
    drop(fence);

    assert!(db.exists());
    assert!(!crate::db::database_path_is_tombstoned(&db).unwrap());
    assert!(
        tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
    let states = recover_precommit_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.missing(), 1);
    assert!(!crate::db::database_path_is_tombstoned(&db).unwrap());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[test]
fn crash_after_physical_rollback_recovers_same_id_deleting_tombstone() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence =
        crate::db::DatabaseDeletionFence::acquire(std::slice::from_ref(&db), "delete branch test")
            .unwrap();
    let transaction_id = fence.transaction_id().to_string();

    let error = prepared
        .commit_with_precommit_hook(
            Some(&transaction_id),
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            |phase| match phase {
                transaction::TransactionPhase::BeforeMetadataPublication => {
                    failpoint("force precommit rollback")
                }
                transaction::TransactionPhase::AfterPhysicalRollbackBeforeDeletingRollback => {
                    failpoint("crash after physical rollback")
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("crash after physical rollback"));
    drop(fence);

    assert!(db.exists());
    assert!(crate::db::database_path_is_tombstoned(&db).unwrap());
    let states = recover_precommit_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.deleting(), 1);
    assert!(!crate::db::database_path_is_tombstoned(&db).unwrap());
}

#[test]
fn crash_after_deleting_rollback_recovers_missing_tombstone_and_clears_journal() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence =
        crate::db::DatabaseDeletionFence::acquire(std::slice::from_ref(&db), "delete branch test")
            .unwrap();
    let transaction_id = fence.transaction_id().to_string();

    let error = prepared
        .commit_with_precommit_hook(
            Some(&transaction_id),
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            |phase| match phase {
                transaction::TransactionPhase::BeforeMetadataPublication => {
                    failpoint("force precommit rollback")
                }
                transaction::TransactionPhase::AfterDeletingRollbackBeforeJournalClear => {
                    failpoint("crash after deleting rollback")
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("crash after deleting rollback"));
    drop(fence);

    assert!(db.exists());
    assert!(!crate::db::database_path_is_tombstoned(&db).unwrap());
    assert!(
        tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
    let states = recover_precommit_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.missing(), 1);
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[test]
fn metadata_commit_before_deleted_promotion_recovers_as_committed() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence =
        crate::db::DatabaseDeletionFence::acquire(std::slice::from_ref(&db), "delete branch test")
            .unwrap();
    let transaction_id = fence.transaction_id().to_string();

    let error = prepared
        .commit_with_transaction(
            &transaction_id,
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            || failpoint("crash before deleted promotion"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("crash before deleted promotion"));
    drop(fence);

    assert!(!db.exists());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
    assert!(!quarantine_files(&tracedecay_dir).is_empty());
    let states = recover_committed_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.deleting(), 1);
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(crate::db::database_path_is_tombstoned(&db).unwrap());
}

#[cfg(unix)]
#[test]
fn committed_recovery_syncs_metadata_before_tombstone_transition() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let metadata_path = tracedecay_dir.join(crate::storage::BRANCH_META_FILENAME);
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence =
        crate::db::DatabaseDeletionFence::acquire(std::slice::from_ref(&db), "delete branch test")
            .unwrap();
    let transaction_id = fence.transaction_id().to_string();
    prepared
        .commit_with_transaction(
            &transaction_id,
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            || failpoint("crash before deleted promotion"),
        )
        .unwrap_err();
    drop(fence);

    let metadata = std::fs::read(&metadata_path).unwrap();
    let recovery = prepare_pending_branch_admin_recovery(&tracedecay_dir)
        .unwrap()
        .unwrap();
    std::fs::remove_file(&metadata_path).unwrap();
    let transitioned = std::cell::Cell::new(false);
    let error = recovery
        .recover(
            |_| Ok(()),
            |_| {
                transitioned.set(true);
                Ok(())
            },
        )
        .unwrap_err();

    assert!(error.to_string().contains("failed to sync"));
    assert!(!transitioned.get());
    assert!(!quarantine_files(&tracedecay_dir).is_empty());
    std::fs::write(metadata_path, metadata).unwrap();
    recover_committed_with_fence(&tracedecay_dir, &transaction_id);
}

#[test]
fn orphan_commit_before_deleted_promotion_recovers_as_committed() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let orphan = tracedecay_dir.join("branches/orphan.db");
    std::fs::write(&orphan, b"orphan").unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Gc,
        u64::MAX,
        0,
    )
    .unwrap();
    let fence = crate::db::DatabaseDeletionFence::acquire(
        std::slice::from_ref(&orphan),
        "delete orphan branch test",
    )
    .unwrap();
    let transaction_id = fence.transaction_id().to_string();

    let error = prepared
        .commit_with_transaction(
            &transaction_id,
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            || failpoint("crash after orphan commit"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("crash after orphan commit"));
    drop(fence);

    let journal =
        std::fs::read_to_string(tracedecay_dir.join(".branch-delete-transaction.json")).unwrap();
    assert!(journal.contains(r#""state": "committed_orphans""#));
    assert!(!orphan.exists());
    let states = recover_committed_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.deleting(), 1);
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(crate::db::database_path_is_tombstoned(&orphan).unwrap());
}

#[cfg(unix)]
#[test]
fn committed_recovery_syncs_store_directory_before_tombstone_transition() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let orphan = tracedecay_dir.join("branches/orphan.db");
    std::fs::write(&orphan, b"orphan").unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Gc,
        u64::MAX,
        0,
    )
    .unwrap();
    let fence = crate::db::DatabaseDeletionFence::acquire(
        std::slice::from_ref(&orphan),
        "delete orphan branch test",
    )
    .unwrap();
    let transaction_id = fence.transaction_id().to_string();
    prepared
        .commit_with_transaction(
            &transaction_id,
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            || failpoint("crash after orphan commit"),
        )
        .unwrap_err();
    drop(fence);

    let recovery = prepare_pending_branch_admin_recovery(&tracedecay_dir)
        .unwrap()
        .unwrap();
    let branches = tracedecay_dir.join("branches");
    let displaced = tracedecay_dir.join("branches-displaced");
    std::fs::rename(&branches, &displaced).unwrap();
    let transitioned = std::cell::Cell::new(false);
    let error = recovery
        .recover(
            |_| Ok(()),
            |_| {
                transitioned.set(true);
                Ok(())
            },
        )
        .unwrap_err();

    assert!(error.to_string().contains("failed to sync directory"));
    assert!(!transitioned.get());
    assert!(std::fs::read_dir(&displaced).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".branch-delete-")
    }));
    std::fs::rename(displaced, branches).unwrap();
    recover_committed_with_fence(&tracedecay_dir, &transaction_id);
}

#[test]
fn partial_rename_failpoint_rolls_back_entire_sqlite_family() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let wal = db.with_extension("db-wal");
    std::fs::write(&wal, b"wal").unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::AfterMove(1) {
                return failpoint("partial rename failpoint");
            }
            Ok(())
        })
        .unwrap_err();

    assert!(error.to_string().contains("partial rename failpoint"));
    assert!(db.exists());
    assert!(wal.exists());
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[cfg(unix)]
#[test]
fn hard_linked_wal_is_rejected_before_journal_publication() {
    let (temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let wal = db.with_extension("db-wal");
    std::fs::write(&wal, b"wal").unwrap();
    std::fs::hard_link(&wal, temp.path().join("wal-alias")).unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("hard links"));
    assert!(db.exists());
    assert!(wal.exists());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn hard_linked_shm_is_rejected_before_journal_publication() {
    let (temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let shm = db.with_extension("db-shm");
    std::fs::write(&shm, b"shm").unwrap();
    std::fs::hard_link(&shm, temp.path().join("shm-alias")).unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("hard links"));
    assert!(db.exists());
    assert!(shm.exists());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[test]
fn metadata_publication_failpoint_rolls_back_quarantine() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::BeforeMetadataPublication {
                return failpoint("metadata publication failpoint");
            }
            Ok(())
        })
        .unwrap_err();

    assert!(error.to_string().contains("metadata publication failpoint"));
    assert!(db.exists());
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn post_commit_cleanup_failpoint_is_retried_during_next_lock_acquisition() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();

    let error = prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::AfterCommitBeforeCleanup {
                return failpoint("post-commit cleanup failpoint");
            }
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("post-commit cleanup failpoint"));
    assert!(!db.exists());
    assert!(
        tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
    assert!(!quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );

    recover_without_fence(&tracedecay_dir);
    let retry = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    assert_eq!(retry.report().outcome, BranchAdminOutcome::NotTracked);
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[test]
fn orphan_only_cleanup_retry_uses_explicit_committed_journal_state() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let orphan = tracedecay_dir.join("branches/orphan.db");
    std::fs::write(&orphan, b"orphan").unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Gc,
        u64::MAX,
        0,
    )
    .unwrap();
    assert_eq!(prepared.report().removed_orphan_dbs, vec![orphan.clone()]);

    prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::AfterCommitBeforeCleanup {
                return failpoint("orphan cleanup failpoint");
            }
            Ok(())
        })
        .unwrap_err();
    let journal =
        std::fs::read_to_string(tracedecay_dir.join(".branch-delete-transaction.json")).unwrap();
    assert!(journal.contains(r#""state": "committed_orphans""#));
    assert!(!orphan.exists());
    assert!(!quarantine_files(&tracedecay_dir).is_empty());

    recover_without_fence(&tracedecay_dir);
    let retry = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Gc,
        u64::MAX,
        0,
    )
    .unwrap();
    assert_eq!(retry.report().outcome, BranchAdminOutcome::NoChanges);
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        !tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );
}

#[test]
fn recreated_original_family_fails_closed_and_retains_recovery_evidence() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        0,
        0,
    )
    .unwrap();
    let fence = crate::db::DatabaseDeletionFence::acquire(
        std::slice::from_ref(&db),
        "delete branch recreation test",
    )
    .unwrap();
    let transaction_id = fence.transaction_id().to_string();
    let mut recreated = false;

    let error = prepared
        .commit_with_precommit_hook(
            Some(&transaction_id),
            || fence.publish_deleting(),
            |_| Ok(()),
            || fence.rollback_deleting(),
            |phase| {
                if phase == transaction::TransactionPhase::BeforeRefRevalidation && !recreated {
                    std::fs::write(&db, b"recreated").unwrap();
                    recreated = true;
                }
                Ok(())
            },
        )
        .unwrap_err();
    drop(fence);

    assert!(
        error
            .to_string()
            .contains("unexpected original branch store")
    );
    assert!(
        error
            .to_string()
            .contains("ambiguous source/quarantine state")
    );
    assert!(error.to_string().contains("recovery evidence was retained"));
    assert_eq!(std::fs::read(&db).unwrap(), b"recreated");
    let quarantine = quarantine_files(&tracedecay_dir);
    assert_eq!(quarantine.len(), 1);
    assert_eq!(std::fs::read(&quarantine[0]).unwrap(), b"feature");
    assert!(crate::db::database_path_is_tombstoned(&db).unwrap());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
    assert!(
        tracedecay_dir
            .join(".branch-delete-transaction.json")
            .exists()
    );

    std::fs::remove_file(&db).unwrap();
    let states = recover_precommit_with_fence(&tracedecay_dir, &transaction_id);
    assert_eq!(states.deleting(), 1);
    assert_eq!(std::fs::read(&db).unwrap(), b"feature");
    assert!(quarantine_files(&tracedecay_dir).is_empty());
}

#[test]
fn gc_ref_reappearance_failpoint_rolls_back_before_metadata_commit() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.branches.get_mut("feature").unwrap().last_synced_at = "0".to_string();
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Gc,
        0,
        u64::MAX,
    )
    .unwrap();
    assert_eq!(prepared.report().removed_branches, vec!["feature"]);
    let mut recreated = false;

    let error = prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::BeforeRefRevalidation && !recreated {
                run_git(&project_root, &["branch", "feature"]);
                recreated = true;
            }
            Ok(())
        })
        .unwrap_err();

    assert!(error.to_string().contains("reappeared"));
    assert!(db.exists());
    assert!(quarantine_files(&tracedecay_dir).is_empty());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}
