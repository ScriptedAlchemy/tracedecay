//! Fixed terminal GitHub stack delivery housekeeping through the daemon's
//! maintenance owner, on a mounted project whose agent jobs and optional
//! retention policies are all disabled.

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::branch_admin::{StoreWriterClass, store_writer_scope};
use tracedecay_runtime_core::native_runtime::{self, NativeTask};

use tempfile::TempDir;
use tracedecay_automation_runtime::automation::config::AutomationConfigPatch;
use tracedecay_daemon_protocol::DaemonHandshake;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_maintenance::tick::{MaintenanceContinuation, MaintenanceTickOutcome};

use super::DaemonEngine;
use super::{
    apply_project_automation_patch_via_surface, enter_test_daemon_database_scope,
    initialize_test_project, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::maintenance::{BranchStoreGcCadenceV1, MaintenanceCoordinator};

const STACK_PROJECT: &str = "project.terminal-housekeeping";
const STACK_RECIPIENT: &str = "actor.terminal-housekeeping";

/// A mounted project with every agent job disabled, its daemon engine, and a
/// maintenance owner started with every optional retention policy disabled.
struct DisabledAutomationDaemon {
    _dir: TempDir,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    engine: DaemonEngine,
    server: Arc<crate::mcp::McpServer>,
    coordinator: MaintenanceCoordinator,
    continuation: tokio::sync::Mutex<Option<MaintenanceContinuation>>,
}

impl DisabledAutomationDaemon {
    async fn open() -> Self {
        Self::open_with_projects(1).await
    }

    async fn open_with_projects(project_count: usize) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        initialize_test_project(&project, &client_identity).await;
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity,
            ..test_handshake_defaults()
        };
        let mut extra_handshakes = Vec::new();
        for ordinal in 1..project_count {
            let root = project.join(format!("project-{ordinal}"));
            std::fs::create_dir_all(root.join("src")).expect("project source directory");
            std::fs::write(root.join("src/lib.rs"), "pub fn marker() {}\n")
                .expect("project source");
            initialize_test_project(&root, &handshake.client_identity).await;
            extra_handshakes.push(DaemonHandshake {
                project_path: Some(root),
                ..handshake.clone()
            });
        }
        let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
        let database_scope = enter_test_daemon_database_scope(
            &handshake.client_identity.profile_root,
            "terminal-housekeeping",
        );
        let server = apply_project_automation_patch_via_surface(
            &engine,
            &handshake,
            AutomationConfigPatch {
                enabled: Some(false),
                ..AutomationConfigPatch::default()
            },
        )
        .await;
        for extra in extra_handshakes {
            apply_project_automation_patch_via_surface(
                &engine,
                &extra,
                AutomationConfigPatch {
                    enabled: Some(false),
                    ..AutomationConfigPatch::default()
                },
            )
            .await;
        }
        let graph = server.cg_snapshot().await;
        assert!(
            !crate::daemon::scheduler::automation_scheduler_has_work_for_project(graph.as_ref())
                .await
                .expect("automation admission"),
            "no agent job is configured, so no project automation scheduler is admitted"
        );
        drop(graph);

        let retention = disabled_retention();
        let coordinator = MaintenanceCoordinator::spawn(
            handshake.client_identity.profile_root.clone(),
            engine.store_administration.clone(),
            tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(
                1,
            ),
            retention,
            BranchStoreGcCadenceV1 { branch_gc_days: 30 },
        )
        .await;
        Self {
            _dir: dir,
            _database_scope: database_scope,
            engine,
            server,
            coordinator,
            continuation: tokio::sync::Mutex::new(None),
        }
    }

    fn shard(&self) -> tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        self.server
            .project_session_db()
            .expect("project session shard")
    }

    async fn tick(&self) -> MaintenanceTickOutcome {
        let mut continuation = self.continuation.lock().await;
        let outcome = terminal_tick(
            &self.coordinator,
            &self.engine.store_administration,
            *continuation,
        )
        .await;
        *continuation = outcome.continuation();
        outcome
    }

    async fn close(self) {
        tracedecay_runtime_core::native_runtime::timeout(
            std::time::Duration::from_secs(10),
            self.coordinator.shutdown(),
        )
        .await
        .expect("shutdown joins the terminal housekeeping owner");
    }
}

