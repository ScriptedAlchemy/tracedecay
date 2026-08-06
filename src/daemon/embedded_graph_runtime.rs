//! Daemon ownership for the one native Grafeo store mounted per project shard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use thiserror::Error;
use tracedecay_global_db::session_temporal::relations::{
    SessionRelationScope, open_persistent_session_relation_graph,
    persistent_session_relation_graph_path,
};
use tracedecay_graph_db::{GraphDb, GraphDbError};

const RELATION_EFFECT_PAGE_SIZE: usize = 64;
const RELATION_EFFECT_MAX_PAGES_PER_WAKE: usize = 8;
const RELATION_EFFECT_MAX_RUN_TIME: Duration = Duration::from_millis(500);
const RELATION_EFFECT_IDLE_RETRY: Duration = Duration::from_secs(1);
const LCM_HOST_EFFECT_MAX_ITEMS_PER_WAKE: usize = 4;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum EmbeddedGraphRuntimeError {
    #[error("embedded graph owner identity conflicts with an existing mount")]
    IdentityConflict,
    #[error("embedded graph store requires reset: {0}")]
    ResetRequired(String),
    #[error("embedded graph store is corrupt: {0}")]
    Corrupt(String),
    #[error("embedded graph store is unavailable: {0}")]
    Unavailable(String),
}

impl From<GraphDbError> for EmbeddedGraphRuntimeError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::ResetRequired { message } => Self::ResetRequired(message),
            GraphDbError::Corrupt { message } | GraphDbError::DurabilityUncertain { message } => {
                Self::Corrupt(message)
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("embedded graph is closed".to_owned()),
            GraphDbError::Cancelled => {
                Self::Unavailable("embedded graph open was cancelled".to_owned())
            }
            GraphDbError::InvalidRequest { message } => Self::Unavailable(message),
            GraphDbError::Conflict => Self::IdentityConflict,
            GraphDbError::BudgetExhausted => {
                Self::Unavailable("embedded graph open budget was exhausted".to_owned())
            }
        }
    }
}

struct MountedProjectGraph {
    store_root: PathBuf,
    graph_path: PathBuf,
    database: Arc<GraphDb>,
}

struct SessionRelationEffectScheduler {
    wake: Arc<tokio::sync::Notify>,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<tokio::sync::Notify>,
    lcm_cancellation: tracedecay_application::CancellationSignal,
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone, Default)]
pub(crate) struct EmbeddedGraphRuntimeRegistry {
    mounted: Arc<Mutex<BTreeMap<SessionRelationScope, MountedProjectGraph>>>,
    effect_schedulers:
        Arc<Mutex<BTreeMap<SessionRelationScope, Arc<SessionRelationEffectScheduler>>>>,
}

impl std::fmt::Debug for EmbeddedGraphRuntimeRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedGraphRuntimeRegistry")
            .finish_non_exhaustive()
    }
}

