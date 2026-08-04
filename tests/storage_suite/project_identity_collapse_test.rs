//! Repository identity must survive linked worktrees and reject ephemeral roots.
//!
//! Two defects are covered here.
//!
//! 1. A *detached* linked worktree used to be excluded from both the
//!    repository identity marker (`storage::repository_identity_path`) and
//!    the registry's `git_common_dir` resolution, so first touch fell through
//!    to the path-hashed identity fallback and minted a second project store
//!    for a repository that already had one.
//! 2. Nothing refused to mint a project authority for a root under the OS
//!    temporary directory, so an installed binary run against a `mktemp -d`
//!    checkout enrolled a throwaway path into the real user profile registry
//!    forever.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::global_db::StoreInstanceUpsert;
use tracedecay::project_registry::ReapEntryKind;
use tracedecay::storage::{default_profile_project_id, repository_identity_path, resolve_layout};

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One primary checkout plus two linked worktrees of the same repository: one
/// on its own branch, one with a detached HEAD (what `git worktree add
/// --detach` and most agent worktree tooling produce).
struct RepoFixture {
    _tmp: TempDir,
    main: PathBuf,
    attached: PathBuf,
    detached: PathBuf,
}

fn repo_with_worktrees() -> RepoFixture {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = root.join("main");
    std::fs::create_dir_all(main.join("packages/app")).unwrap();
    git(&main, &["init", "--quiet"]);
    std::fs::write(main.join("README.md"), "hi").unwrap();
    std::fs::write(main.join("packages/app/lib.rs"), "fn main() {}").unwrap();
    git(&main, &["add", "."]);
    git(
        &main,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
    );

    let attached = root.join("attached-wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            attached.to_str().unwrap(),
        ],
    );
    let detached = root.join("detached-wt");
    git(
        &main,
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );

    RepoFixture {
        _tmp: tmp,
        main,
        attached,
        detached,
    }
}

/// A directory guaranteed to sit outside `std::env::temp_dir()`. Cargo never
/// places build output inside the volatile system temp directory, so deriving
/// the base from the running test binary is robust even when the checkout
/// itself lives under `/tmp`.
fn non_ephemeral_base() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary has a current_exe path");
    let base = exe
        .parent()
        .and_then(Path::parent)
        .expect("test binary sits under a cargo target profile directory")
        .join("project-identity-collapse-fixtures");
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn every_linked_worktree_shares_its_repository_project_identity() {
    let fixture = repo_with_worktrees();

    let main = default_profile_project_id(&fixture.main);
    let attached = default_profile_project_id(&fixture.attached);
    let detached = default_profile_project_id(&fixture.detached);

    assert_eq!(
        main, attached,
        "an attached linked worktree must not mint its own project identity"
    );
    assert_eq!(
        main, detached,
        "a detached linked worktree must not mint its own project identity"
    );
}

#[test]
fn linked_worktrees_resolve_to_one_store() {
    let fixture = repo_with_worktrees();
    let profile = fixture.main.parent().unwrap().join("profile");

    let main = resolve_layout(&fixture.main, &profile).unwrap();
    let attached = resolve_layout(&fixture.attached, &profile).unwrap();
    let detached = resolve_layout(&fixture.detached, &profile).unwrap();

    assert_eq!(
        main.data_root, attached.data_root,
        "an attached linked worktree must resolve to the repository's store"
    );
    assert_eq!(
        main.data_root, detached.data_root,
        "a detached linked worktree must resolve to the repository's store"
    );
}

#[test]
fn detached_worktree_reads_the_shared_repository_identity_marker() {
    let fixture = repo_with_worktrees();

    assert_eq!(
        repository_identity_path(&fixture.detached),
        repository_identity_path(&fixture.main),
        "a detached linked worktree shares its repository's identity marker"
    );
}

/// The collapse keys on the *worktree root*, never on "some ancestor is a git
/// repository". A subdirectory indexed as its own project (a monorepo package)
/// must keep a distinct identity, or unrelated projects would merge.
#[test]
fn subdirectory_projects_keep_their_own_identity() {
    let fixture = repo_with_worktrees();

    assert_ne!(
        default_profile_project_id(&fixture.main.join("packages/app")),
        default_profile_project_id(&fixture.main),
        "a package directory inside a repository is its own project"
    );
}

#[tokio::test]
async fn ephemeral_project_root_cannot_enter_a_durable_registry() {
    let profile = tempfile::Builder::new()
        .prefix("durable-profile-")
        .tempdir_in(non_ephemeral_base())
        .unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(profile.path().join("profile"))
        .await
        .unwrap();
    let ephemeral = TempDir::new().unwrap();

    let registered = db
        .upsert_code_project("proj_ephemeral", ephemeral.path(), None, None, None)
        .await;

    assert!(
        registered.is_none(),
        "a root under the OS temp directory must not become a durable project authority"
    );
    assert!(
        db.project_registry_context_by_alias(ephemeral.path())
            .await
            .unwrap()
            .is_none(),
        "the refused root must leave no alias behind"
    );
}

