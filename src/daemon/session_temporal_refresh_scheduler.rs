use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::{TemporalCoverageCountsV1, UtcMicros};
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancellationRequestV1,
    SessionRefreshCompletionRequestV1, SessionRefreshFailureCodeV1, SessionRefreshFailureRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshStore, SessionStoreError,
    SessionTemporalProjectionBatchV1,
};

use super::StoreOwnerKey;
use crate::global_db::GlobalDb;
use crate::store::{
    GlobalDbSessionTemporalStore, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};

const MAX_PENDING_REFRESH_REQUESTS: usize = 128;

#[derive(Default)]
struct SessionTemporalRefreshWakeState {
    dirty: AtomicBool,
    requests: std::sync::Mutex<VecDeque<SessionRefreshBeginOrJoinRequestV1>>,
    terminal_attempts: std::sync::Mutex<HashSet<String>>,
    recovery_cycle_pending: std::sync::Mutex<VecDeque<String>>,
    busy: AtomicBool,
    pass_count: std::sync::atomic::AtomicUsize,
    wake: tokio::sync::Notify,
    idle: tokio::sync::Notify,
    cancelled: AtomicBool,
    cancellation: tokio::sync::Notify,
}

impl SessionTemporalRefreshWakeState {
    fn handle(self: &Arc<Self>) -> SessionTemporalRefreshWake {
        let route = Arc::new(SessionTemporalRefreshWakeRoute {
            target: std::sync::RwLock::new(Arc::downgrade(self)),
        });
        SessionTemporalRefreshWake { route }
    }

    fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    fn take_requests(&self, limit: usize) -> Vec<SessionRefreshBeginOrJoinRequestV1> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = limit.min(requests.len());
        requests.drain(..count).collect()
    }

    fn requeue_request(&self, request: SessionRefreshBeginOrJoinRequestV1) {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !requests
            .iter()
            .any(|pending| pending.is_equivalent_to(&request))
        {
            requests.push_front(request);
        }
    }

    fn transfer_requests_to(&self, target: &Self) {
        let requests = {
            let mut requests = self
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            requests.drain(..).collect::<Vec<_>>()
        };
        for request in requests {
            target.requeue_request(request);
        }
        if self.take_dirty() || target.has_requests() {
            target.wake();
        }
    }

    fn has_requests(&self) -> bool {
        !self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    }

    fn claim_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) -> bool {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(recovery.operation_id().as_str().to_string())
    }

    fn release_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(recovery.operation_id().as_str());
    }

    fn wake(&self) {
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    fn cancel(&self) {
        let _requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancellation.notify_waiters();
            self.wake.notify_waiters();
        }
    }

    async fn wait_for_cancellation(&self) {
        loop {
            let notified = self.cancellation.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        !self.busy.load(Ordering::Acquire) && !self.dirty.load(Ordering::Acquire)
    }
}

struct TerminalAttemptGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    recovery: &'a SessionRefreshRecoveryV1,
    retain: bool,
}

impl<'a> TerminalAttemptGuard<'a> {
    fn new(
        state: &'a SessionTemporalRefreshWakeState,
        recovery: &'a SessionRefreshRecoveryV1,
    ) -> Self {
        Self {
            state,
            recovery,
            retain: false,
        }
    }

    fn retain(&mut self) {
        self.retain = true;
    }
}

impl Drop for TerminalAttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.retain {
            self.state.release_terminal_attempt(self.recovery);
        }
    }
}

struct PendingBeginRequestGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    request: Option<SessionRefreshBeginOrJoinRequestV1>,
}

impl<'a> PendingBeginRequestGuard<'a> {
    fn new(
        state: &'a SessionTemporalRefreshWakeState,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> Self {
        Self {
            state,
            request: Some(request),
        }
    }

    fn request(&self) -> &SessionRefreshBeginOrJoinRequestV1 {
        self.request.as_ref().expect("pending request disarmed")
    }

    fn disarm(&mut self) {
        self.request = None;
    }
}

impl Drop for PendingBeginRequestGuard<'_> {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            self.state.requeue_request(request);
        }
    }
}

struct RecoverySelectionGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    pending: VecDeque<String>,
}

impl<'a> RecoverySelectionGuard<'a> {
    fn new(state: &'a SessionTemporalRefreshWakeState, pending: Vec<String>) -> Self {
        Self {
            state,
            pending: pending.into(),
        }
    }

    fn complete(&mut self, operation: &str) {
        // Resolve by identity so skipped/missing recoveries cannot desync the
        // local queue from the operations actually projected this pass.
        if let Some(index) = self.pending.iter().position(|item| item == operation) {
            self.pending.remove(index);
        }
    }
}

impl Drop for RecoverySelectionGuard<'_> {
    fn drop(&mut self) {
        let mut cycle = self
            .state
            .recovery_cycle_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while let Some(operation) = self.pending.pop_back() {
            if !cycle.contains(&operation) {
                cycle.push_front(operation);
            }
        }
    }
}

struct SessionTemporalRefreshWakeRoute {
    target: std::sync::RwLock<std::sync::Weak<SessionTemporalRefreshWakeState>>,
}

#[derive(Clone)]
pub(crate) struct SessionTemporalRefreshWake {
    route: Arc<SessionTemporalRefreshWakeRoute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Slice 3 consumes the queued-request disposition.
pub(crate) enum SessionTemporalRefreshWakeDisposition {
    Enqueued,
    Coalesced,
    Saturated,
}

impl SessionTemporalRefreshWake {
    fn target(&self) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.route
            .target
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade()
    }

    fn bind(&self, state: &Arc<SessionTemporalRefreshWakeState>) {
        *self
            .route
            .target
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(state);
    }

