use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::history::SharedSessionHistoricalIngestor;
use super::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshPolicy,
    SessionTemporalRefreshProjector,
};
#[cfg(any(test, feature = "test-helpers"))]
use super::wake::SessionTemporalRefreshWorkerStatus;
use super::wake::{
    SessionTemporalRefreshBlocker, SessionTemporalRefreshRetryClass, SessionTemporalRefreshWake,
    SessionTemporalRefreshWakeState,
};
use super::worker::run_session_temporal_refresh_scheduler;
use crate::StoreOwnerKey;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::session_ingest_disabled;

macro_rules! define_pass_report {
    ($visibility:vis) => {
        #[derive(Default, Debug, Eq, PartialEq)]
        $visibility struct SessionTemporalRefreshPassReport {
            $visibility begun: usize,
            $visibility joined: usize,
            $visibility projected_batches: usize,
            $visibility completed: usize,
            $visibility failed: usize,
            $visibility cancelled: usize,
            $visibility deferred: usize,
            $visibility retryable_errors: usize,
            $visibility terminal_errors: usize,
            $visibility deadline_errors: usize,
            $visibility saturated: bool,
            $visibility backlog: Option<usize>,
            $visibility retry_class: Option<SessionTemporalRefreshRetryClass>,
            $visibility last_error: Option<String>,
        }
    };
}

#[cfg(any(test, feature = "test-helpers"))]
define_pass_report!(pub);
#[cfg(not(any(test, feature = "test-helpers")))]
define_pass_report!(pub(crate));

impl SessionTemporalRefreshPassReport {
    pub(crate) fn observe_retry(&mut self, class: SessionTemporalRefreshRetryClass) {
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

/// Bounded daemon-wide concurrency for historical session-ingest passes.
///
/// Every mounted project scheduler plus the profile scheduler owns one worker
/// task, so a daemon serving N registered projects would otherwise run N + 1
/// historical catch-up passes concurrently at startup, each pass is bounded,
/// but the aggregate grew with the number of projects (the 11.6 GB catch-up
/// incident). Daemon readiness never waits on this admission: catch-up is
/// background work, and a worker that cannot acquire a permit defers its
/// history pass as typed retryable state while projection serving continues.
///
/// Two rather than one for the same reason as the code-index reconcile bound:
/// a pass is not pure CPU, discovery, store writes, and projection drains are
/// I/O and lock phases that overlap a second pass's parsing at negligible
/// cost, while race-to-idle finishes each backlog sooner than interleaving
/// all of them.
const MAX_CONCURRENT_HISTORICAL_INGEST_PASSES: usize = 2;

fn bounded_historical_ingest_permits() -> usize {
    std::thread::available_parallelism().map_or(1, |cores| {
        cores.get().min(MAX_CONCURRENT_HISTORICAL_INGEST_PASSES)
    })
}

struct SessionTemporalRefreshSchedulerEntry {
    state: Arc<SessionTemporalRefreshWakeState>,
    wake: SessionTemporalRefreshWake,
    history: Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    task: tokio::task::JoinHandle<()>,
}

impl SessionTemporalRefreshSchedulerEntry {
    fn cancel(&self) {
        if let Some(history) = self
            .history
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            history.cancel();
        }
        self.state.cancel();
    }
}

#[derive(Clone, PartialEq, Eq)]
enum SessionTemporalRefreshOwner {
    Project(StoreOwnerKey),
    Profile(std::path::PathBuf),
}

type RetiredSessionTemporalRefreshEntry = (
    SessionTemporalRefreshOwner,
    SessionTemporalRefreshSchedulerEntry,
);
type RetiredSessionTemporalRefreshEntries =
    std::sync::Mutex<Vec<RetiredSessionTemporalRefreshEntry>>;

/// Owns entries removed from the registry until their supervisors settle.
/// Construction cancels every entry, and dropping the join (a shared shutdown
/// deadline) returns unfinished entries to the registry, so a later drain or a
/// readmission of the same owner joins those supervisors instead of detaching
/// lease-owning workers.
struct SessionTemporalRefreshRetirement<'a> {
    retired: &'a RetiredSessionTemporalRefreshEntries,
    entries: Vec<RetiredSessionTemporalRefreshEntry>,
}