/// The ephemeral guard compares the root against the *profile*: a hermetic
/// test profile is itself throwaway, so temp fixtures must keep working.
#[tokio::test]
async fn ephemeral_project_root_is_allowed_by_a_hermetic_profile() {
    let dir = TempDir::new().unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path().join("profile"))
        .await
        .unwrap();
    let project = dir.path().join("fixture-project");
    std::fs::create_dir_all(&project).unwrap();

    assert!(
        db.upsert_code_project("proj_hermetic", &project, None, None, None)
            .await
            .is_some(),
        "a throwaway profile must still accept throwaway project fixtures"
    );
}

#[tokio::test]
async fn common_dir_aliases_mint_one_project_and_one_store_authority() {
    let fixture = repo_with_worktrees();
    let profile_root = fixture.main.parent().unwrap().join("profile");
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let common_dir = fixture.main.join(".git").canonicalize().unwrap();

    db.upsert_code_project(
        "proj_primary",
        &fixture.main,
        Some(&common_dir),
        None,
        Some("main"),
    )
    .await
    .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_primary:profile_sharded".to_string(),
        project_id: "proj_primary".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "projects/proj_primary".to_string(),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: None,
    })
    .await
    .unwrap();

    let linked = db
        .upsert_code_project(
            "proj_linked",
            &fixture.attached,
            Some(&common_dir),
            None,
            Some("feature"),
        )
        .await
        .unwrap();

    assert_eq!(linked.project_id, "proj_primary");
    assert!(db.get_code_project("proj_linked").await.is_none());
    assert!(
        db.upsert_store_instance(StoreInstanceUpsert {
            store_id: "store:proj_linked:profile_sharded".to_string(),
            project_id: "proj_linked".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_linked".to_string(),
            manifest_relpath: None,
            last_verified_at: None,
            last_write_at: None,
        })
        .await
        .is_none()
    );

    let context = db
        .project_registry_context_by_identity(&fixture.detached, Some(&common_dir))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(context.project.project_id, "proj_primary");
    assert_eq!(context.stores.len(), 1);
    assert_eq!(
        context.stores[0].store.store_id,
        "store:proj_primary:profile_sharded"
    );

    #[cfg(unix)]
    {
        let common_dir_alias = fixture.main.parent().unwrap().join("common-dir-alias");
        std::os::unix::fs::symlink(&common_dir, &common_dir_alias).unwrap();
        let aliased = db
            .upsert_code_project(
                "proj_symlink_alias",
                &fixture.detached,
                Some(&common_dir_alias),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(aliased.project_id, "proj_primary");
        assert!(db.get_code_project("proj_symlink_alias").await.is_none());
    }
}

#[tokio::test]
async fn registry_entries_for_deleted_paths_are_reapable() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let db = HostAdmissionTestRuntimeV1::profile(profile_root.clone())
        .await
        .unwrap();

    let live = dir.path().join("live-project");
    let deleted = dir.path().join("deleted-project");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::create_dir_all(&deleted).unwrap();
    db.upsert_code_project("proj_live", &live, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("proj_deleted", &deleted, None, None, None)
        .await
        .unwrap();
    std::fs::remove_dir_all(&deleted).unwrap();

    let plan = db.plan_registry_reap().await.unwrap();
    let reaped_authorities = plan
        .reapable
        .iter()
        .filter(|entry| entry.kind == ReapEntryKind::CodeProject)
        .map(|entry| entry.key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        reaped_authorities,
        ["proj_deleted"],
        "only the authority whose root is gone may be reaped"
    );

    let removed = db.apply_registry_reap(&plan).await.unwrap();
    assert!(removed > 0, "reaping must remove the dead rows it planned");
    assert!(
        db.get_code_project("proj_deleted").await.is_none(),
        "the dead authority must be gone from the registry"
    );
    assert!(
        db.get_code_project("proj_live").await.is_some(),
        "reaping must never touch a live project"
    );
    assert!(
        db.plan_registry_reap().await.unwrap().is_empty(),
        "reaping must converge"
    );
}

/// A vanished path is not permission to discard data. Two branch stores were
/// deleted on the strength of a dead path and both held facts that existed
/// nowhere else, so an authority whose store is still on disk is reported and
/// kept rather than reaped.
#[tokio::test]
async fn a_dead_path_with_a_surviving_store_is_retained_not_reaped() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let db = HostAdmissionTestRuntimeV1::profile(profile_root.clone())
        .await
        .unwrap();

    let deleted = dir.path().join("deleted-but-indexed");
    std::fs::create_dir_all(&deleted).unwrap();
    db.upsert_code_project("proj_with_store", &deleted, None, None, None)
        .await
        .unwrap();
    let store_root = profile_root.join("projects").join("proj_with_store");
    std::fs::create_dir_all(&store_root).unwrap();
    let evidence = store_root.join("sessions.db");
    std::fs::write(&evidence, b"irreplaceable").unwrap();
    std::fs::remove_dir_all(&deleted).unwrap();

    let plan = db.plan_registry_reap().await.unwrap();

    assert!(
        plan.reapable
            .iter()
            .all(|entry| entry.kind != ReapEntryKind::CodeProject),
        "an authority backed by a surviving store must not be reaped"
    );
    assert!(
        plan.retained
            .iter()
            .any(|retained| retained.entry.key == "proj_with_store"),
        "the retained authority must be reported with a reason, not silently skipped"
    );

    db.apply_registry_reap(&plan).await.unwrap();
    assert!(evidence.is_file(), "reaping must never delete store data");
    assert!(
        db.get_code_project("proj_with_store").await.is_some(),
        "the retained authority must survive the reap"
    );
}