fn disabled_retention() -> tracedecay_configuration::RetentionConfig {
    let mut retention = tracedecay_configuration::RetentionConfig::default();
    retention.session_lcm.enabled = false;
    retention.observation.enabled = false;
    retention.orphan_store_gc_days = None;
    retention.compaction = None;
    retention
        .store_soft_budgets_bytes
        .insert("sessions.db".to_owned(), 1);
    retention
}

async fn terminal_tick(
    owner: &MaintenanceCoordinator,
    administration: &crate::daemon::branch_admin::StoreAdministration,
    continuation: Option<MaintenanceContinuation>,
) -> MaintenanceTickOutcome {
    let profile = administration
        .registered_profile_database()
        .await
        .expect("profile authority");
    owner
        .run_tick(
            profile.db_path().parent().expect("profile root"),
            &profile,
            administration,
            &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(
                1,
            ),
            &disabled_retention(),
            BranchStoreGcCadenceV1 { branch_gc_days: 30 },
            continuation,
        )
        .await
}

async fn seed_stack_signal(
    database: &RegisteredGlobalDb,
    signal_id: &str,
    observed_at_micros: i64,
    settled: bool,
) {
    database
        .append_github_stack_signal(
            tracedecay_global_db::GitHubStackSignalRecordV1 {
                project_id: STACK_PROJECT.to_owned(),
                signal_id: signal_id.to_owned(),
                scope_digest: "sha256:terminal-housekeeping-scope".to_owned(),
                repository_id: "repository.terminal-housekeeping".to_owned(),
                watermark_id: "watermark.terminal-housekeeping".to_owned(),
                observed_at_micros,
                signal_json: format!(r#"{{"payload":"{signal_id}"}}"#),
            },
            vec![STACK_RECIPIENT.to_owned()],
        )
        .await
        .expect("persist stack signal");
    if settled {
        database
            .acknowledge_github_stack_host_delivery(STACK_PROJECT, signal_id, STACK_RECIPIENT)
            .await
            .expect("settle stack signal");
    }
}

async fn count_rows(database: &RegisteredGlobalDb, sql: &str) -> i64 {
    let mut rows = database
        .read_connection()
        .query(sql, ())
        .await
        .expect("count query");
    rows.next()
        .await
        .expect("count row")
        .expect("count result")
        .get::<i64>(0)
        .expect("count value")
}

async fn retired_signals(database: &RegisteredGlobalDb) -> i64 {
    count_rows(
        database,
        "SELECT COUNT(*) FROM github_stack_delivery_signals
         WHERE json_type(signal_json, '$.__tracedecay_retired_stack_sha256_v1') IS NOT NULL",
    )
    .await
}

async fn retirement_frontiers(database: &RegisteredGlobalDb) -> i64 {
    count_rows(
        database,
        "SELECT COUNT(*) FROM github_stack_delivery_retirement_frontier",
    )
    .await
}

async fn payload_present(database: &RegisteredGlobalDb, signal_id: &str) -> bool {
    database
        .github_stack_signal(STACK_PROJECT, signal_id)
        .await
        .expect("read stack signal")
        .is_some()
}

/// A mounted project's stack history lives in its own session shard. With
/// every agent job and optional policy disabled the fixed pass retires it
/// there; the profile database cannot stand in, and none of the full tick's
/// other retention work runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_settled_project_shard_signals_retire_with_all_automation_disabled() {
    let daemon = DisabledAutomationDaemon::open().await;
    let shard = daemon.shard();
    let profile = daemon
        .engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("profile database");
    assert_ne!(shard.db_path(), profile.db_path());

    let recent = tracedecay_contracts::now_micros().0;
    seed_stack_signal(&shard, "signal.expired-settled", 100, true).await;
    seed_stack_signal(&shard, "signal.expired-pending", 101, false).await;
    seed_stack_signal(&shard, "signal.recent-settled", recent, true).await;
    // Optional-detail observability pruning belongs to the full tick, not to
    // the fixed pass, so an expired row must survive it.
    shard
        .append_observability_event(&tracedecay_global_db::AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: "scope:terminal-housekeeping".to_owned(),
            session_id: None,
            timestamp: 0,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some("terminal-housekeeping:optional-marker".to_owned()),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(
                serde_json::json!({ "retention_class": "optional_local_detail30d" }).to_string(),
            ),
        })
        .await
        .expect("seed an expired optional observability detail");

    // Retiring only the profile shard is the wrong-shard cleanup.
    let wrong_shard =
        tracedecay_maintenance::retention::registered_store::run_github_stack_terminal_retention(
            profile.as_ref(),
            tracedecay_runtime_core::tracedecay::current_timestamp(),
        )
        .await
        .expect("profile-only retirement");
    assert_eq!(wrong_shard.retired, 0);
    assert_eq!(retired_signals(&shard).await, 0);

    assert_eq!(daemon.tick().await, MaintenanceTickOutcome::Complete);

    assert_eq!(retired_signals(&shard).await, 1);
    assert!(
        !payload_present(&shard, "signal.expired-settled").await,
        "the expired settled payload is compacted in the project shard"
    );
    assert!(payload_present(&shard, "signal.expired-pending").await);
    assert!(payload_present(&shard, "signal.recent-settled").await);
    assert_eq!(
        shard
            .query_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
                provider: Some("tracedecay-observability".to_owned()),
                project_id: Some("scope:terminal-housekeeping".to_owned()),
                limit: 10,
                ..tracedecay_global_db::AnalyticsEventQuery::default()
            })
            .await
            .expect("query optional marker")
            .len(),
        1,
        "the fixed pass runs none of the full tick's other retention work"
    );

    drop(shard);
    drop(profile);
    daemon.close().await;
}

