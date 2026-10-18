use super::*;

fn run_git(project_root: &Path, args: &[&str]) {
    let output = std::process::Command::new(
        crate::git::try_git_program().expect("absolute git executable should resolve"),
    )
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
    std::fs::create_dir_all(&tracedecay_dir).unwrap();
    run_git(&project_root, &["init", "-b", "main"]);
    run_git(&project_root, &["config", "user.email", "test@example.com"]);
    run_git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    std::fs::write(project_root.join("fixture"), b"fixture").unwrap();
    run_git(&project_root, &["add", "fixture"]);
    run_git(&project_root, &["commit", "-m", "fixture"]);
    std::fs::write(tracedecay_dir.join(crate::config::DB_FILENAME), b"main").unwrap();
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "main");
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
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
    )
    .unwrap()
}

fn add_sealed_single_store_branch(tracedecay_dir: &Path, branch: &str) {
    let mut meta = crate::branch_meta::load_branch_meta(tracedecay_dir).unwrap();
    meta.add_branch(branch, "main");
    crate::branch_meta::save_branch_meta(tracedecay_dir, &meta).unwrap();
    let source = crate::branch_meta::BranchGraphSourceDraftV1 {
        project_id: "project".to_owned(),
        repository_id: "repository".to_owned(),
        worktree_id: format!("worktree-{branch}"),
        worktree_root: format!("/manual/{branch}"),
        reference: format!("refs/heads/tracedecay/track/{branch}"),
        source_oid: format!("oid-{branch}"),
    };
    let outcome =
        crate::branch_meta::publish_graph_source(tracedecay_dir, branch, None, source).unwrap();
    assert!(matches!(
        outcome,
        crate::branch_meta::BranchGraphSourcePublishOutcomeV1::Published(_)
    ));
}

#[test]
fn selection_is_read_only_and_commit_retires_metadata_only() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let main_db = tracedecay_dir.join(crate::config::DB_FILENAME);
    let prepared = prepare_remove(&project_root, &tracedecay_dir);

    assert!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );

    let report = prepared.commit().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(main_db.exists(), "the project store must survive removal");
    assert!(
        !crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

#[test]
fn metadata_cas_rejects_a_concurrent_metadata_change() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let prepared = prepare_remove(&project_root, &tracedecay_dir);
    let mut changed = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    changed.branches.get_mut("feature").unwrap().last_synced_at = "foreign".to_owned();
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &changed).unwrap();

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("CAS refused"));
    assert_eq!(
        crate::branch_meta::load_branch_meta(&tracedecay_dir)
            .unwrap()
            .branches["feature"]
            .last_synced_at,
        "foreign"
    );
}

#[test]
fn gc_ref_reappearance_is_refused_before_metadata_cas() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.branches.get_mut("feature").unwrap().last_synced_at = "0".to_string();
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
    let prepared =
        prepare_branch_admin_mutation(&project_root, &tracedecay_dir, BranchAdminAction::Gc, 0)
            .unwrap();
    assert_eq!(prepared.report().removed_branches, vec!["feature"]);
    run_git(&project_root, &["branch", "feature"]);

    let error = prepared.commit().unwrap_err();

    assert!(error.to_string().contains("reappeared"));
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
    )
    .err()
    .expect("default branch removal must fail closed");
    assert!(error.to_string().contains("cannot remove default branch"));
    assert!(tracedecay_dir.join(crate::config::DB_FILENAME).exists());
}

#[test]
fn branch_admin_refuses_corrupt_metadata() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    std::fs::write(
        tracedecay_dir.join(crate::storage::BRANCH_META_FILENAME),
        b"{not-json",
    )
    .unwrap();

    let error =
        prepare_branch_admin_mutation(&project_root, &tracedecay_dir, BranchAdminAction::Gc, 0)
            .err()
            .expect("corrupt branch metadata must fail closed");

    assert!(error.to_string().contains("corrupt or unreadable metadata"));
}

#[test]
fn remove_all_carries_exact_single_store_provenance_for_daemon_retirement() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    add_sealed_single_store_branch(&tracedecay_dir, "feature/one");
    add_sealed_single_store_branch(&tracedecay_dir, "feature/two");

    let prepared = prepare_branch_admin_mutation(
        &project_root,
        &tracedecay_dir,
        BranchAdminAction::RemoveAll,
        14,
    )
    .unwrap();

    assert_eq!(
        prepared
            .single_store_retirements()
            .iter()
            .map(|retirement| retirement.branch.as_str())
            .collect::<Vec<_>>(),
        vec!["feature/one", "feature/two"],
        "remove-all must retain exact source provenance until cleanup commits"
    );
}

/// GC of dead branches collects their metadata while the shared main
/// database survives; live and protected branches are retained.
#[test]
fn gc_collects_dead_branch_metadata_but_keeps_the_project_store() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    let main_db = tracedecay_dir.join(crate::config::DB_FILENAME);
    run_git(&project_root, &["branch", "live"]);
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.add_branch("topic", "main");
    meta.add_branch("live", "main");
    meta.add_branch("pinned", "main");
    for branch in ["feature", "topic", "live", "pinned"] {
        meta.branches.get_mut(branch).unwrap().last_synced_at = "0".to_string();
    }
    meta.branches.get_mut("pinned").unwrap().gc_protected = true;
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();

    let prepared =
        prepare_branch_admin_mutation(&project_root, &tracedecay_dir, BranchAdminAction::Gc, 0)
            .unwrap();

    assert_eq!(prepared.report().removed_branches, vec!["feature", "topic"]);
    let report = prepared.commit().unwrap();
    assert_eq!(report.outcome, BranchAdminOutcome::Removed);
    assert!(main_db.exists(), "the project store must survive GC");
    let persisted = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    assert!(!persisted.is_tracked("topic"));
    assert!(!persisted.is_tracked("feature"));
    assert!(persisted.is_tracked("live"));
    assert!(persisted.is_tracked("pinned"));
}

#[test]
fn gc_carries_only_exact_sealed_single_store_provenance_for_retirement() {
    let (_temp, project_root, tracedecay_dir) = fixture();
    add_sealed_single_store_branch(&tracedecay_dir, "feature/stale");
    let mut meta = crate::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    meta.branches
        .get_mut("feature/stale")
        .unwrap()
        .last_synced_at = "0".to_owned();
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();

    let prepared =
        prepare_branch_admin_mutation(&project_root, &tracedecay_dir, BranchAdminAction::Gc, 0)
            .unwrap();

    assert_eq!(
        prepared
            .single_store_retirements()
            .iter()
            .map(|retirement| retirement.branch.as_str())
            .collect::<Vec<_>>(),
        vec!["feature/stale"],
        "GC must carry sealed manual provenance instead of deleting metadata alone"
    );
}
