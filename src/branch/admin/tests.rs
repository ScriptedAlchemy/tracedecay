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
    let mut recreated = false;

    let error = prepared
        .commit_with_hook(None, |phase| {
            if phase == transaction::TransactionPhase::BeforeRefRevalidation && !recreated {
                std::fs::write(&db, b"recreated").unwrap();
                recreated = true;
            }
            Ok(())
        })
        .unwrap_err();

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
    recover_without_fence(&tracedecay_dir);
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