/// Each admitted unit retires one page and releases admission. The same
/// durable frontier advances through continuation ticks and clears on drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_housekeeping_resumes_from_its_frontier_after_the_page_budget() {
    // Preserve the 1041 settled + 3 pending journey across page admissions.
    const FIRST_TICK: i64 = 64 * 16;
    const TAIL: i64 = 17;
    let daemon = DisabledAutomationDaemon::open().await;
    let shard = daemon.shard();
    for pending in 0..3 {
        seed_stack_signal(
            &shard,
            &format!("signal.pending-{pending}"),
            1 + pending,
            false,
        )
        .await;
    }
    for ordinal in 0..(FIRST_TICK + TAIL) {
        seed_stack_signal(
            &shard,
            &format!("signal.settled-{ordinal:05}"),
            1_000 + ordinal,
            true,
        )
        .await;
    }

    for _ in 0..64 {
        assert_eq!(
            daemon.tick().await,
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::StoreRetention)
        );
    }
    assert_eq!(retired_signals(&shard).await, FIRST_TICK);
    assert_eq!(
        retirement_frontiers(&shard).await,
        1,
        "an unfinished pass keeps its durable frontier"
    );

    assert_eq!(
        daemon.tick().await,
        MaintenanceTickOutcome::Continue(MaintenanceContinuation::StoreRetention)
    );
    assert_eq!(retired_signals(&shard).await, FIRST_TICK + 16);
    assert_eq!(daemon.tick().await, MaintenanceTickOutcome::Complete);
    assert_eq!(retired_signals(&shard).await, FIRST_TICK + TAIL);
    assert_eq!(
        retirement_frontiers(&shard).await,
        0,
        "a drained pass clears the frontier so settled history is revisited"
    );
    assert_eq!(
        count_rows(
            shard.as_ref(),
            "SELECT COUNT(*) FROM github_stack_delivery_recipients WHERE state = 'pending'",
        )
        .await,
        3,
        "undelivered signals are never retired by age"
    );

    drop(shard);
    daemon.close().await;
}

