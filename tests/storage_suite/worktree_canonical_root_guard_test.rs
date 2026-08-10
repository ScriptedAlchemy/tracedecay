//! End-to-end coverage for the registry canonical_root worktree guard.
//!
//! A tracedecay project id is shared across every linked worktree of a
//! repository through the registered profile registry's
//! `git-common-dir:<common dir>` alias. Before the guard in
//! `tracedecay::project_registry::primary_checkout_root`, opening a session
//! from *any* linked worktree would re-register the shared project with
//! `canonical_root`/`display_root` pinned to that worktree's own (often
//! transient) path — the last worktree to touch the project would win.
//!
//! These tests drive the real registration/touch call site
//! (`TraceDecay::init_with_options` / `TraceDecay::open_with_options`)
//! against real git worktrees, rather than exercising the guard function in
//! isolation.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::home_env_lock::HOME_ENV_LOCK;

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn git_cli_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

/// Builds a primary checkout with one commit plus a linked worktree on a
/// separate branch, and a `TraceDecayOpenOptions` pointed at an isolated
/// profile root/global db under the same temp dir.
struct Fixture {
    _tmp: TempDir,
    profile_root: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
    open_options: TraceDecayOpenOptions,
}

fn build_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = canonical_temp_path(tmp.path());
    let profile_root = root.join("profile");
    let main = root.join("main");
    let worktree = root.join("main-wt");
    std::fs::create_dir_all(&main).unwrap();

    git(&main, &["init", "-b", "main", "--quiet"]);
    std::fs::write(main.join("README.md"), "worktree canonical_root fixture\n").unwrap();
    commit_all(&main, "initial commit");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree-guard",
            git_cli_path(&worktree).to_str().unwrap(),
            "HEAD",
        ],
    );

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };

    Fixture {
        _tmp: tmp,
        profile_root,
        main,
        worktree,
        open_options,
    }
}

async fn init_primary(fx: &Fixture) -> String {
    let primary = TraceDecay::init_with_options(&fx.main, fx.open_options.clone())
        .await
        .expect("primary init should succeed");
    primary.db().checkpoint().await.expect("primary checkpoint");
    let project_id = primary
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("profile-sharded store must have a project id");
    drop(primary);
    project_id
}

#[tokio::test]
async fn observation_store_resolver_maps_primary_and_linked_worktree_to_same_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let fx = build_fixture();
    let project_id = init_primary(&fx).await;
    let store_root = fx.profile_root.join(format!("projects/{project_id}"));
    let database_path = store_root.join("sessions.db");
    if !database_path.exists() {
        let (database, _) = crate::common::initialize_test_database(&database_path)
            .await
            .unwrap();
        drop(database);
    }
    let marker_path = tracedecay::storage::repository_identity_path(&fx.main)
        .expect("primary checkout should have a repository identity path");
    std::fs::remove_file(&marker_path).unwrap();

    let db = HostAdmissionTestRuntimeV1::profile(&fx.profile_root)
        .await
        .expect("global db should open");
    assert!(
        db.project_registry_context_by_alias(&fx.worktree)
            .await
            .unwrap()
            .is_none(),
        "the linked worktree must resolve through git common-dir, not a preexisting path alias"
    );

    let primary = db
        .resolve_project_observation_store(&fx.main.join("."))
        .await
        .expect("the primary checkout should resolve through its canonical path alias");
    let linked = db
        .resolve_project_observation_store(&fx.worktree)
        .await
        .expect("the linked worktree should resolve through its git common-dir alias");

    assert_eq!(primary.project().project_id, project_id);
    assert_eq!(linked.project().project_id, project_id);
    assert_eq!(linked.store(), primary.store());
    assert_eq!(linked.store_root(), primary.store_root());
    assert_eq!(linked.database_path(), primary.database_path());
    assert_eq!(primary.store_root(), store_root.canonicalize().unwrap());
    assert_eq!(
        primary.database_path(),
        database_path.canonicalize().unwrap()
    );
    drop(db);
}

