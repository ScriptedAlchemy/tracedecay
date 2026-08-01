use super::{
    DatabaseOwnerReconciler, McpServer, McpServerConstructionContext, StalenessBannerInputs,
    format_index_age_phrase, staleness_banner, tool_error_response,
};
use crate::config::PinnedUserDataDir;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

struct FreshnessRuntime {
    registry: DaemonSessionRuntimeRegistryV1,
    _scope: crate::db::DaemonDatabaseScope,
}

impl FreshnessRuntime {
    async fn open(profile_root: &std::path::Path) -> Self {
        std::fs::create_dir_all(profile_root).expect("freshness profile root");
        crate::storage::set_private_dir_permissions(profile_root)
            .expect("restrict freshness profile root");
        let identity = crate::daemon::profile_identity::load_or_create(profile_root)
            .expect("freshness profile identity");
        let scope = crate::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )
        .expect("freshness daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("freshness session runtime registry");
        Self {
            registry,
            _scope: scope,
        }
    }

    async fn profile_database(&self) -> Arc<RegisteredGlobalDb> {
        self.registry
            .profile_database()
            .await
            .expect("registered freshness profile database")
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

struct FreshnessFixtureAuthority {
    _pin: PinnedUserDataDir,
    _runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
}

async fn init_indexed_repo() -> (TraceDecay, TempDir, FreshnessFixtureAuthority) {
    let pin = PinnedUserDataDir::new();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join(".gitignore"), ".tracedecay/\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let (cg, runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(root, "project.mcp-freshness")
            .await
            .expect("init");
    cg.index_all().await.expect("index");
    (
        cg,
        dir,
        FreshnessFixtureAuthority {
            _pin: pin,
            _runtime: runtime,
        },
    )
}

#[tokio::test]
async fn branch_drift_reconciles_database_owner_before_returning() {
    let (cg, dir, fixture_authority) = init_indexed_repo().await;
    let root = dir.path();
    cg.checkpoint().await.unwrap();
    let layout = cg.store_layout().clone();
    drop(cg);

    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    crate::branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();
    std::fs::create_dir_all(layout.data_root.join("branches")).unwrap();
    std::fs::copy(
        &layout.graph_db_path,
        layout.data_root.join("branches/feature.db"),
    )
    .unwrap();

    git(root, &["checkout", "-q", "-b", "feature"]);
    git(root, &["checkout", "-q", "main"]);
    let main = fixture_authority
        ._runtime
        .open_project_graph_for_test(root, crate::tracedecay::TraceDecayOpenOptions::default())
        .await
        .unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let callback: DatabaseOwnerReconciler = {
        let observed = Arc::clone(&observed);
        Arc::new(move |fresh| {
            let observed = Arc::clone(&observed);
            Box::pin(async move {
                observed.lock().unwrap().push(fresh.db_path());
            })
        })
    };
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(main, None).with_database_owner_reconciler(callback),
    )
    .await;

    git(root, &["checkout", "-q", "feature"]);
    let fresh = server.reopen_if_branch_drifted().await;

    assert_eq!(fresh.serving_branch(), Some("feature"));
    assert_eq!(observed.lock().unwrap().as_slice(), &[fresh.db_path()]);
    server.shutdown().await;
}

// ---- D7 pure-logic banner tests (test c) --------------------------

#[test]
fn format_index_age_phrase_preserves_shape() {
    // 2h 5m
    assert_eq!(format_index_age_phrase(2 * 3600 + 5 * 60), "2h 5m");
    // 1d 3h
    assert_eq!(format_index_age_phrase(27 * 3600), "1d 3h");
}

#[test]
fn banner_says_refresh_in_progress_when_auto_sync_on() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: true,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("refresh in progress"), "{banner}");
    assert!(!banner.contains("tracedecay sync"), "{banner}");
    assert!(!banner.starts_with("WARNING"), "{banner}");
}

#[test]
fn banner_says_scheduled_when_auto_sync_on_and_idle() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("refresh scheduled"), "{banner}");
    assert!(!banner.contains("tracedecay sync"), "{banner}");
}

#[test]
fn banner_suppressed_shortly_after_refresh() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: true,
    });
    assert!(banner.is_none(), "expected no banner, got {banner:?}");
}

#[test]
fn banner_instructs_manual_sync_only_on_fallback_store() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: true,
        refresh_running: true,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.starts_with("WARNING"), "{banner}");
    assert!(banner.contains("Run `tracedecay sync`"), "{banner}");
}

#[test]
fn banner_instructs_manual_sync_when_auto_sync_disabled() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: false,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("Run `tracedecay sync`"), "{banner}");
}

// ---- D1: startup catch-up runs exactly once (test b) --------------

