use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use fs2::FileExt;
#[cfg(unix)]
use serde_json::Value;

use crate::common;
use crate::common::fixture::{ClosedRegisteredProject, GitFixture, TestProfile};
use tracedecay::branch::BranchAddOutcome;
use tracedecay::branch_meta::load_branch_meta;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

/// An indexed project on `main` that has been closed so a later checkout can
/// reopen against a live branch tip.
struct BranchSafetyProject {
    repo: GitFixture,
    project: ClosedRegisteredProject,
}

impl BranchSafetyProject {
    async fn indexed_on_main() -> Self {
        let profile = TestProfile::acquire().await;
        let repo = GitFixture::primary(profile.path("project"));
        fs::create_dir_all(repo.root().join("src")).unwrap();
        fs::write(
            repo.root().join("src/lib.rs"),
            "pub fn indexed_on_main() {}\n",
        )
        .unwrap();
        repo.commit_all("initial commit");
        let project = profile.enroll_indexed(repo.root()).await.close().await;
        Self { repo, project }
    }

    fn root(&self) -> &Path {
        self.project.root()
    }

    fn data_root(&self) -> &Path {
        self.project.data_root()
    }

    fn open_options(&self) -> TraceDecayOpenOptions {
        self.project.open_options()
    }

    async fn reopen(&self) -> TraceDecay {
        self.project.reopen().await
    }
}

async fn open_untracked_project() -> (BranchSafetyProject, TraceDecay) {
    let fixture = BranchSafetyProject::indexed_on_main().await;

    fixture.repo.run(&["checkout", "-b", "feature/untracked"]);
    fs::write(
        fixture.root().join("src/untracked_only.rs"),
        "pub fn untracked_only() {}\n",
    )
    .unwrap();

    let feature = fixture.reopen().await;
    assert_eq!(feature.active_branch(), Some("feature/untracked"));
    assert_eq!(feature.serving_branch(), Some("feature/untracked"));
    assert!(!feature.is_fallback());
    let layout = feature.store_layout();
    assert_eq!(layout.dirty_path, layout.data_root.join("dirty"));
    assert_eq!(layout.sync_lock_path, layout.data_root.join("sync.lock"));
    assert_ne!(feature.db_path(), layout.graph_db_path);

    (fixture, feature)
}

async fn open_detached_fallback_project() -> (BranchSafetyProject, TraceDecay) {
    let fixture = BranchSafetyProject::indexed_on_main().await;

    fixture.repo.run(&["checkout", "--detach"]);

    fs::write(
        fixture.root().join("src/detached_only.rs"),
        "pub fn detached_only() {}\n",
    )
    .unwrap();

    let fallback = fixture.reopen().await;
    assert_eq!(fallback.active_branch(), None);
    assert_eq!(fallback.serving_branch(), Some("main"));
    assert!(fallback.is_fallback());
    assert!(
        fallback
            .fallback_warning()
            .unwrap_or_default()
            .contains("detached HEAD"),
        "detached HEAD should explain the fallback branch"
    );

    (fixture, fallback)
}

async fn assert_main_db_missing_symbol(
    fixture: &BranchSafetyProject,
    symbol: &str,
    message: &str,
) {
    fixture.repo.run(&["checkout", "main"]);
    let main = fixture.reopen().await;
    let results = main.search(symbol, 10).await.unwrap();
    assert!(results.is_empty(), "{message}");
}

fn assert_fallback_write_refused(operation: &str, err: impl std::fmt::Display) {
    let message = err.to_string();
    assert!(
        message.contains("fallback")
            && (message.contains("tracedecay branch add")
                || message.contains("Check out a tracked branch")),
        "unexpected {operation} error: {message}"
    );
}

