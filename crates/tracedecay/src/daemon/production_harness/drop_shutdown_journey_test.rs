//! A test body that ends without `shutdown().await` drops its harness and
//! then stops its runtime. The composition must still release the session
//! runtime registry, or the registry, every store it mounted, and their
//! reader and writer threads stay alive for the rest of the test process.

use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::{initialize_project, tool_payload};
use tracedecay_runtime_core::storage::default_profile_sharded_layout;

/// True while another open file description holds the task lease exclusively.
/// The shared probe lock lasts only for the call.
fn task_lease_is_held(lease: &std::path::Path) -> bool {
    std::fs::File::open(lease).is_ok_and(|file| {
        matches!(
            file.try_lock_shared(),
            Err(std::fs::TryLockError::WouldBlock)
        )
    })
}

const RELEASE_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct CuratorSettlementPause {
    release: Mutex<Option<mpsc::Receiver<()>>>,
    entered: tokio::sync::Notify,
}

struct PauseCuratorSettlement(Arc<CuratorSettlementPause>);

impl<S> Layer<S> for PauseCuratorSettlement
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &tracing::span::Id, context: Context<'_, S>) {
        if context.span(id).unwrap().metadata().name() != "daemon.automation.effect.publish" {
            return;
        }
        let release = self.0.release.lock().unwrap().take();
        if let Some(release) = release {
            self.0.entered.notify_one();
            release
                .recv_timeout(RELEASE_DEADLINE)
                .expect("the test must release the accepted curator settlement");
        }
    }
}

#[tokio::test]
async fn shutdown_waits_for_accepted_curator_settlement_and_lock_release() {
    const CHILD: &str = "TRACEDECAY_TEST_CURATOR_SHUTDOWN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // The subscriber follows the real settlement onto a native blocking
        // thread. Isolate it from every other test in the parent process.
        let test_name = format!(
            "{}::shutdown_waits_for_accepted_curator_settlement_and_lock_release",
            module_path!().split_once("::").unwrap().1
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("run isolated curator shutdown journey");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "curator shutdown journey failed: {output:?}"
        );
        return;
    }

    let pause = Arc::new(CuratorSettlementPause::default());
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(PauseCuratorSettlement(Arc::clone(&pause))),
    )
    .expect("isolated curator settlement subscriber");
    let isolation = TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production harness");
    let registry = harness
        .resources
        .as_ref()
        .expect("open harness resources")
        .invocation
        .service
        .project_runtimes
        .clone();
    let server = harness.server(&project).expect("production curator server");
    let dashboard_root = server.cg().await.store_layout().dashboard_root.clone();
    drop(server);

    let (release, release_rx) = mpsc::channel();
    *pause.release.lock().unwrap() = Some(release_rx);
    let response = harness
        .call_tool(
            &project,
            "tracedecay_fact_store_curate",
            json!({
                "fact_review_limit": 7,
                "min_confidence_millionths": 500_000,
                "format": "json"
            }),
        )
        .await
        .expect("curator request");
    let started = tool_payload(&response);
    assert_eq!(started["outcome"]["value"]["payload"]["state"], "started");
    let run_id = started["request_id"].as_str().expect("curator run id");
    tokio::time::timeout(RELEASE_DEADLINE, pause.entered.notified())
        .await
        .expect("the accepted curator reaches durable publication");
    let task_lock = dashboard_root.join("automation_locks/memory_curator.lease");
    assert!(
        task_lease_is_held(&task_lock),
        "the pending settlement still owns its task lock"
    );

    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    registry.arm_shutdown_drain_waiting(drain_waiting);
    let mut shutdown = tokio::spawn(harness.shutdown());
    tokio::select! {
        result = &mut shutdown => panic!(
            "shutdown returned before accepted curator settlement finished: {result:?}"
        ),
        waiting = tokio::time::timeout(RELEASE_DEADLINE, drain_is_waiting) => {
            waiting.expect("shutdown reaches the accepted-work drain")
                .expect("project runtime drain remains owned");
        }
    }
    assert!(!shutdown.is_finished());
    assert!(
        task_lease_is_held(&task_lock),
        "shutdown must preserve the settlement's lock"
    );
    release.send(()).expect("release curator settlement");
    tokio::time::timeout(RELEASE_DEADLINE, shutdown)
        .await
        .expect("settled curator releases shutdown")
        .expect("shutdown task");

    assert!(
        !task_lease_is_held(&task_lock),
        "shutdown must join exact task-lock release"
    );
    let ledger = std::fs::read_to_string(dashboard_root.join("automation_runs.jsonl"))
        .expect("settled automation ledger");
    let record: Value = ledger
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["run_id"] == run_id)
        .expect("the admitted curator settled before shutdown returned");
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "nothing_to_review");
}

#[tokio::test]
async fn a_failed_later_mount_releases_the_whole_composition_before_returning() {
    let isolation = TempDir::new().expect("production harness isolation");
    let first = isolation.path().join("first");
    let refused = isolation.path().join("refused");
    initialize_project(&first);
    initialize_project(&refused);
    let retired = refused.join(".tracedecay");
    std::fs::create_dir(&retired).expect("retired checkout layout");
    std::fs::write(retired.join("config.json"), "{}").expect("retired configuration");

    let error = match ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [first.clone(), refused.clone()],
    )
    .await
    {
        Ok(harness) => {
            harness.shutdown().await;
            panic!("the retired checkout layout must refuse the second mount");
        }
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected the retired-layout refusal: {error:?}"));
    assert_eq!(authority, "project store");
    assert!(reason.contains("retired checkout-local layout"), "{reason}");

    let profile = isolation.path().join("profile");
    let first_layout = default_profile_sharded_layout(&first, &profile)
        .expect("the first mount enrolled its exact project identity");
    let stores = [
        profile.join("global.db"),
        profile.join("user-sessions.db"),
        first_layout.data_root.join("sessions.db"),
        first_layout.graph_db_path,
    ];
    for store in &stores {
        assert!(
            store.is_file(),
            "the first mount opened {}",
            store.display()
        );
        let mut wal = store.as_os_str().to_owned();
        wal.push("-wal");
        let bytes =
            std::fs::metadata(std::path::PathBuf::from(wal)).map_or(0, |metadata| metadata.len());
        assert_eq!(
            bytes,
            0,
            "failed-open rollback must close the writer for {} before returning",
            store.display()
        );
    }

    std::fs::remove_file(retired.join("config.json")).expect("remove the fixture refusal");
    std::fs::remove_dir(retired).expect("remove the empty retired fixture directory");
    let harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [first.clone(), refused.clone()],
    )
    .await
    .expect("both exact projects reopen after failed-open rollback");
    assert!(harness.server(&first).is_ok());
    assert!(harness.server(&refused).is_ok());
    harness.shutdown().await;
}

#[test]
fn a_harness_dropped_as_its_runtime_stops_releases_its_session_registry() {
    let isolation = TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let registry = runtime.block_on(async {
        let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project])
            .await
            .expect("production harness");
        let registry = harness
            .resources
            .as_ref()
            .expect("open harness resources")
            .store_administration
            .session_runtime_registry()
            .await
            .expect("session runtime registry");
        Arc::downgrade(&registry)
    });
    drop(runtime);

    let deadline = Instant::now() + RELEASE_DEADLINE;
    while registry.strong_count() > 0 {
        assert!(
            Instant::now() < deadline,
            "the dropped harness still holds its session runtime registry after {RELEASE_DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