impl<'a> SessionTemporalRefreshRetirement<'a> {
    fn new(
        retired: &'a RetiredSessionTemporalRefreshEntries,
        entries: Vec<RetiredSessionTemporalRefreshEntry>,
    ) -> Self {
        for (_, entry) in &entries {
            entry.cancel();
        }
        Self { retired, entries }
    }

    /// Each supervisor bounds its own drain grace and joins its aborted
    /// workers, so awaiting it proves the worker released its leases.
    async fn join(mut self) {
        while let Some((_, entry)) = self.entries.last_mut() {
            let _ = (&mut entry.task).await;
            self.entries.pop();
        }
    }
}

impl Drop for SessionTemporalRefreshRetirement<'_> {
    fn drop(&mut self) {
        self.entries.retain(|(_, entry)| !entry.task.is_finished());
        self.retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append(&mut self.entries);
    }
}

pub struct SessionTemporalRefreshSchedulerRegistry {
    project: tokio::sync::Mutex<HashMap<StoreOwnerKey, SessionTemporalRefreshSchedulerEntry>>,
    profile: tokio::sync::Mutex<HashMap<std::path::PathBuf, SessionTemporalRefreshSchedulerEntry>>,
    retired: RetiredSessionTemporalRefreshEntries,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    policy: SessionTemporalRefreshPolicy,
    shutting_down: AtomicBool,
    #[cfg_attr(not(unix), allow(dead_code))] // held by the unix-only daemon shutdown path
    shutdown_guard: tokio::sync::Mutex<()>,
    project_lifecycle: tokio::sync::Mutex<()>,
    retired_project_owners: std::sync::Mutex<HashSet<StoreOwnerKey>>,
    codex_discovery: Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub>,
    /// The process background CPU authority mounted by
    /// [`Self::configure_codex_preparation_resources`]; retained so historical
    /// ingest compositions built through this registry inject the same
    /// authority Codex page preparation meters against.
    background_cpu: std::sync::OnceLock<Arc<ProcessBackgroundCpuV1>>,
    historical_ingest_admission: Arc<tokio::sync::Semaphore>,
}

impl Default for SessionTemporalRefreshSchedulerRegistry {
    fn default() -> Self {
        Self {
            project: tokio::sync::Mutex::new(HashMap::new()),
            profile: tokio::sync::Mutex::new(HashMap::new()),
            retired: std::sync::Mutex::new(Vec::new()),
            projector: Arc::new(CanonicalSessionTemporalProjector),
            policy: SessionTemporalRefreshPolicy::default(),
            shutting_down: AtomicBool::new(false),
            shutdown_guard: tokio::sync::Mutex::new(()),
            project_lifecycle: tokio::sync::Mutex::new(()),
            retired_project_owners: std::sync::Mutex::new(HashSet::new()),
            codex_discovery: Arc::new(
                tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub::default(),
            ),
            background_cpu: std::sync::OnceLock::new(),
            historical_ingest_admission: Arc::new(tokio::sync::Semaphore::new(
                bounded_historical_ingest_permits(),
            )),
        }
    }
}

impl Drop for SessionTemporalRefreshSchedulerRegistry {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        let project = self.project.get_mut().values();
        let profile = self.profile.get_mut().values();
        for entry in project.chain(profile) {
            entry.cancel();
        }
    }
}

impl SessionTemporalRefreshSchedulerRegistry {
    #[cfg(any(test, feature = "test-helpers"))]
    pub(crate) fn configure_for_test(
        &mut self,
        projector: Arc<dyn SessionTemporalRefreshProjector>,
        policy: SessionTemporalRefreshPolicy,
    ) {
        self.projector = projector;
        self.policy = policy;
    }