#[tokio::test]
// Regression: init and reopen must use the same graph-scoped lock.
async fn init_index_uses_graph_specific_sync_lock() {
    let profile = TestProfile::acquire().await;
    let project_root = profile.path("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn indexed() {}\n").unwrap();

    let project = profile.enroll(&project_root).await;
    let lock_path = PathBuf::from(format!("{}.sync.lock", project.db_path().display()));
    fs::write(&lock_path, std::process::id().to_string()).unwrap();

    let err = match project.index_all().await {
        Ok(_) => panic!("active graph lock must block init indexing"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("another sync is already in progress")
    );
}

#[tokio::test]
async fn corrupt_derived_branch_store_is_preserved_and_rebuilt_automatically() {
    let (fixture, feature) = open_untracked_project().await;
    let open_options = fixture.open_options();
    let layout = feature.store_layout().clone();
    let corrupt_db = feature.db_path().to_path_buf();
    feature.checkpoint().await.unwrap();
    feature.close();

    let sessions_bytes = b"sessions-are-separate";
    fs::write(&layout.sessions_db_path, sessions_bytes).unwrap();
    let mut corrupted = fs::read(&corrupt_db).unwrap();
    corrupted[..16].copy_from_slice(b"not-a-sqlite-db!");
    fs::write(&corrupt_db, &corrupted).unwrap();

    let wal_path = PathBuf::from(format!("{}-wal", corrupt_db.display()));
    let shm_path = PathBuf::from(format!("{}-shm", corrupt_db.display()));
    let dirty_path = PathBuf::from(format!("{}.dirty", corrupt_db.display()));
    let wal_bytes = b"preserve-corrupt-wal";
    let shm_bytes = b"preserve-corrupt-shm";
    let dirty_bytes = b"pid=99999\nversion=old";
    fs::write(&wal_path, wal_bytes).unwrap();
    fs::write(&shm_path, shm_bytes).unwrap();
    fs::write(&dirty_path, dirty_bytes).unwrap();
    fs::write(&layout.dirty_path, dirty_bytes).unwrap();

    let repaired = TraceDecay::open_with_options(fixture.root(), open_options)
        .await
        .expect("a corrupt derived branch index should rebuild from its tracked ancestor");
    assert_eq!(repaired.active_branch(), Some("feature/untracked"));
    assert_eq!(repaired.serving_branch(), Some("feature/untracked"));
    assert!(!repaired.is_fallback());
    assert!(repaired.db().quick_check().await.unwrap());
    assert!(
        !repaired
            .search("untracked_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "the rebuilt branch index must include branch-only working-tree symbols"
    );
    assert_eq!(fs::read(&layout.sessions_db_path).unwrap(), sessions_bytes);
    assert!(!layout.dirty_path.exists());

    let recovery_root = layout.data_root.join("recovery");
    let recovery_dirs = fs::read_dir(&recovery_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    assert_eq!(recovery_dirs.len(), 1);
    let recovery_dir = &recovery_dirs[0];
    assert_eq!(
        fs::read(recovery_dir.join(corrupt_db.file_name().unwrap())).unwrap(),
        corrupted
    );
    assert_eq!(
        fs::read(recovery_dir.join(wal_path.file_name().unwrap())).unwrap(),
        wal_bytes
    );
    assert_eq!(
        fs::read(recovery_dir.join(shm_path.file_name().unwrap())).unwrap(),
        shm_bytes
    );
    assert_eq!(
        fs::read(recovery_dir.join(dirty_path.file_name().unwrap())).unwrap(),
        dirty_bytes
    );
    assert!(corrupt_db.exists());
    assert_ne!(fs::read(&corrupt_db).unwrap(), corrupted);
    assert!(!dirty_path.exists());
    repaired.close();
}

#[tokio::test]
async fn corrupt_derived_branch_repair_waits_for_the_active_sync_lease() {
    let (fixture, feature) = open_untracked_project().await;
    let open_options = fixture.open_options();
    let corrupt_db = feature.db_path().to_path_buf();
    let recovery_root = feature.store_layout().data_root.join("recovery");
    feature.checkpoint().await.unwrap();
    feature.close();

    let mut corrupted = fs::read(&corrupt_db).unwrap();
    corrupted[..16].copy_from_slice(b"not-a-sqlite-db!");
    fs::write(&corrupt_db, &corrupted).unwrap();
    let lock_path = corrupt_db.with_file_name(format!(
        "{}.sync.lock",
        corrupt_db.file_name().unwrap().to_string_lossy()
    ));
    let active_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    active_lock.try_lock_exclusive().unwrap();

    let error = match TraceDecay::open_with_options(fixture.root(), open_options.clone()).await {
        Ok(_) => panic!("active writer lease must block branch recovery"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("another sync is already in progress"),
        "unexpected active-lease recovery error: {error}"
    );
    assert_eq!(fs::read(&corrupt_db).unwrap(), corrupted);
    assert!(!recovery_root.exists());

    drop(active_lock);
    let repaired = TraceDecay::open_with_options(fixture.root(), open_options)
        .await
        .expect("repair should proceed after the active lease is released");
    assert!(repaired.db().quick_check().await.unwrap());
    assert_ne!(fs::read(&corrupt_db).unwrap(), corrupted);
    repaired.close();
}

#[tokio::test]
async fn open_honors_and_clears_legacy_dirty_sentinel() {
    let profile = TestProfile::acquire().await;
    let project_root = profile.path("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn indexed() {}\n").unwrap();

    let project = profile.enroll_indexed(&project_root).await;
    let legacy_dirty = project.store_layout().data_root.join("dirty");
    fs::write(&legacy_dirty, "interrupted legacy writer").unwrap();
    let project = project.close().await;

    let reopened = project.reopen().await;
    assert!(
        !legacy_dirty.exists(),
        "successful recovery must clear legacy sentinel"
    );
    drop(reopened);
}

#[tokio::test]
async fn open_auto_tracks_untracked_branch_and_syncs_its_db() {
    let (fixture, feature) = open_untracked_project().await;

    assert!(
        !feature
            .search("untracked_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "auto-tracked branch should contain the branch-only symbol"
    );

    let meta = load_branch_meta(fixture.data_root()).unwrap();
    let feature_entry = meta
        .branches
        .get("feature/untracked")
        .expect("open should add the live branch to branch metadata");
    assert_eq!(feature_entry.parent.as_deref(), Some("main"));

    drop(feature);
    assert_main_db_missing_symbol(
        &fixture,
        "untracked_only",
        "auto-tracked branch sync must not index branch files into main DB",
    )
    .await;
}

#[tokio::test]
async fn fallback_writes_are_refused_by_all_sync_entry_points() {
    let (fixture, fallback) = open_detached_fallback_project().await;

    let err = fallback
        .sync()
        .await
        .expect_err("sync should refuse fallback writes");
    assert_fallback_write_refused("sync", err);

    let err = match fallback.index_all().await {
        Ok(_) => panic!("full index should refuse fallback writes"),
        Err(err) => err,
    };
    assert_fallback_write_refused("full index", err);

    let stale_files = ["src/detached_only.rs".to_string()];
    let err = fallback
        .sync_if_stale(&stale_files)
        .await
        .expect_err("stale sync should refuse fallback writes");
    assert_fallback_write_refused("stale sync", err);

    let err = fallback
        .sync_if_stale_silent(&stale_files)
        .await
        .expect_err("silent stale sync should refuse fallback writes");
    assert_fallback_write_refused("silent stale sync", err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &fixture,
        "detached_only",
        "fallback write attempts must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn detached_linked_worktree_uses_worktree_local_index() {
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    fs::create_dir_all(repo.root().join("src")).unwrap();
    fs::write(repo.root().join("src/lib.rs"), "pub fn indexed_on_main() {}\n").unwrap();
    repo.commit_all("initial commit");

    let project = profile.enroll_indexed(repo.root()).await.close().await;
    let linked = profile.scratch().join("linked-detached-worktree");
    let linked = repo.detached_worktree(&linked);
    fs::write(
        linked.join("src/worktree_only.rs"),
        "pub fn worktree_only() {}\n",
    )
    .unwrap();

    // A detached linked worktree collapses onto the primary project identity,
    // so reopen through the same profile options rather than synthesizing a
    // second standalone profile at the worktree path.
    let worktree = TraceDecay::init_with_options(&linked, project.open_options())
        .await
        .unwrap();
    worktree.index_all().await.unwrap();
    drop(worktree);

    let reopened = TraceDecay::open_with_options(&linked, project.open_options())
        .await
        .unwrap();
    assert_eq!(reopened.active_branch(), None);
    assert_eq!(reopened.serving_branch(), None);
    assert!(!reopened.is_fallback());
    assert!(
        !reopened
            .search("worktree_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "detached linked worktree should read its own index"
    );
}

#[tokio::test]
async fn add_branch_tracking_copies_from_nearest_tracked_ancestor() {
    let fixture = BranchSafetyProject::indexed_on_main().await;
    let open_options = fixture.open_options();

    // Seed persisted metadata on main before branching.
    {
        let main = fixture.reopen().await;
        main.set_tokens_saved(111).await.unwrap();
        main.checkpoint().await.unwrap();
        drop(main);
    }

    fixture.repo.run(&["checkout", "-b", "feature/parent"]);
    fs::write(
        fixture.root().join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    fixture.repo.commit_all("feature commit");

    let feature_outcome = TraceDecay::add_branch_tracking_with_options(
        fixture.root(),
        "feature/parent",
        open_options.clone(),
    )
    .await
    .unwrap();
    assert_eq!(feature_outcome, BranchAddOutcome::Added);

    let feature_cg = TraceDecay::open_with_options(fixture.root(), open_options.clone())
        .await
        .unwrap();
    feature_cg.set_tokens_saved(777).await.unwrap();
    feature_cg.checkpoint().await.unwrap();
    drop(feature_cg);

    fixture.repo.run(&["checkout", "-b", "topic/child"]);
    fs::write(
        fixture.root().join("src/topic_only.rs"),
        "pub fn topic_only() {}\n",
    )
    .unwrap();
    fixture.repo.commit_all("topic commit");

    let topic_outcome = TraceDecay::add_branch_tracking_with_options(
        fixture.root(),
        "topic/child",
        open_options.clone(),
    )
    .await
    .unwrap();
    assert_eq!(topic_outcome, BranchAddOutcome::Added);

    let meta = load_branch_meta(fixture.data_root()).unwrap();
    let topic_entry = meta
        .branches
        .get("topic/child")
        .expect("topic branch should be recorded in branch metadata");
    assert_eq!(topic_entry.parent.as_deref(), Some("feature/parent"));

    let topic_cg = fixture.project.open_branch("topic/child").await;
    assert_eq!(
        topic_cg.get_tokens_saved().await.unwrap(),
        777,
        "new branch DB should inherit the nearest tracked ancestor's persisted metadata"
    );
    assert!(
        !topic_cg
            .search("feature_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "topic branch DB should include symbols carried forward from the tracked ancestor"
    );
    assert!(
        !topic_cg.search("topic_only", 10).await.unwrap().is_empty(),
        "new branch tracking should still sync the current branch's own files"
    );
}

#[tokio::test]
async fn add_branch_tracking_refuses_corrupt_metadata_without_overwriting() {
    let fixture = BranchSafetyProject::indexed_on_main().await;
    let open_options = fixture.open_options();

    let meta_path = fixture.data_root().join("branch-meta.json");
    fs::write(&meta_path, b"{not valid json").unwrap();

    fixture.repo.run(&["checkout", "-b", "feature/corrupt-meta"]);
    fs::write(
        fixture.root().join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    fixture.repo.commit_all("feature commit");

    let err = TraceDecay::add_branch_tracking_with_options(
        fixture.root(),
        "feature/corrupt-meta",
        open_options,
    )
    .await
    .expect_err("corrupt metadata must stop branch tracking instead of being replaced");

    assert!(
        err.to_string().contains("corrupt branch metadata"),
        "unexpected error: {err}"
    );
    assert_eq!(
        fs::read(&meta_path).unwrap(),
        b"{not valid json",
        "failed branch add must preserve the original corrupt metadata for repair"
    );
}

#[test]
fn cli_branch_add_refuses_corrupt_metadata_without_overwriting() {
    let profile = TestProfile::acquire_blocking();
    let repo = GitFixture::primary(profile.path("project"));
    fs::create_dir_all(repo.root().join("src")).unwrap();
    fs::write(repo.root().join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    repo.commit_all("initial commit");

    let _daemon = common::spawn_tracedecay_daemon(profile.home());
    let mut init_command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut init_command, profile.home());
    let init = init_command
        .arg("init")
        .arg(repo.root())
        .output()
        .expect("tracedecay init");
    assert!(
        init.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    // CLI init enrolls through the daemon; read the layout the init wrote
    // rather than re-resolving against a second profile.
    let layout = tracedecay::storage::resolve_layout_for_current_profile(repo.root())
        .unwrap_or_else(|err| panic!("failed to resolve CLI-initialized layout: {err}"));
    let meta_path = layout.data_root.join("branch-meta.json");
    fs::write(&meta_path, b"{not valid json").unwrap();

    repo.run(&["checkout", "-b", "feature/corrupt-meta"]);
    fs::write(
        repo.root().join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    repo.commit_all("feature commit");

    let mut branch_add_command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut branch_add_command, profile.home());
    let output = branch_add_command
        .args(["branch", "add", "feature/corrupt-meta", "--path"])
        .arg(repo.root())
        .output()
        .expect("tracedecay branch add");

    assert!(
        !output.status.success(),
        "corrupt metadata must fail CLI branch add\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("corrupt branch metadata"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&meta_path).unwrap(),
        b"{not valid json",
        "failed CLI branch add must preserve corrupt metadata for repair"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cli_branch_add_uses_managed_daemon_as_the_only_database_writer() {
    // Daemon CLI tests must not keep a HostAdmission registry mounted: that
    // holds the profile global.db writer lease and the spawned daemon cannot
    // mount the same store. Use the profile only for isolation + open options.
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    fs::create_dir_all(repo.root().join("src")).unwrap();
    fs::write(repo.root().join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    repo.commit_all("initial commit");

    let open_options = profile.open_options();
    let main = TraceDecay::init_with_options(repo.root(), open_options)
        .await
        .unwrap();
    main.index_all().await.unwrap();
    let data_root = main.store_layout().data_root.clone();
    drop(main);

    repo.run(&["checkout", "-b", "feature/daemon-owned"]);
    fs::write(
        repo.root().join("src/daemon_owned.rs"),
        "pub fn daemon_owned() {}\n",
    )
    .unwrap();
    repo.commit_all("feature commit");

    let daemon_socket = common::daemon_socket_path(profile.home());
    let _daemon = common::spawn_tracedecay_daemon(profile.home());
    let authority: Value = serde_json::from_slice(
        &fs::read(profile.home().join(".tracedecay/daemon-authority.json"))
            .expect("read isolated daemon authority record"),
    )
    .expect("parse isolated daemon authority record");
    let daemon_pid = authority["pid"]
        .as_u64()
        .expect("daemon authority record should contain its PID");

    let branch_add = common::tracedecay_command_with_home(profile.home())
        .current_dir(repo.root())
        .env("TRACEDECAY_DAEMON_SOCKET", &daemon_socket)
        .args(["branch", "add", "feature/daemon-owned", "--path"])
        .arg(repo.root())
        .output()
        .expect("tracedecay branch add through the isolated daemon");
    assert!(
        branch_add.status.success(),
        "daemon-routed branch add should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&branch_add.stdout),
        String::from_utf8_lossy(&branch_add.stderr)
    );
    let branch_add_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&branch_add.stdout),
        String::from_utf8_lossy(&branch_add.stderr)
    );
    assert!(
        !branch_add_text.contains("database access is restricted to the elected managed daemon"),
        "the CLI must proxy branch add to the daemon instead of attempting local database authority:\n{branch_add_text}"
    );

    let meta = load_branch_meta(&data_root).expect("load branch metadata");
    let branch_entry = meta
        .branches
        .get("feature/daemon-owned")
        .expect("daemon branch add should record the feature branch");
    assert_eq!(branch_entry.parent.as_deref(), Some("main"));
    let branch_db = data_root.join(&branch_entry.db_file);
    assert!(
        branch_db.is_file(),
        "daemon branch add should create the recorded branch store at {}",
        branch_db.display()
    );

    let project_arg = repo.root().to_string_lossy().to_string();
    let runtime_output = common::tracedecay_command_with_home(profile.home())
        .current_dir(repo.root())
        .env("TRACEDECAY_DAEMON_SOCKET", &daemon_socket)
        .args([
            "tool",
            "--project",
            &project_arg,
            "runtime",
            "--json",
            "--format",
            "json",
        ])
        .output()
        .expect("daemon-routed runtime inspection");
    assert!(
        runtime_output.status.success(),
        "daemon-routed runtime inspection should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&runtime_output.stdout),
        String::from_utf8_lossy(&runtime_output.stderr)
    );
    let runtime_tool_result: Value =
        serde_json::from_slice(&runtime_output.stdout).expect("runtime tool result JSON");
    let runtime: Value = runtime_tool_result["content"]
        .as_array()
        .expect("runtime tool result content")
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| {
            panic!("runtime tool result should contain a JSON payload: {runtime_tool_result}")
        });
    assert_eq!(
        runtime["database"]["db_path"].as_str(),
        Some(branch_db.to_string_lossy().as_ref()),
        "the daemon should be serving the branch store it created"
    );
    let writer_owner = &runtime["database"]["writer_owner"];
    assert_eq!(
        writer_owner["state"].as_str(),
        Some("active"),
        "the branch store must have an active writer owner: {writer_owner}"
    );
    assert_eq!(
        writer_owner["pid"].as_u64(),
        Some(daemon_pid),
        "the active exclusive writer must be the daemon recorded in daemon-authority.json: {writer_owner}"
    );
}