    #[cfg(test)]
    fn same_route(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.route, &other.route)
    }

    pub(crate) fn wake(&self) {
        if let Some(state) = self.target() {
            state.wake();
        }
    }

    #[allow(dead_code)] // Slice 3 maps admitted source frontiers into begin requests.
    pub(crate) fn request(
        &self,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> SessionTemporalRefreshWakeDisposition {
        let Some(state) = self.target() else {
            return SessionTemporalRefreshWakeDisposition::Saturated;
        };
        let disposition = {
            let mut requests = state
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.cancelled.load(Ordering::Acquire) {
                SessionTemporalRefreshWakeDisposition::Saturated
            } else if requests
                .iter()
                .any(|pending| pending.is_equivalent_to(&request))
            {
                SessionTemporalRefreshWakeDisposition::Coalesced
            } else if requests.len() >= MAX_PENDING_REFRESH_REQUESTS {
                SessionTemporalRefreshWakeDisposition::Saturated
            } else {
                requests.push_back(request);
                SessionTemporalRefreshWakeDisposition::Enqueued
            }
        };
        if disposition != SessionTemporalRefreshWakeDisposition::Saturated {
            state.wake();
        }
        disposition
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionTemporalRefreshRetryClass {
    Storage,
    Projector,
    Deadline,
}

fn session_refresh_retry_delay(class: SessionTemporalRefreshRetryClass, attempt: u32) -> Duration {
    let shift_cap = match class {
        SessionTemporalRefreshRetryClass::Storage => 5,
        SessionTemporalRefreshRetryClass::Projector => 16,
        SessionTemporalRefreshRetryClass::Deadline => 6,
    };
    crate::application::host_admission::replay_backoff(attempt, shift_cap)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SessionTemporalRefreshPolicy {
    max_begin_requests_per_pass: usize,
    max_operations_per_pass: usize,
    operation_deadline: Duration,
}

impl Default for SessionTemporalRefreshPolicy {
    fn default() -> Self {
        Self {
            max_begin_requests_per_pass: 32,
            max_operations_per_pass: 16,
            operation_deadline: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // Slice 3's projector constructs these effects.
pub(super) enum SessionTemporalRefreshEffect {
    Projection {
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
    },
    Fail(SessionRefreshFailureRequestV1),
    Cancel(SessionRefreshCancellationRequestV1),
    Deferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Slice 3 classifies projector failures.
pub(super) enum SessionTemporalRefreshProjectorErrorClass {
    Retryable,
    Terminal,
}

#[derive(Debug)]
#[allow(dead_code)] // Slice 3 returns this through the projector port.
pub(super) struct SessionTemporalRefreshProjectorError {
    class: SessionTemporalRefreshProjectorErrorClass,
    code: String,
}

#[allow(dead_code)] // Slice 3 constructs classified projector failures.
impl SessionTemporalRefreshProjectorError {
    pub(super) fn retryable(code: impl Into<String>) -> Self {
        Self {
            class: SessionTemporalRefreshProjectorErrorClass::Retryable,
            code: code.into(),
        }
    }

    pub(super) fn terminal(code: impl Into<String>) -> Self {
        Self {
            class: SessionTemporalRefreshProjectorErrorClass::Terminal,
            code: code.into(),
        }
    }
}

pub(super) type SessionTemporalRefreshProjectionFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    SessionTemporalRefreshEffect,
                    SessionTemporalRefreshProjectorError,
                >,
            > + Send
            + 'a,
    >,
>;

pub(super) trait SessionTemporalRefreshProjector: Send + Sync {
    fn project<'a>(
        &'a self,
        database: &'a Arc<GlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a>;
}

#[cfg(test)]
struct DeferredSessionTemporalProjector;

#[cfg(test)]
impl SessionTemporalRefreshProjector for DeferredSessionTemporalProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<GlobalDb>,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async { Ok(SessionTemporalRefreshEffect::Deferred) })
    }
}

struct CanonicalSessionTemporalProjector;

impl SessionTemporalRefreshProjector for CanonicalSessionTemporalProjector {
    fn project<'a>(
        &'a self,
        database: &'a Arc<GlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async move {
            match database
                .materialize_session_temporal_refresh_batch_result(&recovery)
                .await
            {
                Ok(Some((progress, batch))) => {
                    Ok(SessionTemporalRefreshEffect::Projection { progress, batch })
                }
                // Empty remaining range is a durable no-op: terminalize with an
                // empty complete progress batch instead of deferring forever.
                Ok(None) => canonical_noop_complete_effect(&recovery),
                Err(error) if error.is_storage() => Err(
                    SessionTemporalRefreshProjectorError::retryable("source_busy"),
                ),
                Err(_) => Err(SessionTemporalRefreshProjectorError::terminal(
                    "projector_failed",
                )),
            }
        })
    }
}

fn refresh_clock_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn zero_refresh_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

fn canonical_noop_complete_effect(
    recovery: &SessionRefreshRecoveryV1,
) -> std::result::Result<SessionTemporalRefreshEffect, SessionTemporalRefreshProjectorError> {
    let next_batch = match recovery.restart_state() {
        SessionRefreshRestartStateV1::BeginProjection => 0,
        SessionRefreshRestartStateV1::ResumeProjection { next_batch_ordinal } => next_batch_ordinal,
        // Ready-to-complete recoveries are finalized by the pass loop; keep
        // them deferred if a projector is invoked defensively.
        SessionRefreshRestartStateV1::ReadyToComplete => {
            return Ok(SessionTemporalRefreshEffect::Deferred);
        }
    };
    let committed = recovery.target_frontier().observed_through();
    let frontier = SessionRefreshFrontierV1::new(committed, committed)
        .map_err(|_| SessionTemporalRefreshProjectorError::terminal("projector_failed"))?;
    let coverage = recovery
        .progress()
        .map(|progress| *progress.coverage())
        .unwrap_or_else(zero_refresh_coverage);
    let committed_records = recovery
        .progress()
        .map(SessionRefreshProgressV1::committed_records)
        .unwrap_or(0);
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        frontier,
        coverage,
        next_batch.saturating_add(1),
        committed_records,
        refresh_clock_micros(),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .and_then(|batch| batch.with_checkpoint(next_batch, committed, committed))
    .map_err(|_| SessionTemporalRefreshProjectorError::terminal("projector_failed"))?;
    Ok(SessionTemporalRefreshEffect::Projection { progress, batch })
}

fn durable_projector_failure_code(code: &str) -> String {
    match SessionRefreshFailureCodeV1::new(code) {
        Ok(code) => code.as_str().to_string(),
        Err(_) => "projector_failed".to_string(),
    }
}

#[derive(Default, Debug, Eq, PartialEq)]
struct SessionTemporalRefreshPassReport {
    begun: usize,
    joined: usize,
    projected_batches: usize,
    completed: usize,
    failed: usize,
    cancelled: usize,
    deferred: usize,
    retryable_errors: usize,
    terminal_errors: usize,
    deadline_errors: usize,
    saturated: bool,
    retry_class: Option<SessionTemporalRefreshRetryClass>,
    last_error: Option<String>,
}

impl SessionTemporalRefreshPassReport {
    fn observe_retry(&mut self, class: SessionTemporalRefreshRetryClass) {
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
    task: tokio::task::JoinHandle<()>,
}

impl SessionTemporalRefreshSchedulerEntry {
    async fn shutdown(self) {
        self.state.cancel();
        let mut task = self.task;
        if tokio::time::timeout(super::DAEMON_TASK_ABORT_DEADLINE, &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

pub(super) struct SessionTemporalRefreshSchedulerRegistry {
    project: tokio::sync::Mutex<HashMap<StoreOwnerKey, SessionTemporalRefreshSchedulerEntry>>,
    profile: tokio::sync::Mutex<HashMap<std::path::PathBuf, SessionTemporalRefreshSchedulerEntry>>,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    policy: SessionTemporalRefreshPolicy,
    shutting_down: AtomicBool,
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
                entry.state.cancel();
            }
        }
        if let Ok(profile) = self.profile.try_lock() {
            for entry in profile.values() {
                entry.state.cancel();
            }
        }
    }
}

impl SessionTemporalRefreshSchedulerRegistry {
    fn spawn_entry(
        &self,
        database: Arc<GlobalDb>,
        route: Option<SessionTemporalRefreshWake>,
    ) -> SessionTemporalRefreshSchedulerEntry {
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let wake = route.unwrap_or_else(|| state.handle());
        wake.bind(&state);
        let worker_state = Arc::clone(&state);
        let projector = Arc::clone(&self.projector);
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
                    policy,
                ));
                let Some(result) = workers.join_next().await else {
                    return;
                };
                match result {
                    Ok(()) => return,
                    Err(error)
                        if error.is_panic() && !worker_state.cancelled.load(Ordering::Acquire) =>
                    {
                        panic_attempt = panic_attempt.saturating_add(1);
                        worker_state.busy.store(false, Ordering::Release);
                        worker_state.wake();
                        tokio::select! {
                            () = worker_state.wait_for_cancellation() => return,
                            () = tokio::time::sleep(session_refresh_retry_delay(
                                SessionTemporalRefreshRetryClass::Projector,
                                panic_attempt,
                            )) => {}
                        }
                    }
                    Err(_) => return,
                }
            }
        });
        SessionTemporalRefreshSchedulerEntry { state, wake, task }
    }

    pub(super) async fn ensure_project(
        &self,
        owner: StoreOwnerKey,
        database: Arc<GlobalDb>,
    ) -> SessionTemporalRefreshWake {
        if self.shutting_down.load(Ordering::Acquire) {
            return inert_session_temporal_refresh_wake();
        }
        let _lifecycle = self.project_lifecycle.lock().await;
        if self
            .retired_project_owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            let finished = project.remove(&owner).expect("finished entry disappeared");
            let route = finished.wake.clone();
            finished.shutdown().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            let entry = self.spawn_entry(database, Some(route));
            let wake = entry.wake.clone();
            project.insert(owner, entry);
            return wake;
        }
        if let Some(entry) = project.get(&owner) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        let entry = self.spawn_entry(database, None);
        let wake = entry.wake.clone();
        project.insert(owner, entry);
        wake
    }

    pub(super) async fn ensure_profile(
        &self,
        database_path: std::path::PathBuf,
        database: Arc<GlobalDb>,
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
            let finished = profile
                .remove(&database_path)
                .expect("finished entry disappeared");
            let route = finished.wake.clone();
            finished.shutdown().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return inert_session_temporal_refresh_wake();
            }
            let entry = self.spawn_entry(database, Some(route));
            let wake = entry.wake.clone();
            profile.insert(database_path, entry);
            return wake;
        }
        if let Some(entry) = profile.get(&database_path) {
            entry.wake.wake();
            return entry.wake.clone();
        }
        let entry = self.spawn_entry(database, None);
        let wake = entry.wake.clone();
        profile.insert(database_path, entry);
        wake
    }

    pub(super) async fn rekey_project(
        &self,
        old_owner: &StoreOwnerKey,
        new_owner: StoreOwnerKey,
        database: Arc<GlobalDb>,
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
                .unwrap_or_else(|error| error.into_inner());
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
        let entry = self.spawn_entry(database, route);
        if let Some(staging) = staging {
            staging.cancel();
            staging.transfer_requests_to(&entry.state);
        }
        project.insert(new_owner, entry);
    }

    pub(super) async fn retire_project(&self, owner: &StoreOwnerKey) {
        let _lifecycle = self.project_lifecycle.lock().await;
        if let Some(entry) = self.project.lock().await.remove(owner) {
            entry.shutdown().await;
        }
    }

    pub(super) async fn owns_project_database_paths(
        &self,
        database_paths: &HashSet<std::path::PathBuf>,
    ) -> bool {
        self.project
            .lock()
            .await
            .keys()
            .any(|owner| database_paths.contains(&owner.graph_db_path))
    }

    pub(super) async fn shutdown(&self) {
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
    async fn project_state(
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
    async fn project_worker_count(&self) -> usize {
        self.project.lock().await.len()
    }

    #[cfg(test)]
    async fn profile_worker_count(&self) -> usize {
        self.profile.lock().await.len()
    }

    #[cfg(test)]
    async fn profile_pass_count(&self, database_path: &std::path::Path) -> usize {
        self.profile
            .lock()
            .await
            .get(database_path)
            .map_or(0, |entry| entry.state.pass_count.load(Ordering::Acquire))
    }

    #[cfg(test)]
    async fn wait_profile_idle(&self, database_path: &std::path::Path, timeout: Duration) -> bool {
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
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    state.cancel();
    state.handle()
}

async fn run_session_temporal_refresh_scheduler(
    database: Arc<GlobalDb>,
    state: Arc<SessionTemporalRefreshWakeState>,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    policy: SessionTemporalRefreshPolicy,
) {
    let mut retry_attempt = 0u32;
    loop {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        while state.take_dirty() {
            state.busy.store(true, Ordering::Release);
            state.pass_count.fetch_add(1, Ordering::AcqRel);
            let pass =
                run_session_temporal_refresh_pass(&database, &state, projector.as_ref(), policy);
            tokio::pin!(pass);
            let report = tokio::select! {
                biased;
                () = state.wait_for_cancellation() => return,
                report = &mut pass => report,
            };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            if let Some(class) = report.retry_class {
                retry_attempt = retry_attempt.saturating_add(1);
                state.dirty.store(true, Ordering::Release);
                tokio::select! {
                    () = state.wait_for_cancellation() => return,
                    () = state.wake.notified() => {}
                    () = tokio::time::sleep(session_refresh_retry_delay(class, retry_attempt)) => {}
                }
            } else if state.has_requests()
                || report.begun > 0
                || report.saturated
                || report.projected_batches > 0
            {
                retry_attempt = 0;
                state.dirty.store(true, Ordering::Release);
                tokio::task::yield_now().await;
            } else {
                retry_attempt = 0;
            }
        }
        state.busy.store(false, Ordering::Release);
        state.idle.notify_waiters();
        let wake = state.wake.notified();
        if state.dirty.load(Ordering::Acquire) {
            continue;
        }
        tokio::select! {
            () = state.wait_for_cancellation() => return,
            () = wake => {}
        }
    }
}

fn classify_store_error(error: &SessionStoreError) -> SessionTemporalRefreshRetryClass {
    if error.is_storage() {
        SessionTemporalRefreshRetryClass::Storage
    } else {
        SessionTemporalRefreshRetryClass::Projector
    }
}

async fn process_refresh_begin_requests(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) {
    for _ in 0..limit {
        let Some(request) = state.take_requests(1).pop() else {
            break;
        };
        let mut pending = PendingBeginRequestGuard::new(state, request);
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        match store
            .begin_or_join_session_refresh(pending.request().clone())
            .await
        {
            Ok(receipt) => {
                pending.disarm();
                match receipt.disposition() {
                    tracedecay_store::SessionRefreshDispositionV1::Started => report.begun += 1,
                    tracedecay_store::SessionRefreshDispositionV1::Joined => report.joined += 1,
                }
            }
            Err(error) if error.is_storage() => {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                break;
            }
            Err(_) => {
                pending.disarm();
                report.terminal_errors += 1;
            }
        }
    }
    report.saturated |= state.has_requests();
}

async fn begin_admitted_session_refreshes(
    database: &GlobalDb,
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let mut requests = match database
        .pending_session_temporal_refresh_requests_result(limit.saturating_add(1))
        .await
    {
        Ok(requests) => requests,
        Err(error) => {
            if classify_store_error(&error) == SessionTemporalRefreshRetryClass::Storage {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return;
        }
    };
    report.saturated |= requests.len() > limit;
    requests.truncate(limit);
    for request in requests {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        match store.begin_or_join_session_refresh(request).await {
            Ok(receipt) => match receipt.disposition() {
                tracedecay_store::SessionRefreshDispositionV1::Started => report.begun += 1,
                tracedecay_store::SessionRefreshDispositionV1::Joined => report.joined += 1,
            },
            Err(error) if error.is_storage() => {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            }
            Err(_) => report.terminal_errors += 1,
        }
    }
}

async fn complete_ready_refresh(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if !state.claim_terminal_attempt(recovery) {
        return;
    }
    let mut attempt = TerminalAttemptGuard::new(state, recovery);
    let Some(progress) = recovery.progress() else {
        attempt.retain();
        report.terminal_errors += 1;
        return;
    };
    let request = match SessionRefreshCompletionRequestV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    ) {
        Ok(request) => request,
        Err(_) => {
            attempt.retain();
            report.terminal_errors += 1;
            return;
        }
    };
    match store.complete_session_refresh(request).await {
        Ok(_) => {
            report.completed += 1;
        }
        Err(error) if error.is_storage() => {
            report.last_error = Some(format!("{error:?}"));
            report.retryable_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
        }
        Err(error) => {
            attempt.retain();
            report.last_error = Some(format!("{error:?}"));
            report.terminal_errors += 1;
        }
    }
}

fn record_projector_error(
    error: SessionTemporalRefreshProjectorError,
    report: &mut SessionTemporalRefreshPassReport,
) {
    report.last_error = Some(error.code);
    match error.class {
        SessionTemporalRefreshProjectorErrorClass::Retryable => {
            report.retryable_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Projector);
        }
        SessionTemporalRefreshProjectorErrorClass::Terminal => {
            report.terminal_errors += 1;
        }
    }
}

async fn apply_refresh_effect(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    recovery: &SessionRefreshRecoveryV1,
    effect: SessionTemporalRefreshEffect,
    report: &mut SessionTemporalRefreshPassReport,
) {
    match effect {
        SessionTemporalRefreshEffect::Projection { progress, batch } => {
            match store
                .persist_session_refresh_projection_batch(progress, batch)
                .await
            {
                Ok(_) => report.projected_batches += 1,
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Fail(request) => {
            if !state.claim_terminal_attempt(recovery) {
                return;
            }
            let mut attempt = TerminalAttemptGuard::new(state, recovery);
            match store.fail_session_refresh(request).await {
                Ok(_) => {
                    report.failed += 1;
                }
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    attempt.retain();
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Cancel(request) => {
            if !state.claim_terminal_attempt(recovery) {
                return;
            }
            let mut attempt = TerminalAttemptGuard::new(state, recovery);
            match store.cancel_session_refresh(request).await {
                Ok(_) => {
                    report.cancelled += 1;
                }
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    attempt.retain();
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Deferred => report.deferred += 1,
    }
}

async fn project_running_refresh(
    database: &Arc<GlobalDb>,
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let deadline_at = tokio::time::Instant::now() + policy.operation_deadline;
    let projection = projector.project(database, recovery.clone());
    tokio::pin!(projection);
    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);
    let effect = tokio::select! {
        biased;
        () = state.wait_for_cancellation() => return,
        () = &mut deadline => {
            report.deadline_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
            return;
        }
        effect = &mut projection => effect,
    };
    let effect = match effect {
        Ok(effect) => effect,
        Err(error) if error.class == SessionTemporalRefreshProjectorErrorClass::Retryable => {
            record_projector_error(error, report);
            return;
        }
        Err(error) => {
            let failure_code = durable_projector_failure_code(&error.code);
            report.last_error = Some(failure_code.clone());
            let (frontier, coverage) = if let Some(progress) = recovery.progress() {
                (progress.frontier(), *progress.coverage())
            } else {
                let Ok(frontier) = SessionRefreshFrontierV1::new(
                    recovery.target_frontier().observed_through(),
                    recovery.source_frontier(),
                ) else {
                    report.terminal_errors += 1;
                    return;
                };
                (frontier, zero_refresh_coverage())
            };
            let request = match SessionRefreshFailureRequestV1::new(
                recovery.operation_id().clone(),
                recovery.session_id().clone(),
                frontier,
                coverage,
                failure_code,
            ) {
                Ok(request) => request,
                Err(_) => {
                    report.terminal_errors += 1;
                    return;
                }
            };
            SessionTemporalRefreshEffect::Fail(request)
        }
    };
    if state.cancelled.load(Ordering::Acquire) {
        return;
    }
    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        () = state.wait_for_cancellation() => {}
        () = &mut deadline => {
            report.deadline_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
        }
        () = apply_refresh_effect(store, state, recovery, effect, report) => {}
    }
}

fn recovery_key(recovery: &SessionRefreshRecoveryV1) -> String {
    format!(
        "{}\0{}",
        recovery.session_id().as_str(),
        recovery.operation_id().as_str()
    )
}

async fn run_session_temporal_refresh_pass(
    database: &Arc<GlobalDb>,
    state: &Arc<SessionTemporalRefreshWakeState>,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
) -> SessionTemporalRefreshPassReport {
    let store = GlobalDbSessionTemporalStore::new(database.as_ref());
    let mut report = SessionTemporalRefreshPassReport::default();
    if state.cancelled.load(Ordering::Acquire) {
        return report;
    }
    process_refresh_begin_requests(
        &store,
        state,
        policy.max_begin_requests_per_pass,
        &mut report,
    )
    .await;
    begin_admitted_session_refreshes(
        database,
        &store,
        state,
        policy.max_begin_requests_per_pass,
        &mut report,
    )
    .await;
    let mut recoveries = match store.running_session_refreshes().await {
        Ok(recoveries) => recoveries,
        Err(error) => {
            if classify_store_error(&error) == SessionTemporalRefreshRetryClass::Storage {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return report;
        }
    };
    recoveries.sort_by_cached_key(recovery_key);
    let ordered_keys = recoveries.iter().map(recovery_key).collect::<Vec<_>>();
    let current_keys = ordered_keys.iter().cloned().collect::<HashSet<_>>();
    let (selected_keys, recoveries_remaining) = {
        let mut pending = state
            .recovery_cycle_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending.retain(|operation| current_keys.contains(operation));
        if pending.is_empty() {
            pending.extend(ordered_keys);
        }
        let limit = policy.max_operations_per_pass.max(1);
        let mut selected = Vec::with_capacity(limit.min(pending.len()));
        for _ in 0..limit {
            let Some(operation) = pending.pop_front() else {
                break;
            };
            selected.push(operation);
        }
        let remaining = !pending.is_empty();
        (selected, remaining)
    };
    let mut recoveries_by_key = recoveries
        .into_iter()
        .map(|recovery| (recovery_key(&recovery), recovery))
        .collect::<HashMap<_, _>>();
    let mut selection = RecoverySelectionGuard::new(state, selected_keys.clone());
    let selected = selected_keys
        .into_iter()
        .filter_map(|operation| recoveries_by_key.remove(&operation))
        .collect::<Vec<_>>();
    report.saturated |= recoveries_remaining;
    for recovery in selected {
        let operation = recovery_key(&recovery);
        if state.cancelled.load(Ordering::Acquire) {
            return report;
        }
        match recovery.restart_state() {
            SessionRefreshRestartStateV1::ReadyToComplete => {
                if tokio::time::timeout(
                    policy.operation_deadline,
                    complete_ready_refresh(&store, state, &recovery, &mut report),
                )
                .await
                .is_err()
                {
                    report.deadline_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
                }
            }
            SessionRefreshRestartStateV1::BeginProjection
            | SessionRefreshRestartStateV1::ResumeProjection { .. } => {
                project_running_refresh(
                    database,
                    &store,
                    state,
                    projector,
                    policy,
                    &recovery,
                    &mut report,
                )
                .await;
            }
        }
        selection.complete(&operation);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
        CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
        DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId, TemporalCoverageCountsV1, UtcMicros,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
        SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
        SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshReceiptRequestV1,
        SessionRefreshStore, SessionRefreshTerminalStateV1, SessionTemporalProjectionBatchV1,
        build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
    };

    fn sanitization_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).unwrap(),
                ComponentVersion::new("sanitizer.refresh-scheduler-test.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(payload).unwrap()),
        )
        .unwrap()
    }

    fn canonical_observation(
        session_id: &SessionId,
        ordinal: u64,
        text: &str,
    ) -> DurableObservationV1 {
        let provider = ProviderId::new(format!("cursor-refresh-{ordinal}")).unwrap();
        let source =
            ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
                .unwrap();
        let generation = ObservationSourceGenerationV1::new(1).unwrap();
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
        let record_id = ObservationId::new(format!("record.refresh-scheduler.{ordinal}")).unwrap();
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider,
            "message",
            record_id.clone(),
            CanonicalObservationRelationsV1::new(session_id.clone()),
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": text}),
                model: Some("model.fixture".to_owned()),
                timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .unwrap();
        let payload = serde_json::to_value(envelope).unwrap();
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            generation,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .unwrap();
        DurableObservationV1::new(
            identity,
            sanitization_receipt(&format!("receipt.refresh-scheduler.{ordinal}"), &payload),
            RetentionClass::new("retention.refresh-scheduler-test").unwrap(),
            payload,
        )
        .unwrap()
    }

    fn anchored_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
        let identity = observation.identity();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            identity.generation(),
            identity.ordering_domain(),
            identity.position().end(),
        )
        .unwrap();
        let write = ObservationWrite::new(observation, None, next_cursor).unwrap();
        let generation =
            ProjectionGenerationId::new("projection.refresh-scheduler-test.v1").unwrap();
        let authorization =
            build_observation_resolution_authorization_v1(write.observation(), "refresh-scheduler")
                .unwrap();
        let anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        AnchoredObservationWrite::new(write, anchor, generation).unwrap()
    }

    async fn admit_canonical_effect(
        db: &Arc<GlobalDb>,
        session_id: &SessionId,
        ordinal: u64,
        text: &str,
    ) {
        let store = crate::store::GlobalDbObservationStore::new(db.as_ref());
        store
            .persist_observation(anchored_write(canonical_observation(
                session_id, ordinal, text,
            )))
            .await
            .unwrap();
        let observation_id = store.next_queued_observation().await.unwrap().unwrap();
        store.project_observation(&observation_id).await.unwrap();
    }

    async fn scalar(db: &GlobalDb, query: &str) -> i64 {
        let mut rows = db.read_connection().query(query, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    fn request(session: &str, observed: u64) -> SessionRefreshBeginOrJoinRequestV1 {
        SessionRefreshBeginOrJoinRequestV1::new(
            SessionId::new(session).unwrap(),
            SessionRefreshFrontierV1::new(observed, 0).unwrap(),
        )
    }

    fn now() -> UtcMicros {
        UtcMicros(
            i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros(),
            )
            .unwrap(),
        )
    }

    fn zero_coverage() -> TemporalCoverageCountsV1 {
        TemporalCoverageCountsV1 {
            visible: 0,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        }
    }

    fn empty_projection_effect(
        recovery: &SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshEffect {
        let next_batch = match recovery.restart_state() {
            SessionRefreshRestartStateV1::BeginProjection => 0,
            SessionRefreshRestartStateV1::ResumeProjection { next_batch_ordinal } => {
                next_batch_ordinal
            }
            SessionRefreshRestartStateV1::ReadyToComplete => unreachable!(),
        };
        let committed = recovery.target_frontier().observed_through();
        let progress = SessionRefreshProgressV1::new(
            recovery.operation_id().clone(),
            recovery.session_id().clone(),
            SessionRefreshFrontierV1::new(committed, committed).unwrap(),
            zero_coverage(),
            next_batch + 1,
            0,
            now(),
        );
        let batch = SessionTemporalProjectionBatchV1::new(
            recovery.session_id().clone(),
            recovery.candidate_generation(),
            recovery.frozen_watermarks().clone(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_checkpoint(next_batch, committed, committed)
        .unwrap();
        SessionTemporalRefreshEffect::Projection { progress, batch }
    }

    struct EmptyProjector {
        calls: std::sync::atomic::AtomicUsize,
        database: std::sync::Mutex<Option<usize>>,
    }

    impl EmptyProjector {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                database: std::sync::Mutex::new(None),
            }
        }
    }

    impl SessionTemporalRefreshProjector for EmptyProjector {
        fn project<'a>(
            &'a self,
            database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            *self.database.lock().unwrap() = Some(Arc::as_ptr(database) as usize);
            Box::pin(async move { Ok(empty_projection_effect(&recovery)) })
        }
    }

    #[test]
    fn equivalent_wakes_coalesce_into_one_pending_pass() {
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let wake = state.handle();

        for _ in 0..32 {
            wake.wake();
        }

        assert!(state.take_dirty());
        assert!(!state.take_dirty());
    }

    #[test]
    fn equivalent_begin_requests_join_before_the_store_boundary() {
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let wake = state.handle();
        let request = request("session.join", 4);

        assert_eq!(
            wake.request(request.clone()),
            SessionTemporalRefreshWakeDisposition::Enqueued
        );
        assert_eq!(
            wake.request(request),
            SessionTemporalRefreshWakeDisposition::Coalesced
        );
        assert_eq!(state.take_requests(8).len(), 1);
    }

    #[test]
    fn retry_classes_use_distinct_bounded_backoff_curves() {
        let storage = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Storage, 99);
        let projector =
            session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Projector, 99);
        let deadline = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Deadline, 99);

        assert!(storage <= std::time::Duration::from_secs(2));
        assert!(projector <= std::time::Duration::from_secs(8));
        assert!(deadline <= std::time::Duration::from_secs(4));
        assert_ne!(storage, projector);
        assert_ne!(projector, deadline);
    }

    #[tokio::test]
    async fn admitted_effect_refreshes_to_a_real_non_empty_active_projection() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("user-sessions.db"))
                .await
                .unwrap(),
        );
        let session_id = SessionId::new("session.refresh.real").unwrap();
        admit_canonical_effect(&db, &session_id, 1, "durable refresh canary").await;

        let first = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(
            first.projected_batches, 1,
            "first refresh pass failed: {first:?}"
        );
        let second = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(second.completed, 1, "completion pass failed: {second:?}");

        let effect_count = scalar(
            db.as_ref(),
            "SELECT COUNT(*) FROM session_temporal_observation_effects
             WHERE session_id = 'session.refresh.real'",
        )
        .await;
        let operation_count = scalar(
            db.as_ref(),
            "SELECT COUNT(*) FROM session_refresh_operations
             WHERE session_id = 'session.refresh.real'",
        )
        .await;
        let occurrence_count = scalar(
            db.as_ref(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.refresh.real'",
        )
        .await;
        assert_eq!(
            occurrence_count, 1,
            "effect_count={effect_count} operation_count={operation_count}"
        );
        assert_eq!(
            scalar(
                db.as_ref(),
                "SELECT COALESCE(SUM(occurrence_count), 0)
                 FROM session_temporal_projection_receipts
                 WHERE session_id = 'session.refresh.real'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn restart_after_materialization_resumes_from_durable_receipts() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let session_id = SessionId::new("session.refresh.materialized-crash").unwrap();
        admit_canonical_effect(&db, &session_id, 2, "materialized crash canary").await;
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let request = db
            .pending_session_temporal_refresh_requests_result(1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        store.begin_or_join_session_refresh(request).await.unwrap();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();

        let materialized = CanonicalSessionTemporalProjector
            .project(&db, recovery)
            .await
            .unwrap();
        match materialized {
            SessionTemporalRefreshEffect::Projection { progress, batch } => {
                assert_eq!(batch.item_count(), 1);
                assert_eq!(progress.committed_records(), 1);
            }
            other => panic!("expected non-empty projection, got {other:?}"),
        }

        let restarted_state = Arc::new(SessionTemporalRefreshWakeState::default());
        let projected = run_session_temporal_refresh_pass(
            &db,
            &restarted_state,
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(projected.projected_batches, 1);
        let completed = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(completed.completed, 1);
        assert_eq!(
            scalar(
                db.as_ref(),
                "SELECT COUNT(*) FROM session_temporal_projection_receipts
                 WHERE session_id = 'session.refresh.materialized-crash'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                db.as_ref(),
                "SELECT COUNT(*) FROM session_occurrences
                 WHERE session_id = 'session.refresh.materialized-crash'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn new_effect_wake_is_bounded_to_its_profile_database() {
        let temp = TempDir::new().unwrap();
        let first_db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("first-sessions.db"))
                .await
                .unwrap(),
        );
        let second_db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("second-sessions.db"))
                .await
                .unwrap(),
        );
        let first_session = SessionId::new("session.refresh.profile-first").unwrap();
        let second_session = SessionId::new("session.refresh.profile-second").unwrap();
        admit_canonical_effect(&first_db, &first_session, 3, "first profile canary").await;
        admit_canonical_effect(&second_db, &second_session, 3, "second profile canary").await;

        let registry = SessionTemporalRefreshSchedulerRegistry::default();
        let first_wake = registry
            .ensure_profile(first_db.db_path().to_path_buf(), Arc::clone(&first_db))
            .await;
        assert!(
            registry
                .wait_profile_idle(first_db.db_path(), Duration::from_secs(2))
                .await
        );
        assert_eq!(
            scalar(
                first_db.as_ref(),
                "SELECT COUNT(*)
                 FROM session_occurrences AS occurrence
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                 WHERE generation.state = 'active'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                second_db.as_ref(),
                "SELECT COUNT(*) FROM session_occurrences"
            )
            .await,
            0
        );

        admit_canonical_effect(&first_db, &first_session, 4, "second first-profile canary").await;
        first_wake.wake();
        assert!(
            registry
                .wait_profile_idle(first_db.db_path(), Duration::from_secs(2))
                .await
        );
        assert_eq!(
            scalar(
                first_db.as_ref(),
                "SELECT COUNT(*)
                 FROM session_occurrences AS occurrence
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                 WHERE generation.state = 'active'"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                second_db.as_ref(),
                "SELECT COUNT(*) FROM session_occurrences"
            )
            .await,
            0
        );
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn restart_finalizes_ready_progress_without_replaying_projection() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let session_id = SessionId::new("session.restart.ready").unwrap();
        let started = store
            .begin_or_join_session_refresh(request(session_id.as_str(), 0))
            .await
            .unwrap();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let coverage = TemporalCoverageCountsV1 {
            visible: 0,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        };
        let progress = SessionRefreshProgressV1::new(
            started.operation_id().clone(),
            session_id.clone(),
            SessionRefreshFrontierV1::new(0, 0).unwrap(),
            coverage,
            1,
            0,
            now(),
        );
        let batch = SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            recovery.candidate_generation(),
            recovery.frozen_watermarks().clone(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_checkpoint(0, 0, 0)
        .unwrap();
        store
            .persist_session_refresh_projection_batch(progress.clone(), batch)
            .await
            .unwrap();
        drop(store);

        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let report = run_session_temporal_refresh_pass(
            &db,
            &state,
            &DeferredSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(report.completed, 1);
        assert_eq!(report.projected_batches, 0);

        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let receipt = store
            .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                started.operation_id().clone(),
                session_id.clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
        assert_eq!(
            receipt,
            store
                .complete_session_refresh(
                    SessionRefreshCompletionRequestV1::new(
                        started.operation_id().clone(),
                        session_id,
                        progress.frontier(),
                        *progress.coverage(),
                    )
                    .unwrap(),
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn restart_resumes_each_committed_boundary_without_writer_fallback() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let wake = state.handle();
        assert_eq!(
            wake.request(request("session.restart.boundaries", 0)),
            SessionTemporalRefreshWakeDisposition::Enqueued
        );
        let projector = EmptyProjector::new();

        let first = run_session_temporal_refresh_pass(
            &db,
            &state,
            &projector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(first.begun, 1);
        assert_eq!(first.projected_batches, 1);
        assert_eq!(projector.calls.load(Ordering::Acquire), 1);
        assert_eq!(
            *projector.database.lock().unwrap(),
            Some(Arc::as_ptr(&db) as usize)
        );

        let restarted_state = Arc::new(SessionTemporalRefreshWakeState::default());
        let second = run_session_temporal_refresh_pass(
            &db,
            &restarted_state,
            &DeferredSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(second.completed, 1);
        assert_eq!(second.projected_batches, 0);
        assert!(
            crate::store::GlobalDbSessionTemporalStore::new(db.as_ref())
                .running_session_refreshes()
                .await
                .unwrap()
                .is_empty()
        );
    }

    struct PrematureFailureProjector {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl SessionTemporalRefreshProjector for PrematureFailureProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                Ok(SessionTemporalRefreshEffect::Fail(
                    tracedecay_store::SessionRefreshFailureRequestV1::new(
                        recovery.operation_id().clone(),
                        recovery.session_id().clone(),
                        SessionRefreshFrontierV1::new(
                            recovery.target_frontier().observed_through(),
                            recovery.source_frontier(),
                        )
                        .unwrap(),
                        zero_coverage(),
                        "projector_failed",
                    )
                    .unwrap(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn failed_terminal_operation_is_not_retried_in_one_owner_generation() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        store
            .begin_or_join_session_refresh(request("session.terminal.once", 0))
            .await
            .unwrap();
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let projector = PrematureFailureProjector {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let first = run_session_temporal_refresh_pass(
            &db,
            &state,
            &projector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        let second = run_session_temporal_refresh_pass(
            &db,
            &state,
            &projector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

        assert_eq!(first.failed, 1);
        assert_eq!(first.terminal_errors, 0);
        assert_eq!(second.terminal_errors, 0);
        assert_eq!(second.failed, 0);
        assert_eq!(projector.calls.load(Ordering::Acquire), 1);
        assert!(store.running_session_refreshes().await.unwrap().is_empty());
    }

    struct TerminalProjector {
        cancel: bool,
    }

    impl SessionTemporalRefreshProjector for TerminalProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            Box::pin(async move {
                let progress = recovery.progress().unwrap();
                if self.cancel {
                    Ok(SessionTemporalRefreshEffect::Cancel(
                        tracedecay_store::SessionRefreshCancellationRequestV1::new(
                            recovery.operation_id().clone(),
                            recovery.session_id().clone(),
                            progress.frontier(),
                            *progress.coverage(),
                        ),
                    ))
                } else {
                    Ok(SessionTemporalRefreshEffect::Fail(
                        tracedecay_store::SessionRefreshFailureRequestV1::new(
                            recovery.operation_id().clone(),
                            recovery.session_id().clone(),
                            progress.frontier(),
                            *progress.coverage(),
                            "projector_failed",
                        )
                        .unwrap(),
                    ))
                }
            })
        }
    }

    async fn begin_with_incomplete_progress(db: &Arc<GlobalDb>, session_id: &SessionId) {
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        store
            .begin_or_join_session_refresh(request(session_id.as_str(), 1))
            .await
            .unwrap();
        let recovery = store
            .session_refresh_recovery(session_id)
            .await
            .unwrap()
            .unwrap();
        let progress = SessionRefreshProgressV1::new(
            recovery.operation_id().clone(),
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
            zero_coverage(),
            1,
            0,
            now(),
        );
        let batch = SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            recovery.candidate_generation(),
            recovery.frozen_watermarks().clone(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_checkpoint(0, 0, 0)
        .unwrap();
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failure_and_cancel_effects_use_typed_terminal_store_operations() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let failed_session = SessionId::new("session.effect.failed").unwrap();
        begin_with_incomplete_progress(&db, &failed_session).await;
        let failed = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalProjector { cancel: false },
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(failed.failed, 1);

        let cancelled_session = SessionId::new("session.effect.cancelled").unwrap();
        begin_with_incomplete_progress(&db, &cancelled_session).await;
        let cancelled = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalProjector { cancel: true },
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(cancelled.cancelled, 1);

        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        assert!(
            store
                .session_refresh_recovery(&failed_session)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .session_refresh_recovery(&cancelled_session)
                .await
                .unwrap()
                .is_none()
        );
    }

    struct BlockingProjector {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl SessionTemporalRefreshProjector for BlockingProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            let started = Arc::clone(&self.started);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                started.notify_one();
                release.notified().await;
                Ok(empty_projection_effect(&recovery))
            })
        }
    }

    #[tokio::test]
    async fn stale_owner_cannot_persist_after_cancellation() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        store
            .begin_or_join_session_refresh(request("session.stale.owner", 0))
            .await
            .unwrap();
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let projector = BlockingProjector {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };
        let pass = tokio::spawn({
            let db = Arc::clone(&db);
            let state = Arc::clone(&state);
            async move {
                run_session_temporal_refresh_pass(
                    &db,
                    &state,
                    &projector,
                    SessionTemporalRefreshPolicy::default(),
                )
                .await
            }
        });
        started.notified().await;
        state.cancel();
        release.notify_one();
        let report = pass.await.unwrap();

        assert_eq!(report.projected_batches, 0);
        assert_eq!(
            store
                .session_refresh_recovery(&SessionId::new("session.stale.owner").unwrap())
                .await
                .unwrap()
                .unwrap()
                .restart_state(),
            SessionRefreshRestartStateV1::BeginProjection
        );
    }

    struct RecordingDeferredProjector {
        sessions: std::sync::Mutex<HashSet<String>>,
    }

    impl RecordingDeferredProjector {
        fn new() -> Self {
            Self {
                sessions: std::sync::Mutex::new(HashSet::new()),
            }
        }

        fn observed_session_count(&self) -> usize {
            self.sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
        }
    }

    impl SessionTemporalRefreshProjector for RecordingDeferredProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            self.sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(recovery.session_id().as_str().to_string());
            Box::pin(async { Ok(SessionTemporalRefreshEffect::Deferred) })
        }
    }

    #[tokio::test]
    async fn saturated_recovery_passes_visit_every_operation_before_idling() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let projector = Arc::new(RecordingDeferredProjector::new());
        let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
        registry.projector = projector.clone();
        registry.policy = SessionTemporalRefreshPolicy {
            max_operations_per_pass: 2,
            ..SessionTemporalRefreshPolicy::default()
        };
        let wake = registry
            .ensure_profile(db.db_path().to_path_buf(), Arc::clone(&db))
            .await;
        for index in 0..3 {
            assert_eq!(
                wake.request(request(&format!("session.saturated.{index}"), 0)),
                SessionTemporalRefreshWakeDisposition::Enqueued
            );
        }

        assert!(
            registry
                .wait_profile_idle(db.db_path(), Duration::from_secs(2))
                .await
        );
        assert_eq!(projector.observed_session_count(), 3);
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn project_retirement_cancels_and_awaits_an_inflight_projector() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let owner = super::super::StoreOwnerKey {
            profile_root: temp.path().to_path_buf(),
            global_db_path: temp.path().join("global.db"),
            project_id: Some("project.retire".to_string()),
            store_root: temp.path().join("store"),
            graph_db_path: temp.path().join("store/graph.db"),
        };
        let started = Arc::new(tokio::sync::Notify::new());
        let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
        registry.projector = Arc::new(BlockingProjector {
            started: Arc::clone(&started),
            release: Arc::new(tokio::sync::Notify::new()),
        });
        let wake = registry
            .ensure_project(owner.clone(), Arc::clone(&db))
            .await;
        assert_eq!(
            wake.request(request("session.retire.inflight", 0)),
            SessionTemporalRefreshWakeDisposition::Enqueued
        );
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_millis(250), registry.retire_project(&owner))
            .await
            .expect("retirement should cancel and await the worker promptly");

        assert_eq!(registry.project_worker_count().await, 0);
        assert_eq!(
            wake.request(request("session.retire.after", 0)),
            SessionTemporalRefreshWakeDisposition::Saturated
        );
    }

    struct PanicOnceProjector {
        panicked: AtomicBool,
    }

    impl SessionTemporalRefreshProjector for PanicOnceProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            let should_panic = !self.panicked.swap(true, Ordering::AcqRel);
            Box::pin(async move {
                assert!(!should_panic, "injected refresh worker panic");
                Ok(empty_projection_effect(&recovery))
            })
        }
    }

    #[tokio::test]
    async fn worker_panic_is_supervised_and_durable_work_resumes_automatically() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
        registry.projector = Arc::new(PanicOnceProjector {
            panicked: AtomicBool::new(false),
        });
        let wake = registry
            .ensure_profile(db.db_path().to_path_buf(), Arc::clone(&db))
            .await;
        assert_eq!(
            wake.request(request("session.worker.restart", 0)),
            SessionTemporalRefreshWakeDisposition::Enqueued
        );
        assert!(
            registry
                .wait_profile_idle(db.db_path(), Duration::from_secs(2))
                .await
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        assert!(store.running_session_refreshes().await.unwrap().is_empty());
        registry.shutdown().await;
    }

    struct PendingProjector;

    impl SessionTemporalRefreshProjector for PendingProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            _recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    struct TerminalErrorProjector;

    impl SessionTemporalRefreshProjector for TerminalErrorProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            _recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            Box::pin(async {
                Err(SessionTemporalRefreshProjectorError::terminal(
                    "source_invalid",
                ))
            })
        }
    }

    #[tokio::test]
    async fn terminal_projector_error_persists_a_failure_receipt() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let started = store
            .begin_or_join_session_refresh(request("session.terminal.error", 0))
            .await
            .unwrap();

        let report = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalErrorProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

        assert_eq!(report.failed, 1);
        let receipt = store
            .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                started.operation_id().clone(),
                started.session_id().clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Failed);
        assert_eq!(receipt.failure_code().unwrap().as_str(), "source_invalid");
    }

    struct NonCanonicalTerminalProjector;

    impl SessionTemporalRefreshProjector for NonCanonicalTerminalProjector {
        fn project<'a>(
            &'a self,
            _database: &'a Arc<GlobalDb>,
            _recovery: SessionRefreshRecoveryV1,
        ) -> SessionTemporalRefreshProjectionFuture<'a> {
            Box::pin(async {
                Err(SessionTemporalRefreshProjectorError::terminal(
                    "Debug { error: \"not a failure code\" }",
                ))
            })
        }
    }

    #[tokio::test]
    async fn noncanonical_terminal_projector_errors_persist_projector_failed() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let started = store
            .begin_or_join_session_refresh(request("session.terminal.noncanonical", 0))
            .await
            .unwrap();

        let report = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &NonCanonicalTerminalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

        assert_eq!(report.failed, 1);
        let receipt = store
            .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                started.operation_id().clone(),
                started.session_id().clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Failed);
        assert_eq!(receipt.failure_code().unwrap().as_str(), "projector_failed");
        assert!(store.running_session_refreshes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn canonical_noop_materialize_terminalizes_with_complete_receipt() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        let store = crate::store::GlobalDbSessionTemporalStore::new(db.as_ref());
        let started = store
            .begin_or_join_session_refresh(request("session.canonical.noop", 0))
            .await
            .unwrap();

        let first = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        let second = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

        assert_eq!(first.projected_batches, 1);
        assert_eq!(first.deferred, 0);
        assert_eq!(second.completed, 1);
        let receipt = store
            .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                started.operation_id().clone(),
                started.session_id().clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
        assert!(store.running_session_refreshes().await.unwrap().is_empty());
    }

    #[test]
    fn recovery_selection_completes_by_identity_when_keys_are_skipped() {
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let mut selection = RecoverySelectionGuard::new(
            &state,
            vec![
                "session.a\0op.a".to_string(),
                "session.b\0op.b".to_string(),
                "session.c\0op.c".to_string(),
            ],
        );
        selection.complete("session.a\0op.a");
        selection.complete("session.c\0op.c");
        drop(selection);

        let pending = state
            .recovery_cycle_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            pending.iter().cloned().collect::<Vec<_>>(),
            vec!["session.b\0op.b".to_string()]
        );
    }

    #[tokio::test]
    async fn operation_deadline_is_bounded_and_retryable_by_class() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("sessions.db"))
                .await
                .unwrap(),
        );
        crate::store::GlobalDbSessionTemporalStore::new(db.as_ref())
            .begin_or_join_session_refresh(request("session.deadline", 0))
            .await
            .unwrap();
        let report = run_session_temporal_refresh_pass(
            &db,
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &PendingProjector,
            SessionTemporalRefreshPolicy {
                operation_deadline: Duration::from_millis(10),
                ..SessionTemporalRefreshPolicy::default()
            },
        )
        .await;

        assert_eq!(report.deadline_errors, 1);
        assert_eq!(
            report.retry_class,
            Some(SessionTemporalRefreshRetryClass::Deadline)
        );
        let retryable = SessionTemporalRefreshProjectorError::retryable("source_busy");
        let terminal = SessionTemporalRefreshProjectorError::terminal("source_invalid");
        assert_eq!(
            retryable.class,
            SessionTemporalRefreshProjectorErrorClass::Retryable
        );
        assert_eq!(
            terminal.class,
            SessionTemporalRefreshProjectorErrorClass::Terminal
        );
    }

    #[tokio::test]
    async fn profile_database_has_one_scheduler_and_equivalent_kicks_coalesce() {
        let temp = TempDir::new().unwrap();
        let db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("user-sessions.db"))
                .await
                .unwrap(),
        );
        let registry = SessionTemporalRefreshSchedulerRegistry::default();

        let first = registry
            .ensure_profile(db.db_path().to_path_buf(), Arc::clone(&db))
            .await;
        let second = registry
            .ensure_profile(db.db_path().to_path_buf(), Arc::clone(&db))
            .await;
        for _ in 0..32 {
            second.wake();
        }

        assert!(first.same_route(&second));
        assert_eq!(registry.profile_worker_count().await, 1);
        assert!(
            registry
                .wait_profile_idle(db.db_path(), Duration::from_secs(2))
                .await
        );
        assert!(registry.profile_pass_count(db.db_path()).await <= 2);
        registry.shutdown().await;
        assert_eq!(registry.profile_worker_count().await, 0);
    }

    #[tokio::test]
    async fn project_rekey_retires_old_owner_before_rebinding_wake() {
        let temp = TempDir::new().unwrap();
        let old_db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("old-sessions.db"))
                .await
                .unwrap(),
        );
        let new_db = Arc::new(
            crate::global_db::GlobalDb::open_at(&temp.path().join("new-sessions.db"))
                .await
                .unwrap(),
        );
        let old_owner = super::super::StoreOwnerKey {
            profile_root: temp.path().to_path_buf(),
            global_db_path: temp.path().join("global.db"),
            project_id: Some("project".to_string()),
            store_root: temp.path().join("old"),
            graph_db_path: temp.path().join("old/graph.db"),
        };
        let new_owner = super::super::StoreOwnerKey {
            store_root: temp.path().join("new"),
            graph_db_path: temp.path().join("new/graph.db"),
            ..old_owner.clone()
        };
        let registry = SessionTemporalRefreshSchedulerRegistry::default();
        let wake = registry
            .ensure_project(old_owner.clone(), Arc::clone(&old_db))
            .await;
        let old_state = registry.project_state(&old_owner).await.unwrap();

        registry
            .rekey_project(&old_owner, new_owner.clone(), Arc::clone(&new_db))
            .await;
        wake.wake();

        assert!(old_state.cancelled.load(Ordering::Acquire));
        assert!(registry.project_state(&old_owner).await.is_none());
        let new_state = registry.project_state(&new_owner).await.unwrap();
        assert!(!Arc::ptr_eq(&old_state, &new_state));
        let stale = registry
            .ensure_project(old_owner.clone(), Arc::clone(&old_db))
            .await;
        assert_eq!(
            stale.request(request("session.rekey.stale-owner", 0)),
            SessionTemporalRefreshWakeDisposition::Saturated
        );
        assert!(registry.project_state(&old_owner).await.is_none());
        assert_eq!(registry.project_worker_count().await, 1);
        assert!(new_state.take_dirty());
        registry.shutdown().await;
    }
}
