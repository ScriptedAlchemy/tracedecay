//! PROBE (measurement only, not committed): per-phase registry open cost.

use std::path::{Path, PathBuf};
use std::time::Instant;

use tempfile::TempDir;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::ProjectId;

use super::DaemonSessionRuntimeRegistryV1;

fn enrolled_root(base: &Path, project_id: &ProjectId) -> PathBuf {
    let root = base.join(project_id.as_str());
    std::fs::create_dir_all(&root).expect("project root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(&root, project_id.as_str())
        .expect("project enrollment");
    root
}

struct Phase {
    started: Instant,
    counts: Vec<(&'static str, u64)>,
}

impl Phase {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            counts: tracedecay_runtime_core::db::engine_probe_counts(),
        }
    }

    fn end(self, round: usize, name: &str) {
        let elapsed = self.started.elapsed();
        let after = tracedecay_runtime_core::db::engine_probe_counts();
        let deltas = after
            .iter()
            .zip(self.counts.iter())
            .map(|((n, a), (_, b))| format!("{n}={}", a - b))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "PROBE round={round} phase={name} ms={:.1} {deltas}",
            elapsed.as_secs_f64() * 1000.0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_registry_open_cost() {
    for round in 0..4 {
        let temp = TempDir::new().expect("probe root");
        let profile_root = temp.path().join("profile");
        let project_id = ProjectId::new("project.open-cost-probe").expect("project id");
        let project_root = enrolled_root(temp.path(), &project_id);
        let _scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            53,
            "open cost probe",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");

        let phase = Phase::start();
        let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("registry open");
        phase.end(round, "registry_open_fresh");

        let phase = Phase::start();
        let profile_memory = registry.profile_memory().await.expect("profile memory");
        phase.end(round, "profile_memory");
        let phase = Phase::start();
        let profile_sessions = registry.profile_sessions().await.expect("profile sessions");
        phase.end(round, "profile_sessions");
        let phase = Phase::start();
        let project_memory = registry
            .project_memory(project_id.clone(), [project_root.clone()])
            .await
            .expect("project memory");
        phase.end(round, "project_memory");
        let phase = Phase::start();
        let project_sessions = registry
            .project_sessions(project_id.clone(), [project_root.clone()])
            .await
            .expect("project sessions");
        phase.end(round, "project_sessions");

        let phase = Phase::start();
        drop((
            profile_memory,
            profile_sessions,
            project_memory,
            project_sessions,
        ));
        shutdown(&registry).await;
        drop(registry);
        phase.end(round, "shutdown");

        let phase = Phase::start();
        let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("registry reopen");
        phase.end(round, "registry_open_warm");
        let phase = Phase::start();
        let project_sessions = registry
            .project_sessions(project_id.clone(), [project_root.clone()])
            .await
            .expect("project sessions reopen");
        phase.end(round, "project_sessions_warm");
        drop(project_sessions);
        shutdown(&registry).await;
    }
}

async fn shutdown(registry: &DaemonSessionRuntimeRegistryV1) {
    registry.shutdown_terminal_tasks().await.expect("terminal");
    registry.cancel_memory_graph_reconciliation_tasks();
    registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("reconciliation");
    registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("close");
}