#[tokio::test]
async fn opening_from_linked_worktree_keeps_canonical_root_on_primary() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let fx = build_fixture();
    let project_id = init_primary(&fx).await;

    // Opening from the linked worktree resolves the *same* project id via
    // the git-common-dir identity alias registered during primary init.
    let from_worktree = TraceDecay::open_with_options(&fx.worktree, fx.open_options.clone())
        .await
        .expect("open from linked worktree should succeed");
    assert_eq!(
        from_worktree.store_layout().identity.project_id.as_deref(),
        Some(project_id.as_str()),
        "a linked worktree session must resolve the primary's shared project id"
    );
    drop(from_worktree);

    let db = HostAdmissionTestRuntimeV1::profile(&fx.profile_root)
        .await
        .expect("global db should open");
    let record = db
        .get_code_project(&project_id)
        .await
        .expect("registry read should not fault")
        .expect("project should be registered");
    assert_eq!(
        record.canonical_root,
        HostAdmissionTestRuntimeV1::canonical_project_key(&fx.main),
        "canonical_root must stay pinned to the primary checkout, not the worktree that just touched it"
    );
    assert_eq!(
        record.display_root,
        fx.main.to_string_lossy(),
        "display_root must stay pinned to the primary checkout"
    );

    // The worktree's own path must still resolve (as an alias) to the same
    // shared project id, so future sessions opened from the worktree keep
    // working.
    let context = db
        .project_registry_context_by_id(&project_id)
        .await
        .expect("registry context should exist");
    let worktree_key = HostAdmissionTestRuntimeV1::canonical_project_key(&fx.worktree);
    assert!(
        context
            .aliases
            .iter()
            .any(|alias| alias.alias_path == worktree_key),
        "the worktree path must remain a resolvable alias: {:?}",
        context.aliases
    );
}

#[tokio::test]
async fn stale_worktree_canonical_root_heals_on_next_touch() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let fx = build_fixture();
    let project_id = init_primary(&fx).await;

    // Simulate the pre-guard bug: some earlier session registered straight
    // from the worktree and pinned canonical_root/display_root to it.
    {
        let db = HostAdmissionTestRuntimeV1::profile(&fx.profile_root)
            .await
            .expect("global db should open");
        let git_common_dir = tracedecay::worktree::git_common_dir(&fx.worktree);
        db.upsert_code_project(
            &project_id,
            &fx.worktree,
            git_common_dir.as_deref(),
            None,
            Some("feature/worktree-guard"),
        )
        .await
        .expect("seeding the stale row should succeed");
        db.checkpoint_profile_database_for_test().await;
        drop(db);

        let db = HostAdmissionTestRuntimeV1::profile(&fx.profile_root)
            .await
            .expect("global db should reopen");
        let stale = db
            .get_code_project(&project_id)
            .await
            .expect("registry read should not fault")
            .expect("stale row should exist");
        assert_eq!(
            stale.canonical_root,
            HostAdmissionTestRuntimeV1::canonical_project_key(&fx.worktree),
            "fixture setup should have produced the stale (bug) state"
        );
        drop(db);
    }

    // Any subsequent touch — even one opened from the same worktree — must
    // self-heal canonical_root/display_root back to the primary checkout.
    let reopened = TraceDecay::open_with_options(&fx.worktree, fx.open_options.clone())
        .await
        .expect("reopen from worktree should succeed");
    drop(reopened);

    let db = HostAdmissionTestRuntimeV1::profile(&fx.profile_root)
        .await
        .expect("global db should open");
    let healed = db
        .get_code_project(&project_id)
        .await
        .expect("registry read should not fault")
        .expect("project should still be registered");
    assert_eq!(
        healed.canonical_root,
        HostAdmissionTestRuntimeV1::canonical_project_key(&fx.main),
        "a stale worktree-pinned canonical_root must heal back to the primary checkout on touch"
    );
    assert_eq!(healed.display_root, fx.main.to_string_lossy());
}

// Coverage for "the primary checkout no longer exists on disk" lives in
// `tracedecay::project_registry::tests::primary_checkout_root_keeps_worktree_when_primary_checkout_is_missing`.
// A real git worktree cannot produce that state end-to-end: a linked
// worktree resolves `git_common_dir` by reading files inside the primary's
// `.git` directory, so deleting the primary also deletes the very metadata
// the worktree needs to resolve a common dir at all — the unit test exposes
// the guard function directly to exercise that branch precisely instead.
