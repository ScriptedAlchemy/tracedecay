use super::{
    McpServer, StalenessBannerInputs, format_index_age_phrase, staleness_banner,
    tool_error_response,
};
use crate::config::PinnedUserDataDir;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
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
        refresh_running: false,
        refreshed_recently: true,
    });
    assert!(banner.is_none(), "expected no banner, got {banner:?}");
}

#[test]
fn banner_instructs_manual_sync_when_auto_sync_disabled() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: false,
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
        server.startup_catch_up.dispatch_claimed(),
        "startup catch-up should have been dispatched by new_with_dbs"
    );
    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(30))
            .await,
        "startup catch-up should settle"
    );
    assert!(server.startup_catch_up_done());
    assert!(server.transcript_ingest_done());

    // The claim is one-shot for the life of the server: a hypothetical
    // second new_with_dbs-style dispatch is refused even now that the
    // machine has settled.
    assert!(
        !server.startup_catch_up.try_claim_dispatch(),
        "startup catch-up dispatch must stay claimed (runs at most once)"
    );
    // The refused claim must not have dragged the settled machine back into
    // a pending phase.
    assert!(server.startup_catch_up_done());
    assert!(server.transcript_ingest_done());
}

/// A dispatched catch-up must never read as settled before it runs. This is
/// the ordering hazard the old default-`true` completion flags carried: the
/// dispatch site had to pre-clear them in a separate store, and any waiter
/// that landed in between observed a false "ready".
#[tokio::test]
async fn a_claimed_dispatch_is_never_observed_as_settled() {
    use crate::mcp::server::lifecycle::StartupCatchUpMachineV1;

    let machine = StartupCatchUpMachineV1::default();
    // Undispatched machines are ready: nothing will ever run.
    assert!(machine.sync_phase_settled_for_test());
    assert!(machine.ingest_phase_settled_for_test());

    assert!(machine.try_claim_dispatch());
    assert!(machine.dispatch_claimed());
    // Claiming the dispatch is itself the transition into `Syncing`, so
    // there is no window in which both phases read settled.
    assert!(!machine.sync_phase_settled_for_test());
    assert!(!machine.ingest_phase_settled_for_test());

    machine.enter_ingesting_for_test();
    assert!(machine.sync_phase_settled_for_test());
    assert!(!machine.ingest_phase_settled_for_test());

    machine.settle_for_test();
    assert!(machine.sync_phase_settled_for_test());
    assert!(machine.ingest_phase_settled_for_test());
}

/// Shutdown must leave both phases readable as settled, so a waiter can
/// never block on a task that was just aborted.
#[tokio::test]
async fn a_cancelled_machine_reads_as_settled_and_refuses_further_phases() {
    use crate::mcp::server::lifecycle::StartupCatchUpMachineV1;

    let machine = StartupCatchUpMachineV1::default();
    assert!(machine.try_claim_dispatch());
    machine.mark_cancelled_for_test();
    assert!(machine.sync_phase_settled_for_test());
    assert!(machine.ingest_phase_settled_for_test());

    // A late in-flight task settling after shutdown must not resurrect the
    // machine into a non-terminal phase.
    machine.enter_ingesting_for_test();
    machine.settle_for_test();
    assert!(machine.sync_phase_settled_for_test());
    assert!(machine.ingest_phase_settled_for_test());
    assert!(!machine.try_claim_dispatch());
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