impl EmbeddedGraphRuntimeRegistry {
    pub(crate) fn resolve_scope(
        &self,
        scope: &SessionRelationScope,
        session_store_root: &Path,
    ) -> Result<Arc<GraphDb>, EmbeddedGraphRuntimeError> {
        let store_root = session_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical session store {} is unavailable: {error}",
                session_store_root.display()
            ))
        })?;
        let graph_path = persistent_session_relation_graph_path(&store_root);
        let graph_directory = graph_path.parent().ok_or_else(|| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph path has no storage directory".to_owned(),
            )
        })?;
        std::fs::create_dir_all(&graph_directory).map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "embedded graph directory {} is unavailable: {error}",
                graph_directory.display()
            ))
        })?;
        let mut mounted = self.mounted.lock().map_err(|_| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph registry lock is poisoned".to_owned(),
            )
        })?;
        if let Some(existing) = mounted.get(scope) {
            return if existing.store_root == store_root && existing.graph_path == graph_path {
                Ok(Arc::clone(&existing.database))
            } else {
                Err(EmbeddedGraphRuntimeError::IdentityConflict)
            };
        }
        if mounted
            .values()
            .any(|existing| existing.graph_path == graph_path)
        {
            return Err(EmbeddedGraphRuntimeError::IdentityConflict);
        }
        let database = open_persistent_session_relation_graph(graph_path.clone())?;
        mounted.insert(
            scope.clone(),
            MountedProjectGraph {
                store_root,
                graph_path,
                database: Arc::clone(&database),
            },
        );
        Ok(database)
    }

    pub(crate) fn ensure_relation_effect_scheduler(
        &self,
        scope: &SessionRelationScope,
        database: Arc<tracedecay_global_db::RegisteredGlobalDb>,
    ) -> Result<(), EmbeddedGraphRuntimeError> {
        let mut schedulers = self.effect_schedulers.lock().map_err(|_| {
            EmbeddedGraphRuntimeError::Unavailable(
                "session relation effect scheduler lock is poisoned".to_owned(),
            )
        })?;
        if let Some(existing) = schedulers.get(scope) {
            database
                .bind_session_relation_effect_wake(Arc::clone(&existing.wake))
                .map_err(|error| EmbeddedGraphRuntimeError::Unavailable(error.to_string()))?;
            existing.wake.notify_one();
            return Ok(());
        }

        let wake = Arc::new(tokio::sync::Notify::new());
        database
            .bind_session_relation_effect_wake(Arc::clone(&wake))
            .map_err(|error| EmbeddedGraphRuntimeError::Unavailable(error.to_string()))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());
        let lcm_cancellation = tracedecay_application::CancellationSignal::active(format!(
            "session-effects-{}",
            scope.identity()
        ))
        .map_err(|error| EmbeddedGraphRuntimeError::Unavailable(error.to_string()))?;
        let worker_wake = Arc::clone(&wake);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_shutdown_notify = Arc::clone(&shutdown_notify);
        let worker_lcm_cancellation = lcm_cancellation.clone();
        let join = tokio::spawn(async move {
            run_relation_effect_scheduler(
                database,
                worker_wake,
                worker_shutdown,
                worker_shutdown_notify,
                worker_lcm_cancellation,
            )
            .await;
        });
        schedulers.insert(
            scope.clone(),
            Arc::new(SessionRelationEffectScheduler {
                wake,
                shutdown,
                shutdown_notify,
                lcm_cancellation,
                join: Mutex::new(Some(join)),
            }),
        );
        Ok(())
    }

    pub(crate) async fn shutdown_relation_effect_schedulers(&self) {
        let schedulers = match self.effect_schedulers.lock() {
            Ok(schedulers) => schedulers.values().cloned().collect::<Vec<_>>(),
            Err(_) => {
                tracing::error!(
                    event = "session_relation_effect_scheduler_shutdown",
                    outcome = "registry_lock_poisoned"
                );
                return;
            }
        };
        for scheduler in &schedulers {
            scheduler.shutdown.store(true, Ordering::Release);
            if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                && let Ok(micros) = i64::try_from(duration.as_micros())
            {
                scheduler
                    .lcm_cancellation
                    .cancel(tracedecay_domain::UtcMicros(micros));
            }
            scheduler.shutdown_notify.notify_waiters();
            scheduler.wake.notify_waiters();
        }
        for scheduler in schedulers {
            let join = match scheduler.join.lock() {
                Ok(mut join) => join.take(),
                Err(_) => {
                    tracing::error!(
                        event = "session_relation_effect_scheduler_shutdown",
                        outcome = "join_lock_poisoned"
                    );
                    None
                }
            };
            if let Some(join) = join
                && let Err(error) = join.await
            {
                tracing::error!(
                    event = "session_relation_effect_scheduler_shutdown",
                    outcome = "join_failed",
                    error = %error
                );
            }
        }
        if let Ok(mut schedulers) = self.effect_schedulers.lock() {
            schedulers.clear();
        }
    }
}

#[derive(Debug)]
struct SchedulerCancellation {
    shutdown: Arc<AtomicBool>,
}

