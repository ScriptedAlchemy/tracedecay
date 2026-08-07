use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::super::StoreOwnerKey;
use super::history::SharedSessionHistoricalIngestor;
use super::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshPolicy,
    SessionTemporalRefreshProjector,
};
#[cfg(test)]
use super::wake::SessionTemporalRefreshWorkerStatus;
use super::wake::{
    SessionTemporalRefreshBlocker, SessionTemporalRefreshRetryClass, SessionTemporalRefreshWake,
    SessionTemporalRefreshWakeState,
};
use super::worker::run_session_temporal_refresh_scheduler;
use crate::global_db::RegisteredGlobalDb;

#[derive(Default, Debug, Eq, PartialEq)]
pub(super) struct SessionTemporalRefreshPassReport {
    pub(super) begun: usize,
    pub(super) joined: usize,
    pub(super) projected_batches: usize,
    pub(super) completed: usize,
    pub(super) failed: usize,
    pub(super) cancelled: usize,
    pub(super) deferred: usize,
    pub(super) retryable_errors: usize,
    pub(super) terminal_errors: usize,
    pub(super) deadline_errors: usize,
    pub(super) saturated: bool,
    pub(super) backlog: Option<usize>,
    pub(super) retry_class: Option<SessionTemporalRefreshRetryClass>,
    pub(super) last_error: Option<String>,
}

impl SessionTemporalRefreshPassReport {
    pub(super) fn observe_retry(&mut self, class: SessionTemporalRefreshRetryClass) {
        let rank = |candidate| match candidate {
            SessionTemporalRefreshRetryClass::Storage => 1,
            SessionTemporalRefreshRetryClass::Projector => 2,
            SessionTemporalRefreshRetryClass::Deadline => 3,
        };
        if self
            .retry_class
            .is_none_or(|current| rank(class) > rank(current))
        {
            self.retry_class = Some(class);
        }
    }
}

struct SessionTemporalRefreshSchedulerEntry {
    state: Arc<SessionTemporalRefreshWakeState>,
    wake: SessionTemporalRefreshWake,
    history: Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    task: tokio::task::JoinHandle<()>,
}