/// Hold the real SQLite writer in A after it has a terminal backlog, so its
/// admitted maintenance is suspended inside storage while B opens a writer.
#[test]
fn terminal_backlog_does_not_block_an_unrelated_store_writer() {
    native_runtime::block_on(async {
        let daemon = DisabledAutomationDaemon::open().await;
        let shard = daemon.shard();
        for ordinal in 0..33 {
            seed_stack_signal(&shard, &format!("signal.{ordinal:03}"), 100 + ordinal, true).await;
        }
        let held = shard.begin_write_transaction().await.expect("hold A SQLite writer");
        let owner = daemon.coordinator.clone();
        let administration = daemon.engine.store_administration.clone();
        let tick = NativeTask::spawn(async move {
            terminal_tick(&owner, &administration, None).await
        }).expect("native terminal tick");
        let a_scope = store_writer_scope(shard.db_path().parent().expect("A store root"), StoreWriterClass::Content);
        native_runtime::timeout(Duration::from_secs(10), async {
            while daemon.engine.store_administration.try_with_writer_in(a_scope.clone(), || async {}).await.is_some() {
                assert!(!tick.is_finished(), "tick must wait inside A storage");
                tokio::task::yield_now().await;
            }
        }).await.expect("maintenance admitted A");
        let other = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("terminal-writer-b").await;
        let b_scope = store_writer_scope(other.registered.db_path().parent().expect("B store root"), StoreWriterClass::Content);
        let progressed = native_runtime::timeout(Duration::from_secs(2),
            daemon.engine.store_administration.with_writer_in(b_scope, || async {
                other.registered.writer_connection().expect("B writer")
                    .execute_batch("CREATE TABLE unrelated_writer_progress (id INTEGER PRIMARY KEY) STRICT;
                                   INSERT INTO unrelated_writer_progress VALUES (1);").await.expect("B writes");
            })
        ).await;
        // Release A before asserting, so a failed interleaving proof still
        // settles its admitted owner and releases every fixture lease.
        held.rollback().await.expect("release A");
        let outcome = native_runtime::timeout(Duration::from_secs(10), tick.wait()).await.expect("terminal tick settled");
        assert!(matches!(outcome, native_runtime::NativeTaskOutcome::Completed(_)));
        assert!(progressed.is_ok(), "B must write while A is suspended with a terminal backlog");
        assert_eq!(retired_signals(&shard).await, 16, "one admission runs one bounded terminal page");
        assert_eq!(retirement_frontiers(&shard).await, 1);
        drop(shard);
        daemon.close().await;
    }).expect("native runtime");
}

/// The profile shard plus eight mounted projects exceeds one store window.
/// Unvisited shards must continue promptly without retaining their leases.
#[test]
fn terminal_housekeeping_bounds_a_mounted_store_window_and_reaches_its_tail() {
    native_runtime::block_on(async {
        let daemon = DisabledAutomationDaemon::open_with_projects(8).await;
        let databases = daemon
            .engine
            .store_administration
            .mounted_registered_session_databases()
            .await;
        assert_eq!(
            databases.len(),
            9,
            "eight projects and their profile shard are mounted"
        );
        for database in &databases {
            seed_stack_signal(database, "signal.window", 100, true).await;
        }
        assert_eq!(
            daemon.tick().await,
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::StoreRetention)
        );
        let mut retired = 0;
        for database in &databases {
            retired += retired_signals(database).await;
        }
        assert_eq!(retired, 8, "one tick admits at most eight store pages");
        assert_eq!(daemon.tick().await, MaintenanceTickOutcome::Complete);
        for database in &databases {
            assert_eq!(
                retired_signals(database).await,
                1,
                "every mounted shard is reached"
            );
        }
        drop(databases);
        daemon.close().await;
    })
    .expect("native runtime");
}