impl tracedecay_graph_db::GraphCancellation for SchedulerCancellation {
    fn is_cancelled(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

async fn run_relation_effect_scheduler(
    database: Arc<tracedecay_global_db::RegisteredGlobalDb>,
    wake: Arc<tokio::sync::Notify>,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<tokio::sync::Notify>,
    lcm_cancellation: tracedecay_application::CancellationSignal,
) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        for _ in 0..RELATION_EFFECT_MAX_PAGES_PER_WAKE {
            if shutdown.load(Ordering::Acquire) || started.elapsed() >= RELATION_EFFECT_MAX_RUN_TIME
            {
                break;
            }
            let recovered = database
                .recover_pending_session_relation_projections(
                    RELATION_EFFECT_PAGE_SIZE,
                    Arc::new(SchedulerCancellation {
                        shutdown: Arc::clone(&shutdown),
                    }),
                )
                .await;
            match recovered {
                Ok(0) => break,
                Ok(recovered) if recovered < RELATION_EFFECT_PAGE_SIZE => break,
                Ok(_) => {}
                Err(_) if shutdown.load(Ordering::Acquire) => break,
                Err(error) => {
                    crate::daemon::log_daemon_event(
                        "session_relation_effect_scheduler",
                        &[
                            ("outcome", "degraded".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    break;
                }
            }
        }
        for _ in 0..LCM_HOST_EFFECT_MAX_ITEMS_PER_WAKE {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            match super::lcm_host_effects::process_one_pending_lcm_host_effect(
                Arc::clone(&database),
                &lcm_cancellation,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    crate::daemon::log_daemon_event(
                        "lcm_host_effect_scheduler",
                        &[
                            ("outcome", "degraded".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    break;
                }
            }
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            () = wake.notified() => {}
            () = shutdown_notify.notified() => {}
            () = tokio::time::sleep(RELATION_EFFECT_IDLE_RETRY) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_domain::{ProjectId, UserProfileId};

    use super::*;
    use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    #[test]
    fn project_and_profile_mounts_require_distinct_exact_store_roots() {
        let temporary = TempDir::new().expect("temporary mount root");
        let project_root = temporary.path().join("project-sessions");
        let profile_root = temporary.path().join("profile-sessions");
        std::fs::create_dir_all(&project_root).expect("project root");
        std::fs::create_dir_all(&profile_root).expect("profile root");
        let project_scope = SessionRelationScope::project(
            ProjectId::new("project.mount-isolation").expect("project id"),
        );
        let profile_scope = SessionRelationScope::profile(
            UserProfileId::new("profile.mount-isolation").expect("profile id"),
        );
        let registry = EmbeddedGraphRuntimeRegistry::default();

        let project = registry
            .resolve_scope(&project_scope, &project_root)
            .expect("project mount");
        assert!(matches!(
            registry.resolve_scope(&profile_scope, &project_root),
            Err(EmbeddedGraphRuntimeError::IdentityConflict)
        ));
        let profile = registry
            .resolve_scope(&profile_scope, &profile_root)
            .expect("profile mount");
        assert!(!Arc::ptr_eq(&project, &profile));
    }

    #[tokio::test]
    async fn relation_effect_scheduler_survives_idle_and_shutdown_joins_worker() {
        let temporary = TempDir::new().expect("temporary profile root");
        let runtime = HostAdmissionTestRuntimeV1::profile(temporary.path())
            .await
            .expect("registered profile runtime");
        let database = runtime
            .session_database_arc_for_test(HostAdmissionScope::Profile)
            .expect("profile session database");
        let scope = SessionRelationScope::profile(database.binding().shard_id.profile_id.clone());
        let registry = EmbeddedGraphRuntimeRegistry::default();

        registry
            .ensure_relation_effect_scheduler(&scope, database)
            .expect("start relation effect scheduler");
        tokio::task::yield_now().await;
        let scheduler = registry
            .effect_schedulers
            .lock()
            .expect("scheduler registry")
            .get(&scope)
            .cloned()
            .expect("retained scheduler");
        assert!(
            !scheduler
                .join
                .lock()
                .expect("scheduler join")
                .as_ref()
                .expect("scheduler worker")
                .is_finished(),
            "the scheduler must remain retained while the journal is idle"
        );

        registry.shutdown_relation_effect_schedulers().await;
        assert!(
            registry
                .effect_schedulers
                .lock()
                .expect("scheduler registry after shutdown")
                .is_empty(),
            "shutdown must join and retire every relation effect worker"
        );
    }
}