    /// The bounded historical-ingest admission, so a test can occupy its
    /// permits and assert that saturated workers defer their history pass
    /// instead of running an unbounded aggregate.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn historical_ingest_admission(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.historical_ingest_admission)
    }

    /// Mount the process resources session preparation meters against. The
    /// first mount wins, matching the process-wide Codex preparation authority
    /// it configures, so a later fixture reopen rejoins the retained CPU
    /// authority instead of shadowing it.
    pub fn configure_codex_preparation_resources(
        &self,
        memory: Arc<tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1>,
        background_cpu: Arc<ProcessBackgroundCpuV1>,
    ) -> tracedecay_sessions::runtime::source::TranscriptIngestResult<()> {
        self.codex_discovery
            .configure_preparation_resources(memory, Arc::clone(&background_cpu))?;
        let _ = self.background_cpu.set(background_cpu);
        Ok(())
    }

    /// The retained process background CPU authority, or `None` before
    /// [`Self::configure_codex_preparation_resources`] ran.
    pub fn background_cpu(&self) -> Option<Arc<ProcessBackgroundCpuV1>> {
        self.background_cpu.get().map(Arc::clone)
    }

    pub fn codex_discovery(
        &self,
    ) -> Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub> {
        Arc::clone(&self.codex_discovery)
    }

    fn spawn_entry(
        &self,
        database: RegisteredGlobalDbLeaseV1,
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
        let history_admission = Arc::clone(&self.historical_ingest_admission);
        let policy = self.policy;
        state.wake();
        let supervisor = tracing::Instrument::instrument(
            async move {
                if session_ingest_disabled() {
                    // Dev/profiling switch: leave the scheduler mounted but run
                    // no ingest workers, so code indexing can be measured
                    // without transcript ingestion competing for the writer.
                    worker_state.mark_stopped();
                    return;
                }
                let mut workers = tokio::task::JoinSet::new();
                let mut panic_attempt = 0u32;
                loop {
                    workers.spawn(tracing::Instrument::instrument(
                        run_session_temporal_refresh_scheduler(
                            database.clone(),
                            Arc::clone(&worker_state),
                            Arc::clone(&projector),
                            Arc::clone(&worker_history),
                            Arc::clone(&history_admission),
                            policy,
                        ),
                        tracing::trace_span!("daemon.scheduler.session_temporal.worker"),
                    ));
                    let joined = tokio::select! {
                        joined = tracing::Instrument::instrument(
                            workers.join_next(),
                            tracing::trace_span!("daemon.scheduler.session_temporal.worker_join_wait"),
                        ) => joined,
                        () = async {
                            worker_state.wait_for_cancellation().await;
                            tokio::time::sleep(crate::DAEMON_CLIENT_DRAIN_DEADLINE).await;
                        } => {
                            // An abort only requests cancellation; joining the
                            // aborted worker proves its lease clones dropped.
                            workers.abort_all();
                            while workers.join_next().await.is_some() {}
                            worker_state.mark_stopped();
                            return;
                        }
                    };
                    let Some(result) = joined else {
                        worker_state.mark_stopped();
                        return;
                    };
                    match result {
                        Err(error)
                            if error.is_panic()
                                && !worker_state.cancelled.load(Ordering::Acquire) =>
                        {
                            panic_attempt = panic_attempt.saturating_add(1);
                            worker_state.mark_worker_idle();
                            worker_state.mark_recovering(
                                SessionTemporalRefreshBlocker::WorkerPanicked,
                                SessionTemporalRefreshRetryClass::Projector,
                            );
                            worker_state.requeue_projection();
                            worker_state.recover_history_after_worker_panic();
                            tokio::select! {
                                () = tracing::Instrument::instrument(worker_state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.supervisor_retry_cancel")) => return,
                                () = tracing::Instrument::instrument(tokio::time::sleep(session_refresh_retry_delay(
                                        SessionTemporalRefreshRetryClass::Projector,
                                        panic_attempt,
                                    )), tracing::trace_span!("daemon.scheduler.session_temporal.supervisor_retry_wait")) => {}
                            }
                        }
                        Ok(()) | Err(_) => {
                            worker_state.mark_stopped();
                            return;
                        }
                    }
                }
            },
            tracing::trace_span!("daemon.scheduler.session_temporal.supervisor"),
        );
        let task = tokio::spawn(supervisor);
        SessionTemporalRefreshSchedulerEntry {
            state,
            wake,
            history,
            task,
        }
    }

    fn retire<'a>(
        &'a self,
        entries: Vec<RetiredSessionTemporalRefreshEntry>,
    ) -> SessionTemporalRefreshRetirement<'a> {
        SessionTemporalRefreshRetirement::new(&self.retired, entries)
    }

    /// Joins an unfinished retirement of `owner`, so the owner never runs a
    /// fresh supervisor beside one that still holds its leases.
    async fn settle_retired(&self, owner: &SessionTemporalRefreshOwner) {
        let entries = {
            let mut retired = self.retired.lock().unwrap_or_else(PoisonError::into_inner);
            let (owned, others) = std::mem::take(&mut *retired)
                .into_iter()
                .partition(|(retired_owner, _)| retired_owner == owner);
            *retired = others;
            owned
        };
        self.retire(entries).join().await;
    }

    pub async fn ensure_project(
        &self,
        owner: StoreOwnerKey,
        database: RegisteredGlobalDbLeaseV1,
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
        let retired_owner = SessionTemporalRefreshOwner::Project(owner.clone());
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
            self.retire(vec![(retired_owner.clone(), finished)])
                .join()
                .await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            self.settle_retired(&retired_owner).await;
            let entry = self.spawn_entry(database, Some(route), history);
            let wake = entry.wake.clone();
            project.insert(owner, entry);
            return wake;
        }
        if let Some(entry) = project.get(&owner) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        self.settle_retired(&retired_owner).await;
        let entry = self.spawn_entry(database, None, None);
        let wake = entry.wake.clone();
        project.insert(owner, entry);
        wake
    }

    pub async fn ensure_profile(
        &self,
        database_path: std::path::PathBuf,
        database: RegisteredGlobalDbLeaseV1,
    ) -> SessionTemporalRefreshWake {
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        let mut profile = self.profile.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        let retired_owner = SessionTemporalRefreshOwner::Profile(database_path.clone());
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
            self.retire(vec![(retired_owner.clone(), finished)])
                .join()
                .await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            self.settle_retired(&retired_owner).await;
            let entry = self.spawn_entry(database, Some(route), history);
            let wake = entry.wake.clone();
            profile.insert(database_path, entry);
            return wake;
        }
        if let Some(entry) = profile.get(&database_path) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        self.settle_retired(&retired_owner).await;
        let entry = self.spawn_entry(database, None, None);
        let wake = entry.wake.clone();
        profile.insert(database_path, entry);
        wake
    }

    pub async fn ensure_project_with_history(
        &self,
        owner: StoreOwnerKey,
        database: RegisteredGlobalDbLeaseV1,
        history: SharedSessionHistoricalIngestor,
    ) -> SessionTemporalRefreshWake {
        let wake = self.ensure_project(owner.clone(), database).await;
        if session_ingest_disabled() {
            // Dev/profiling switch: never install a history ingestor, so the
            // project-open catch-up path cannot ingest either.
            return wake;
        }
        if let Some(entry) = self.project.lock().await.get(&owner) {
            let mut retained = entry
                .history
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let installed = retained.is_none();
            if installed {
                *retained = Some(history);
                entry.state.mark_history_pending();
            }
            let blocked = !installed && entry.state.history_is_blocked();
            drop(retained);
            if !blocked {
                entry.state.wake_history();
            }
        }
        wake
    }

    pub async fn ensure_profile_with_history(
        &self,
        database_path: std::path::PathBuf,
        database: RegisteredGlobalDbLeaseV1,
        history: SharedSessionHistoricalIngestor,
    ) -> SessionTemporalRefreshWake {
        let wake = self.ensure_profile(database_path.clone(), database).await;
        if session_ingest_disabled() {
            // Dev/profiling switch: never install a history ingestor, so the
            // project-open catch-up path cannot ingest either.
            return wake;
        }
        if let Some(entry) = self.profile.lock().await.get(&database_path) {
            let mut retained = entry
                .history
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let installed = retained.is_none();
            if installed {
                *retained = Some(history);
                entry.state.mark_history_pending();
            }
            let blocked = !installed && entry.state.history_is_blocked();
            drop(retained);
            if !blocked {
                entry.state.wake_history();
            }
        }
        wake
    }

    pub async fn rekey_project(
        &self,
        old_owner: &StoreOwnerKey,
        new_owner: StoreOwnerKey,
        database: RegisteredGlobalDbLeaseV1,
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
            self.retire(vec![(
                SessionTemporalRefreshOwner::Project(old_owner.clone()),
                entry,
            )])
            .join()
            .await;
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
        self.settle_retired(&SessionTemporalRefreshOwner::Project(new_owner.clone()))
            .await;
        let entry = self.spawn_entry(database, route, None);
        if let Some(staging) = staging {
            staging.cancel();
            staging.transfer_requests_to(&entry.state);
        }
        project.insert(new_owner, entry);
    }

    pub async fn retire_project(&self, owner: &StoreOwnerKey) {
        let _lifecycle = self.project_lifecycle.lock().await;
        let retired = self.project.lock().await.remove(owner);
        let owner = SessionTemporalRefreshOwner::Project(owner.clone());
        self.retire(retired.map(|entry| (owner, entry)).into_iter().collect())
            .join()
            .await;
    }

    pub async fn cancel_historical_ingest(&self) {
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
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let _guard = self.shutdown_guard.lock().await;
        let _project_lifecycle = self.project_lifecycle.lock().await;
        let mut project = self.project.lock().await;
        let mut profile = self.profile.lock().await;
        // Removal and retirement share one poll: no await separates an entry
        // leaving its map from the retirement that cancels and owns it.
        let mut entries =
            std::mem::take(&mut *self.retired.lock().unwrap_or_else(PoisonError::into_inner));
        entries.extend(
            project
                .drain()
                .map(|(owner, entry)| (SessionTemporalRefreshOwner::Project(owner), entry)),
        );
        entries.extend(
            profile
                .drain()
                .map(|(path, entry)| (SessionTemporalRefreshOwner::Profile(path), entry)),
        );
        let retirement = self.retire(entries);
        drop((project, profile));
        retirement.join().await;
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn project_state(
        &self,
        owner: &StoreOwnerKey,
    ) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.project
            .lock()
            .await
            .get(owner)
            .map(|entry| Arc::clone(&entry.state))
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn profile_worker_status(
        &self,
        database_path: &std::path::Path,
    ) -> SessionTemporalRefreshWorkerStatus {
        self.profile.lock().await.get(database_path).map_or_else(
            || SessionTemporalRefreshWake::unavailable().status(),
            |entry| entry.wake.status(),
        )
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn project_worker_count(&self) -> usize {
        self.project.lock().await.len()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn profile_worker_count(&self) -> usize {
        self.profile.lock().await.len()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn profile_pass_count(&self, database_path: &std::path::Path) -> usize {
        self.profile
            .lock()
            .await
            .get(database_path)
            .map_or(0, |entry| entry.state.pass_count.load(Ordering::Acquire))
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn wait_profile_idle(
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

pub(crate) fn session_refresh_retry_delay(
    class: SessionTemporalRefreshRetryClass,
    attempt: u32,
) -> Duration {
    let shift_cap = match class {
        SessionTemporalRefreshRetryClass::Storage => 5,
        SessionTemporalRefreshRetryClass::Projector => 16,
        SessionTemporalRefreshRetryClass::Deadline => 6,
    };
    tracedecay_host_admission::replay_backoff(attempt, shift_cap)
}