impl SessionTemporalRefreshSchedulerEntry {
    async fn shutdown(self) {
        if let Some(history) = self
            .history
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            history.cancel();
        }
        self.state.cancel();
        let mut task = self.task;
        if tokio::time::timeout(super::super::DAEMON_CLIENT_DRAIN_DEADLINE, &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

pub(in crate::daemon) struct SessionTemporalRefreshSchedulerRegistry {
    project: tokio::sync::Mutex<HashMap<StoreOwnerKey, SessionTemporalRefreshSchedulerEntry>>,
    profile: tokio::sync::Mutex<HashMap<std::path::PathBuf, SessionTemporalRefreshSchedulerEntry>>,
    pub(super) projector: Arc<dyn SessionTemporalRefreshProjector>,
    pub(super) policy: SessionTemporalRefreshPolicy,
    shutting_down: AtomicBool,
    #[cfg_attr(not(unix), allow(dead_code))] // held by the unix-only daemon shutdown path
    shutdown_guard: tokio::sync::Mutex<()>,
    project_lifecycle: tokio::sync::Mutex<()>,
    retired_project_owners: std::sync::Mutex<HashSet<StoreOwnerKey>>,
}

impl Default for SessionTemporalRefreshSchedulerRegistry {
    fn default() -> Self {
        Self {
            project: tokio::sync::Mutex::new(HashMap::new()),
            profile: tokio::sync::Mutex::new(HashMap::new()),
            projector: Arc::new(CanonicalSessionTemporalProjector),
            policy: SessionTemporalRefreshPolicy::default(),
            shutting_down: AtomicBool::new(false),
            shutdown_guard: tokio::sync::Mutex::new(()),
            project_lifecycle: tokio::sync::Mutex::new(()),
            retired_project_owners: std::sync::Mutex::new(HashSet::new()),
        }
    }
}

impl Drop for SessionTemporalRefreshSchedulerRegistry {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        if let Ok(project) = self.project.try_lock() {
            for entry in project.values() {
                if let Some(history) = entry
                    .history
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                {
                    history.cancel();
                }
                entry.state.cancel();
            }
        }
        if let Ok(profile) = self.profile.try_lock() {
            for entry in profile.values() {
                if let Some(history) = entry
                    .history
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                {
                    history.cancel();
                }
                entry.state.cancel();
            }
        }
    }
}

impl SessionTemporalRefreshSchedulerRegistry {
    fn spawn_entry(
        &self,
        database: Arc<RegisteredGlobalDb>,
        route: Option<SessionTemporalRefreshWake>,
        history: Option<SharedSessionHistoricalIngestor>,
    ) -> SessionTemporalRefreshSchedulerEntry {
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let history = Arc::new(std::sync::RwLock::new(history));
        let wake = route.unwrap_or_else(|| state.handle());
        wake.bind(&state);
        state.mark_running();
        let worker_state = Arc::clone(&state);
        let projector = Arc::clone(&self.projector);
        let worker_history = Arc::clone(&history);
        let policy = self.policy;
        state.wake();
        let task = tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            let mut panic_attempt = 0u32;
            loop {
                workers.spawn(run_session_temporal_refresh_scheduler(
                    Arc::clone(&database),
                    Arc::clone(&worker_state),
                    Arc::clone(&projector),
                    Arc::clone(&worker_history),
                    policy,
                ));
                let Some(result) = workers.join_next().await else {
                    worker_state.mark_stopped();
                    return;
                };
                match result {
                    Err(error)
                        if error.is_panic() && !worker_state.cancelled.load(Ordering::Acquire) =>
                    {
                        panic_attempt = panic_attempt.saturating_add(1);
                        worker_state.busy.store(false, Ordering::Release);
                        worker_state.mark_recovering(
                            SessionTemporalRefreshBlocker::WorkerPanicked,
                            SessionTemporalRefreshRetryClass::Projector,
                        );
                        worker_state.dirty.store(true, Ordering::Release);
                        worker_state.recover_history_after_worker_panic();
                        tokio::select! {
                            () = worker_state.wait_for_cancellation() => return,
                            () = tokio::time::sleep(session_refresh_retry_delay(
                                SessionTemporalRefreshRetryClass::Projector,
                                panic_attempt,
                            )) => {}
                        }
                    }
                    Ok(()) | Err(_) => {
                        worker_state.mark_stopped();
                        return;
                    }
                }
            }
        });
        SessionTemporalRefreshSchedulerEntry {
            state,
            wake,
            history,
            task,
        }
    }

    pub(in crate::daemon) async fn ensure_project(
        &self,
        owner: StoreOwnerKey,
        database: Arc<RegisteredGlobalDb>,
    ) -> SessionTemporalRefreshWake {
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        let _lifecycle = self.project_lifecycle.lock().await;
        if self
            .retired_project_owners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&owner)
        {
            return inert_session_temporal_refresh_wake();
        }
        let mut project = self.project.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        if project
            .get(&owner)
            .is_some_and(|entry| entry.task.is_finished())
        {
            // Confirmed present by the check above while holding the lock.
            #[allow(clippy::expect_used)]
            let finished = project.remove(&owner).expect("finished entry disappeared");
            let route = finished.wake.clone();
            let history = finished
                .history
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            finished.shutdown().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            let entry = self.spawn_entry(database, Some(route), history);
            let wake = entry.wake.clone();
            project.insert(owner, entry);
            return wake;
        }
        if let Some(entry) = project.get(&owner) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        let entry = self.spawn_entry(database, None, None);
        let wake = entry.wake.clone();
        project.insert(owner, entry);
        wake
    }

    pub(in crate::daemon) async fn ensure_profile(
        &self,
        database_path: std::path::PathBuf,
        database: Arc<RegisteredGlobalDb>,
    ) -> SessionTemporalRefreshWake {
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        let mut profile = self.profile.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        if profile
            .get(&database_path)
            .is_some_and(|entry| entry.task.is_finished())
        {
            // Confirmed present by the check above while holding the lock.
            #[allow(clippy::expect_used)]
            let finished = profile
                .remove(&database_path)
                .expect("finished entry disappeared");
            let route = finished.wake.clone();
            let history = finished
                .history
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            finished.shutdown().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            let entry = self.spawn_entry(database, Some(route), history);
            let wake = entry.wake.clone();
            profile.insert(database_path, entry);
            return wake;
        }
        if let Some(entry) = profile.get(&database_path) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        let entry = self.spawn_entry(database, None, None);
        let wake = entry.wake.clone();
        profile.insert(database_path, entry);
        wake
    }

    pub(in crate::daemon) async fn ensure_project_with_history(
        &self,
        owner: StoreOwnerKey,
        database: Arc<RegisteredGlobalDb>,
        history: SharedSessionHistoricalIngestor,
    ) -> SessionTemporalRefreshWake {
        let wake = self.ensure_project(owner.clone(), database).await;
        if let Some(entry) = self.project.lock().await.get(&owner) {
            let mut retained = entry
                .history
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if retained.is_none() {
                *retained = Some(history);
                entry.state.mark_history_pending();
            }
            drop(retained);
            entry.state.wake_history();
        }
        wake
    }

    pub(in crate::daemon) async fn ensure_profile_with_history(
        &self,
        database_path: std::path::PathBuf,
        database: Arc<RegisteredGlobalDb>,
        history: SharedSessionHistoricalIngestor,
    ) -> SessionTemporalRefreshWake {
        let wake = self.ensure_profile(database_path.clone(), database).await;
        if let Some(entry) = self.profile.lock().await.get(&database_path) {
            let mut retained = entry
                .history
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if retained.is_none() {
                *retained = Some(history);
                entry.state.mark_history_pending();
            }
            drop(retained);
            entry.state.wake_history();
        }
        wake
    }

    pub(in crate::daemon) async fn rekey_project(
        &self,
        old_owner: &StoreOwnerKey,
        new_owner: StoreOwnerKey,
        database: Arc<RegisteredGlobalDb>,
    ) {
        if old_owner == &new_owner {
            self.ensure_project(new_owner, database).await;
            return;
        }
        let _lifecycle = self.project_lifecycle.lock().await;
        {
            let mut retired = self
                .retired_project_owners
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            retired.insert(old_owner.clone());
            retired.remove(&new_owner);
        }
        let retired = self.project.lock().await.remove(old_owner);
        let (route, staging) = if let Some(entry) = retired {
            let route = entry.wake.clone();
            let staging = Arc::new(SessionTemporalRefreshWakeState::default());
            let retired_state = Arc::clone(&entry.state);
            route.bind(&staging);
            entry.shutdown().await;
            retired_state.transfer_requests_to(&staging);
            (Some(route), Some(staging))
        } else {
            (None, None)
        };
        if self.shutting_down.load(Ordering::Acquire) {
            if let Some(staging) = staging {
                staging.cancel();
            }
            return;
        }
        let mut project = self.project.lock().await;
        if let Some(existing) = project.get(&new_owner) {
            if let Some(route) = route {
                route.bind(&existing.state);
            }
            if let Some(staging) = staging {
                staging.cancel();
                staging.transfer_requests_to(&existing.state);
            }
            existing.wake.wake();
            return;
        }
        let entry = self.spawn_entry(database, route, None);
        if let Some(staging) = staging {
            staging.cancel();
            staging.transfer_requests_to(&entry.state);
        }
        project.insert(new_owner, entry);
    }

    pub(in crate::daemon) async fn retire_project(&self, owner: &StoreOwnerKey) {
        let _lifecycle = self.project_lifecycle.lock().await;
        if let Some(entry) = self.project.lock().await.remove(owner) {
            entry.shutdown().await;
        }
    }

    pub(in crate::daemon) async fn owns_project_database_paths(
        &self,
        database_paths: &HashSet<std::path::PathBuf>,
    ) -> bool {
        self.project
            .lock()
            .await
            .keys()
            .any(|owner| database_paths.contains(&owner.graph_db_path))
    }

    pub(in crate::daemon) async fn cancel_historical_ingest(&self) {
        let project = self.project.lock().await;
        let profile = self.profile.lock().await;
        for entry in project.values().chain(profile.values()) {
            if let Some(history) = entry
                .history
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                history.cancel();
            }
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))] // invoked by the unix-only daemon shutdown path
    pub(in crate::daemon) async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let _guard = self.shutdown_guard.lock().await;
        let _project_lifecycle = self.project_lifecycle.lock().await;
        let project = self
            .project
            .lock()
            .await
            .drain()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let profile = self
            .profile
            .lock()
            .await
            .drain()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let mut retirements = tokio::task::JoinSet::new();
        for entry in project.into_iter().chain(profile) {
            retirements.spawn(entry.shutdown());
        }
        while retirements.join_next().await.is_some() {}
    }

    #[cfg(test)]
    pub(super) async fn project_state(
        &self,
        owner: &StoreOwnerKey,
    ) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.project
            .lock()
            .await
            .get(owner)
            .map(|entry| Arc::clone(&entry.state))
    }

    #[cfg(test)]
    pub(super) async fn profile_worker_status(
        &self,
        database_path: &std::path::Path,
    ) -> SessionTemporalRefreshWorkerStatus {
        self.profile.lock().await.get(database_path).map_or_else(
            || SessionTemporalRefreshWake::unavailable().status(),
            |entry| entry.wake.status(),
        )
    }

    #[cfg(test)]
    pub(super) async fn project_worker_count(&self) -> usize {
        self.project.lock().await.len()
    }

    #[cfg(test)]
    pub(super) async fn profile_worker_count(&self) -> usize {
        self.profile.lock().await.len()
    }

    #[cfg(test)]
    pub(super) async fn profile_pass_count(&self, database_path: &std::path::Path) -> usize {
        self.profile
            .lock()
            .await
            .get(database_path)
            .map_or(0, |entry| entry.state.pass_count.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(super) async fn wait_profile_idle(
        &self,
        database_path: &std::path::Path,
        timeout: Duration,
    ) -> bool {
        let state = self
            .profile
            .lock()
            .await
            .get(database_path)
            .map(|entry| Arc::clone(&entry.state));
        let Some(state) = state else {
            return true;
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if state.is_idle() {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::select! {
                () = state.idle.notified() => {}
                () = tokio::time::sleep(remaining) => return state.is_idle(),
            }
        }
    }
}

fn inert_session_temporal_refresh_wake() -> SessionTemporalRefreshWake {
    SessionTemporalRefreshWake::unavailable()
}

pub(super) fn session_refresh_retry_delay(
    class: SessionTemporalRefreshRetryClass,
    attempt: u32,
) -> Duration {
    let shift_cap = match class {
        SessionTemporalRefreshRetryClass::Storage => 5,
        SessionTemporalRefreshRetryClass::Projector => 16,
        SessionTemporalRefreshRetryClass::Deadline => 6,
    };
    crate::application::host_admission::replay_backoff(attempt, shift_cap)
}