#[tokio::test]
async fn startup_catch_up_spawned_once_per_server() {
    let (cg, _dir, _pin) = init_indexed_repo().await;
    let server = McpServer::new(cg, None).await;
    // The D1 spawn should have claimed the one-shot flag.
    assert!(
        server.startup_catch_up_started.load(Ordering::Acquire),
        "startup catch-up should have been dispatched by new_with_dbs"
    );
    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(30))
            .await,
        "startup catch-up should settle"
    );
    assert!(server.startup_catch_up_done());

    // The one-shot flag is already claimed; a hypothetical second
    // new_with_dbs-style dispatch would be refused by the same
    // compare_exchange. Assert the flag can't be re-claimed.
    assert!(
        server
            .startup_catch_up_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err(),
        "startup catch-up flag must stay claimed (runs at most once)"
    );
}

#[tokio::test]
async fn direct_server_keeps_configured_profile_root_with_overridden_registry_db() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let profile_root = crate::config::user_data_dir().expect("configured profile root");
    let override_root = dir.path().join("registry-override");
    let runtime = FreshnessRuntime::open(&override_root).await;
    let registry = runtime.profile_database().await;

    let server = McpServer::new_with_dbs(cg, None, None, Some(registry), true).await;

    assert_eq!(server.profile_root.as_deref(), Some(profile_root.as_path()));
    assert_ne!(
        server.profile_root.as_deref(),
        server
            .registry_db
            .as_deref()
            .and_then(|db| db.db_path().parent())
    );
}

#[test]
fn hook_runtime_failures_keep_structured_retry_data_at_json_rpc_boundary() {
    let error = crate::errors::TraceDecayError::hook_runtime(
        "observation_cursor_conflict",
        true,
        "Claude observation store operation failed",
    );

    let response = tool_error_response(serde_json::json!(7), "tracedecay_hook_runtime", &error);
    let data = response.error.unwrap().data.unwrap();

    assert_eq!(data["reason_code"], "observation_cursor_conflict");
    assert_eq!(data["retryable"], true);
    assert_eq!(data["detail"], "Claude observation store operation failed");
}

// ---- ledger settle is bounded when a recorder task wedges ---------

// A dedicated multi-thread runtime keeps the timer driver off the same worker
// that runs the server's startup catch-up sync, so the bound is honored
// promptly regardless of machine load.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_writes_settled_is_bounded_when_a_write_wedges() {
    let (cg, _dir, _pin) = init_indexed_repo().await;
    let mut config = crate::config::load_config(cg.project_root()).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(cg.project_root(), &config).expect("disable unrelated catch-up");
    let server = McpServer::new(cg, None).await;

    // Inject a never-completing observed ledger write via the same accounting
    // the production path uses. Without a bound, awaiting settlement would hang
    // forever (the defect this guards against).
    server.spawn_wedged_ledger_write_for_test();

    // Wrap in an outer wall-clock guard: if the bound were ever ignored the
    // call would hang, so an elapsed outer timeout is itself the failure
    // signal (a plain assertion could never fire on a hung await).
    let bounded = tokio::time::timeout(
        Duration::from_secs(30),
        server.ledger_writes_settled_within(Duration::from_millis(150)),
    )
    .await
    .expect("bounded settle must return, never hang on a wedged write");

    assert!(
        !bounded,
        "a wedged ledger write must be reported as un-settled"
    );
}

// ---- D4: sync-on-read never blocks + single-flight (tests a, d) ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_refresh_is_non_blocking_and_single_flighted() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let mut config = crate::config::load_config(&root).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&root, &config).expect("save config");
    let server = McpServer::new(cg, None).await;
    // Reset the read cooldown so the next spawn is eligible regardless of
    // any startup timing.
    server
        .last_background_refresh_at
        .store(0, Ordering::Release);
    server
        .background_refresh_running
        .store(false, Ordering::Release);

    // Make the tree stale: a new committed source file, as the
    // diff-scoped refresh contract tracks git history.
    std::fs::write(root.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "add b"]);

    let cg_snapshot = server.cg_snapshot().await;

    // First read-refresh: returns immediately (we never await the sync).
    // Assert it does not block by bounding the call duration well under a
    // real sync (~hundreds of ms); the spawn does the work off-thread.
    let start = std::time::Instant::now();
    server.maybe_spawn_read_refresh(&cg_snapshot, &cg_snapshot.branch_memo());
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "maybe_spawn_read_refresh must not block on the sync (took {elapsed:?})"
    );
    // The refresh should have been claimed (running flag set) — proving a
    // task was spawned rather than run inline.
    // (It may already have finished on a very fast machine; in that case
    // the cooldown stamp still advanced, which we assert below.)
    assert_ne!(
        server.last_background_refresh_at.load(Ordering::Acquire),
        0,
        "cooldown stamp must advance when a refresh is kicked"
    );

    // Second immediate read-refresh: single-flighted. Because the cooldown
    // stamp just advanced (and/or a refresh is running), no second task is
    // spawned. We verify by confirming the stamp does not change to a new
    // value on a back-to-back call within the cooldown window.
    let stamp_after_first = server.last_background_refresh_at.load(Ordering::Acquire);
    server.maybe_spawn_read_refresh(&cg_snapshot, &cg_snapshot.branch_memo());
    let stamp_after_second = server.last_background_refresh_at.load(Ordering::Acquire);
    assert_eq!(
        stamp_after_first, stamp_after_second,
        "second read within cooldown must not re-kick (single-flight)"
    );
}
