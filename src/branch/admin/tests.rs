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

fn prepare_remove(project_root: &Path, tracedecay_dir: &Path) -> PreparedBranchAdminMutation {
    prepare_branch_admin_mutation(
        project_root,
        tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "feature".to_string(),
        },
        14,
        7,
    )
    .unwrap()
}

fn failpoint(message: &str) -> crate::errors::Result<()> {
    Err(crate::errors::TraceDecayError::Config {
        message: message.to_string(),
    })
}

#[test]
fn selection_is_read_only_and_commit_unlinks_exact_family() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    std::fs::write(db.with_extension("db-wal"), b"wal").unwrap();
    std::fs::write(db.with_extension("db-shm"), b"shm").unwrap();
    let prepared = prepare_remove(&project_root, &tracedecay_dir);

    assert_eq!(prepared.database_paths(), std::slice::from_ref(&db));
    assert!(db.exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );

    let report = prepared.commit().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(!db.exists());
    assert!(!db.with_extension("db-wal").exists());
    assert!(!db.with_extension("db-shm").exists());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn crash_before_metadata_cas_preserves_route_and_files() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let error = prepare_remove(&project_root, &tracedecay_dir)
        .commit_with_hook(|boundary| {
            if boundary == BranchAdminCommitBoundary::BeforeMetadataCas {
                failpoint("crash before metadata CAS")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(error.to_string().contains("crash before metadata CAS"));
    assert!(db.exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn crash_after_metadata_cas_leaves_only_unreferenced_files() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let error = prepare_remove(&project_root, &tracedecay_dir)
        .commit_with_hook(|boundary| {
            if boundary == BranchAdminCommitBoundary::AfterMetadataCas {
                failpoint("crash after metadata CAS")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(error.to_string().contains("crash after metadata CAS"));
    assert!(db.exists());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn metadata_cas_rejects_changed_store_path_without_unlink() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let prepared = prepare_remove(&project_root, &tracedecay_dir);
    let mut changed = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    changed.branches.get_mut("feature").unwrap().db_file = "branches/recreated.db".to_owned();
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &changed).unwrap();

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("destructive CAS refused"));
    assert!(db.exists());
    assert_eq!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .branches["feature"]
            .db_file,
        "branches/recreated.db"
    );
}

#[test]
fn gc_ref_reappearance_is_refused_before_metadata_cas() {
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
    run_git(&project_root, &["branch", "feature"]);

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("reappeared"));
    assert!(db.exists());
    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn nonempty_metadata_only_finish_fails_closed_without_deleting() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");
    let error = prepare_remove(&project_root, &tracedecay_dir)
        .finish_without_database_deletion()
        .unwrap_err();
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

#[test]
fn failed_branch_sync_rollback_retires_only_metadata() {
    let (_temp, _project_root, tracedecay_dir) = fixture();
    let db = tracedecay_dir.join("branches/feature.db");

    rollback_published_branch_tracking(&tracedecay_dir, "feature", "branches/feature.db").unwrap();

    assert!(db.exists());
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

/// A branch tracked on the single project store retires metadata-only: no
/// physical deletion candidate may reference the shared main database.
#[test]
fn removing_a_single_store_branch_never_deletes_the_project_store() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.add_branch("topic", crate::config::DB_FILENAME, "main");
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
    let main_db = tracedecay_dir.join(crate::config::DB_FILENAME);

    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::Remove {
            branch: "topic".to_string(),
        },
        14,
        7,
    )
    .unwrap();

    assert!(
        prepared.database_paths().is_empty(),
        "single-store branch removal must not select any database for deletion"
    );
    // The daemon routes empty selections through the metadata-only commit.
    let report = prepared.finish_without_database_deletion().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(main_db.exists(), "the project store must survive removal");
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("topic")
    );
}

/// GC of a dead single-store branch collects its metadata while the shared
/// main database survives; a dead legacy private copy is still physically
/// collected in the same pass (Plan 38 keep-list).
#[test]
fn gc_collects_single_store_metadata_and_legacy_stores_but_keeps_the_project_store() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let legacy_db = tracedecay_dir.join("branches/feature.db");
    let main_db = tracedecay_dir.join(crate::config::DB_FILENAME);
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.add_branch("topic", crate::config::DB_FILENAME, "main");
    meta.branches.get_mut("topic").unwrap().last_synced_at = "0".to_string();
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

    assert_eq!(prepared.report().removed_branches, vec!["feature", "topic"]);
    assert_eq!(
        prepared.database_paths(),
        std::slice::from_ref(&legacy_db),
        "only the legacy private store may be a physical deletion candidate"
    );
    let report = prepared.commit().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(
        !legacy_db.exists(),
        "legacy private store must be collected"
    );
    assert!(main_db.exists(), "the project store must survive GC");
    let persisted = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    assert!(!persisted.is_tracked("topic"));
    assert!(!persisted.is_tracked("feature"));
}
