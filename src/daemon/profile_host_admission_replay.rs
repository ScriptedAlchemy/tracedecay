//! Coalesced per-profile host-admission replay.
//!
//! Handshake paths must only kick this worker. Replay never runs under a
//! client permit wait, and concurrent kicks collapse into one bounded pass.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
#[cfg(test)]
use tokio::task::JoinSet;

use crate::application::host_admission::{
    HostAdmissionOutcome, ReplayPassDecision, SharedHostAdmissionBroker, classify_replay_pass,
    replay_backoff,
};

use super::log_daemon_event;

const REPLAY_BACKOFF_SHIFT_CAP: u32 = 16;
const IDLE_EVICTION_AFTER: Duration = Duration::from_secs(30);
const BOOTSTRAP_RUNNING: u8 = 0;
const BOOTSTRAP_READY: u8 = 1;
const BOOTSTRAP_TERMINAL: u8 = 2;
const BOOTSTRAP_CANCELLED: u8 = 3;

pub(super) type ProfileHostAdmissionBootstrapOperation =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = crate::errors::Result<()>> + Send>> + Send + Sync>;

#[cfg(test)]
type ReplayPassOverride = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
        + Send
        + Sync,
>;

pub(super) struct ProfileHostAdmissionReplayRegistry {
    workers: Arc<tokio::sync::Mutex<HashMap<PathBuf, ReplayWorkerEntry>>>,
    bootstrap_workers:
        Arc<tokio::sync::Mutex<HashMap<PathBuf, ProfileHostAdmissionBootstrapEntry>>>,
    shutting_down: AtomicBool,
    idle_eviction_after: Duration,
}

struct ReplayWorkerEntry {
    worker: Arc<ProfileHostAdmissionReplayWorker>,
    task: JoinHandle<()>,
}

struct ProfileHostAdmissionBootstrapEntry {
    worker: Arc<ProfileHostAdmissionBootstrapWorker>,
    task: JoinHandle<()>,
}

struct ProfileHostAdmissionBootstrapWorker {
    state: AtomicU8,
    attempt_count: AtomicUsize,
    backoff_count: AtomicUsize,
    completed: Notify,
    cancelled: AtomicBool,
    cancellation: Notify,
}

struct ProfileHostAdmissionReplayWorker {
    broker: SharedHostAdmissionBroker,
    profile_root: PathBuf,
    dirty: AtomicBool,
    busy: AtomicBool,
    pass_count: AtomicUsize,
    backoff_count: AtomicUsize,
    idle: Notify,
    wake: Notify,
    cancelled: AtomicBool,
    cancellation: Notify,
    #[cfg(test)]
    pass_override: Option<ReplayPassOverride>,
}

impl Default for ProfileHostAdmissionReplayRegistry {
    fn default() -> Self {
        Self {
            workers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            bootstrap_workers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            shutting_down: AtomicBool::new(false),
            idle_eviction_after: IDLE_EVICTION_AFTER,
        }
    }
}

impl Drop for ProfileHostAdmissionReplayRegistry {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        if let Ok(workers) = self.workers.try_lock() {
            for entry in workers.values() {
                entry.worker.cancel();
            }
        }
        if let Ok(workers) = self.bootstrap_workers.try_lock() {
            for entry in workers.values() {
                entry.worker.cancel();
            }
        }
    }
}

impl ProfileHostAdmissionReplayRegistry {
    pub(super) async fn ensure_bootstrap(
        &self,
        profile_root: &Path,
        operation: ProfileHostAdmissionBootstrapOperation,
    ) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let mut workers = self.bootstrap_workers.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let Some(existing) = workers.get(profile_root) {
            let state = existing.worker.state.load(Ordering::Acquire);
            if matches!(state, BOOTSTRAP_READY | BOOTSTRAP_TERMINAL) || !existing.task.is_finished()
            {
                return;
            }
            // A cancelled or panicked task must not permanently suppress a
            // later authenticated request while the registry is still live.
            workers.remove(profile_root);
        }

        let worker = Arc::new(ProfileHostAdmissionBootstrapWorker::new());
        let task_worker = Arc::clone(&worker);
        let task = tokio::spawn(async move {
            task_worker.run(operation).await;
        });
        workers.insert(
            profile_root.to_path_buf(),
            ProfileHostAdmissionBootstrapEntry { worker, task },
        );
    }

    pub(super) async fn ensure(
        &self,
        broker_path: &Path,
        profile_root: &Path,
        broker: &SharedHostAdmissionBroker,
    ) {
        let worker = Arc::new(ProfileHostAdmissionReplayWorker::new(
            broker,
            profile_root,
            #[cfg(test)]
            None,
        ));
        self.ensure_worker(broker_path, worker).await;
    }

    #[cfg(test)]
    pub(super) async fn ensure_with_pass_override(
        &self,
        broker_path: &Path,
        profile_root: &Path,
        broker: &SharedHostAdmissionBroker,
        pass_override: ReplayPassOverride,
    ) {
        let worker = Arc::new(ProfileHostAdmissionReplayWorker::new(
            broker,
            profile_root,
            Some(pass_override),
        ));
        self.ensure_worker(broker_path, worker).await;
    }

    async fn ensure_worker(
        &self,
        broker_path: &Path,
        candidate: Arc<ProfileHostAdmissionReplayWorker>,
    ) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let mut workers = self.workers.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let Some(existing) = workers.get(broker_path) {
            existing.worker.kick();
            return;
        }

        candidate.kick();
        let worker = Arc::clone(&candidate);
        let worker_path = broker_path.to_path_buf();
        let workers_weak = Arc::downgrade(&self.workers);
        let idle_eviction_after = self.idle_eviction_after;
        let task = tokio::spawn(async move {
            worker.run(idle_eviction_after).await;
            let Some(workers) = workers_weak.upgrade() else {
                return;
            };
            let mut workers = workers.lock().await;
            if workers
                .get(&worker_path)
                .is_some_and(|entry| Arc::ptr_eq(&entry.worker, &worker))
            {
                workers.remove(&worker_path);
            }
        });
        workers.insert(
            broker_path.to_path_buf(),
            ReplayWorkerEntry {
                worker: candidate,
                task,
            },
        );
    }

    pub(super) async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let replay_entries = {
            let mut workers = self.workers.lock().await;
            workers.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        let bootstrap_entries = {
            let mut workers = self.bootstrap_workers.lock().await;
            workers.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in &replay_entries {
            entry.worker.cancel();
        }
        for entry in &bootstrap_entries {
            entry.worker.cancel();
        }
        for entry in replay_entries {
            let _ = entry.task.await;
        }
        for entry in bootstrap_entries {
            let _ = entry.task.await;
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_idle(&self, broker_path: &Path, timeout: Duration) -> bool {
        let worker = {
            let workers = self.workers.lock().await;
            workers
                .get(broker_path)
                .map(|entry| Arc::clone(&entry.worker))
        };
        let Some(worker) = worker else {
            return true;
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if worker.is_idle().await {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::select! {
                () = worker.idle.notified() => {}
                () = tokio::time::sleep(remaining) => {
                    return worker.is_idle().await;
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn pass_count(&self, broker_path: &Path) -> usize {
        let workers = self.workers.lock().await;
        workers
            .get(broker_path)
            .map_or(0, |entry| entry.worker.pass_count.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(super) async fn backoff_count(&self, broker_path: &Path) -> usize {
        let workers = self.workers.lock().await;
        workers.get(broker_path).map_or(0, |entry| {
            entry.worker.backoff_count.load(Ordering::Acquire)
        })
    }

    #[cfg(test)]
    async fn worker_count(&self) -> usize {
        self.workers.lock().await.len()
    }

    #[cfg(test)]
    async fn bootstrap_worker_count(&self) -> usize {
        self.bootstrap_workers.lock().await.len()
    }

    #[cfg(test)]
    async fn bootstrap_attempt_count(&self, profile_root: &Path) -> usize {
        self.bootstrap_workers
            .lock()
            .await
            .get(profile_root)
            .map_or(0, |entry| {
                entry.worker.attempt_count.load(Ordering::Acquire)
            })
    }

    #[cfg(test)]
    async fn bootstrap_backoff_count(&self, profile_root: &Path) -> usize {
        self.bootstrap_workers
            .lock()
            .await
            .get(profile_root)
            .map_or(0, |entry| {
                entry.worker.backoff_count.load(Ordering::Acquire)
            })
    }

    #[cfg(test)]
    async fn wait_bootstrap_completed(&self, profile_root: &Path, timeout: Duration) -> bool {
        let worker = {
            let workers = self.bootstrap_workers.lock().await;
            workers
                .get(profile_root)
                .map(|entry| Arc::clone(&entry.worker))
        };
        let Some(worker) = worker else {
            return true;
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let completed = worker.completed.notified();
            if worker.state.load(Ordering::Acquire) != BOOTSTRAP_RUNNING {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::select! {
                () = completed => {}
                () = tokio::time::sleep(remaining) => {
                    return worker.state.load(Ordering::Acquire) != BOOTSTRAP_RUNNING;
                }
            }
        }
    }

    #[cfg(test)]
    fn with_idle_eviction_after(idle_eviction_after: Duration) -> Self {
        let mut registry = Self::default();
        registry.idle_eviction_after = idle_eviction_after;
        registry
    }
}

impl ProfileHostAdmissionBootstrapWorker {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(BOOTSTRAP_RUNNING),
            attempt_count: AtomicUsize::new(0),
            backoff_count: AtomicUsize::new(0),
            completed: Notify::new(),
            cancelled: AtomicBool::new(false),
            cancellation: Notify::new(),
        }
    }

    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancellation.notify_waiters();
        }
    }

    fn finish(&self, state: u8) {
        self.state.store(state, Ordering::Release);
        self.completed.notify_waiters();
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

    async fn run(&self, operation: ProfileHostAdmissionBootstrapOperation) {
        let mut consecutive_retryable = 0u32;
        loop {
            self.attempt_count.fetch_add(1, Ordering::AcqRel);
            let result = tokio::select! {
                () = self.wait_for_cancellation() => {
                    self.finish(BOOTSTRAP_CANCELLED);
                    return;
                }
                result = operation() => result,
            };
            match result {
                Ok(()) => {
                    if consecutive_retryable > 0 {
                        log_daemon_event(
                            "profile_host_admission_bootstrap_recovered",
                            &[("attempts", (consecutive_retryable + 1).to_string())],
                        );
                    }
                    self.finish(BOOTSTRAP_READY);
                    return;
                }
                Err(error) => {
                    let (reason_code, retryable) = error
                        .project_route_context()
                        .map_or(("authority_unavailable", true), |(reason, retryable, _)| {
                            (reason, retryable)
                        });
                    if !retryable {
                        log_daemon_event(
                            "profile_host_admission_bootstrap_stopped",
                            &[("reason_code", reason_code.to_owned())],
                        );
                        self.finish(BOOTSTRAP_TERMINAL);
                        return;
                    }
                    consecutive_retryable = consecutive_retryable.saturating_add(1);
                    self.backoff_count.fetch_add(1, Ordering::AcqRel);
                    if consecutive_retryable == 1 {
                        log_daemon_event(
                            "profile_host_admission_bootstrap_retry",
                            &[("reason_code", reason_code.to_owned())],
                        );
                    }
                    tokio::select! {
                        () = self.wait_for_cancellation() => {
                            self.finish(BOOTSTRAP_CANCELLED);
                            return;
                        }
                        () = tokio::time::sleep(profile_replay_backoff(consecutive_retryable)) => {}
                    }
                }
            }
        }
    }
}

impl ProfileHostAdmissionReplayWorker {
    fn new(
        broker: &SharedHostAdmissionBroker,
        profile_root: &Path,
        #[cfg(test)] pass_override: Option<ReplayPassOverride>,
    ) -> Self {
        Self {
            broker: Arc::clone(broker),
            profile_root: profile_root.to_path_buf(),
            dirty: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            pass_count: AtomicUsize::new(0),
            backoff_count: AtomicUsize::new(0),
            idle: Notify::new(),
            wake: Notify::new(),
            cancelled: AtomicBool::new(false),
            cancellation: Notify::new(),
            #[cfg(test)]
            pass_override,
        }
    }

    fn kick(&self) {
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancellation.notify_waiters();
        }
    }

    #[cfg(test)]
    async fn is_idle(&self) -> bool {
        !self.busy.load(Ordering::Acquire)
            && !self.dirty.load(Ordering::Acquire)
            && !self.broker.has_pending_replay().await
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

    async fn run(&self, idle_eviction_after: Duration) {
        let mut consecutive_retryable = 0u32;
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            // Drain work that arrived before this wait (broker create / admit kicks).
            while self.dirty.load(Ordering::Acquire) || self.broker.has_pending_replay().await {
                let _ = self.dirty.swap(false, Ordering::AcqRel);
                self.busy.store(true, Ordering::Release);
                self.pass_count.fetch_add(1, Ordering::AcqRel);
                let pending_before = self.broker.pending_replay_count().await.unwrap_or(0);
                let outcome = tokio::select! {
                    () = self.wait_for_cancellation() => return,
                    outcome = self.run_pass() => outcome,
                };
                let pending_after = self.broker.pending_replay_count().await.unwrap_or(0);
                match classify_replay_pass(pending_before, pending_after, &outcome) {
                    ReplayPassDecision::ProgressPending => {
                        consecutive_retryable = 0;
                        tokio::task::yield_now().await;
                    }
                    ReplayPassDecision::Backoff => {
                        consecutive_retryable = consecutive_retryable.saturating_add(1);
                        self.backoff_count.fetch_add(1, Ordering::AcqRel);
                        self.dirty.store(true, Ordering::Release);
                        tokio::select! {
                            () = self.wait_for_cancellation() => return,
                            () = tokio::time::sleep(profile_replay_backoff(consecutive_retryable)) => {}
                        }
                    }
                    ReplayPassDecision::Stop => {
                        consecutive_retryable = 0;
                        log_daemon_event(
                            "profile_host_admission_replay_stopped",
                            &[(
                                "reason_code",
                                outcome
                                    .reason_code
                                    .unwrap_or("host_admission_unavailable")
                                    .to_string(),
                            )],
                        );
                        // Non-retryable failure: stop until the next explicit kick.
                        break;
                    }
                    ReplayPassDecision::Requeue => {
                        consecutive_retryable = 0;
                    }
                }
            }
            self.busy.store(false, Ordering::Release);
            self.idle.notify_waiters();
            tokio::select! {
                () = self.wait_for_cancellation() => return,
                () = self.wake.notified() => {}
                () = self.broker.wait_for_replay_request() => {}
                () = tokio::time::sleep(idle_eviction_after) => {
                    if !self.dirty.load(Ordering::Acquire)
                        && !self.broker.has_pending_replay().await
                        && Arc::strong_count(&self.broker) <= 2
                    {
                        return;
                    }
                }
            }
        }
    }

    async fn run_pass(&self) -> HostAdmissionOutcome {
        #[cfg(test)]
        if let Some(pass_override) = &self.pass_override {
            return pass_override().await;
        }
        crate::mcp::tools::replay_projectless_hermes_host_admission(
            &self.broker,
            &self.profile_root,
        )
        .await
    }
}

pub(super) fn profile_replay_backoff(attempt: u32) -> Duration {
    replay_backoff(attempt, REPLAY_BACKOFF_SHIFT_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn profile_replay_backoff_grows_then_caps() {
        assert_eq!(profile_replay_backoff(1), Duration::from_millis(25));
        assert_eq!(profile_replay_backoff(2), Duration::from_millis(50));
        assert_eq!(profile_replay_backoff(3), Duration::from_millis(100));
        assert_eq!(profile_replay_backoff(20), Duration::from_secs(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simultaneous_bootstrap_ensures_coalesce_and_cache_readiness() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let registry = Arc::new(ProfileHostAdmissionReplayRegistry::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = Arc::clone(&attempts);
        let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
            let attempts = Arc::clone(&operation_attempts);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::AcqRel);
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok(())
            })
        });

        let mut tasks = JoinSet::new();
        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            let profile_root = profile_root.clone();
            let operation = Arc::clone(&operation);
            tasks.spawn(async move {
                registry.ensure_bootstrap(&profile_root, operation).await;
            });
        }
        while tasks.join_next().await.is_some() {}
        assert!(
            registry
                .wait_bootstrap_completed(&profile_root, Duration::from_secs(2))
                .await,
            "coalesced bootstrap must complete"
        );
        assert_eq!(attempts.load(Ordering::Acquire), 1);
        assert_eq!(registry.bootstrap_worker_count().await, 1);

        registry
            .ensure_bootstrap(&profile_root, Arc::clone(&operation))
            .await;
        assert_eq!(registry.bootstrap_attempt_count(&profile_root).await, 1);
        assert_eq!(attempts.load(Ordering::Acquire), 1);
        registry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bootstrap_retries_transient_failure_without_another_ensure() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = Arc::clone(&attempts);
        let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
            let attempts = Arc::clone(&operation_attempts);
            Box::pin(async move {
                let attempt = attempts.fetch_add(1, Ordering::AcqRel);
                if attempt < 2 {
                    Err(crate::errors::TraceDecayError::project_route(
                        "test_bootstrap_unavailable",
                        true,
                        "transient test failure",
                    ))
                } else {
                    Ok(())
                }
            })
        });

        registry.ensure_bootstrap(&profile_root, operation).await;
        assert!(
            registry
                .wait_bootstrap_completed(&profile_root, Duration::from_secs(2))
                .await,
            "retrying bootstrap must recover without another request"
        );
        assert_eq!(attempts.load(Ordering::Acquire), 3);
        assert_eq!(registry.bootstrap_attempt_count(&profile_root).await, 3);
        assert_eq!(registry.bootstrap_backoff_count(&profile_root).await, 2);
        registry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bootstrap_shutdown_cancels_and_joins_in_flight_operation() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let started = Arc::new(Notify::new());
        let operation_started = Arc::clone(&started);
        let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
            let started = Arc::clone(&operation_started);
            Box::pin(async move {
                started.notify_one();
                std::future::pending::<crate::errors::Result<()>>().await
            })
        });

        registry.ensure_bootstrap(&profile_root, operation).await;
        started.notified().await;
        tokio::time::timeout(Duration::from_secs(1), registry.shutdown())
            .await
            .expect("shutdown must cancel and join bootstrap workers");
        assert_eq!(registry.bootstrap_worker_count().await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simultaneous_ensures_coalesce_to_one_pass() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let db_path = crate::sessions::user_sessions_db_path(&profile_root);
        let (runtime, _) =
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
                .unwrap();
        let broker =
            Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let passes = Arc::new(AtomicUsize::new(0));
        let passes_for_override = Arc::clone(&passes);
        let pass_override: Arc<
            dyn Fn() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>,
                > + Send
                + Sync,
        > = Arc::new(move || {
            let passes = Arc::clone(&passes_for_override);
            Box::pin(async move {
                passes.fetch_add(1, Ordering::AcqRel);
                tokio::time::sleep(Duration::from_millis(40)).await;
                HostAdmissionOutcome::accepted_for_replay()
            })
        });

        registry
            .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
            .await;

        tokio::time::sleep(Duration::from_millis(5)).await;
        let mut kick_tasks = JoinSet::new();
        for _ in 0..8 {
            let registry_workers = {
                let workers = registry.workers.lock().await;
                Arc::clone(&workers.get(&db_path).expect("worker").worker)
            };
            kick_tasks.spawn(async move {
                registry_workers.kick();
            });
        }
        while kick_tasks.join_next().await.is_some() {}
        assert!(
            registry.wait_idle(&db_path, Duration::from_secs(2)).await,
            "coalesced worker must become idle"
        );
        let observed = registry.pass_count(&db_path).await;
        assert!(
            observed <= 3,
            "simultaneous kicks must coalesce; observed {observed} passes"
        );
        assert!(observed >= 1);
        assert_eq!(passes.load(Ordering::Acquire), observed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retryable_failures_apply_bounded_backoff() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let db_path = crate::sessions::user_sessions_db_path(&profile_root);
        let (runtime, _) =
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
                .unwrap();
        let broker =
            Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_override = Arc::clone(&attempts);
        let pass_override: Arc<
            dyn Fn() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>,
                > + Send
                + Sync,
        > = Arc::new(move || {
            let attempts = Arc::clone(&attempts_for_override);
            Box::pin(async move {
                let n = attempts.fetch_add(1, Ordering::AcqRel);
                if n < 2 {
                    HostAdmissionOutcome::retained_unavailable("test_retryable")
                } else {
                    HostAdmissionOutcome::accepted_for_replay()
                }
            })
        });

        let started = tokio::time::Instant::now();
        registry
            .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
            .await;
        assert!(
            registry.wait_idle(&db_path, Duration::from_secs(2)).await,
            "retryable worker must become idle after success"
        );
        let elapsed = started.elapsed();
        assert!(
            registry.backoff_count(&db_path).await >= 2,
            "retryable outcomes must count backoff sleeps"
        );
        assert!(
            elapsed >= Duration::from_millis(25 + 50),
            "retryable backoff must delay at least the first two intervals; elapsed={elapsed:?}"
        );
        assert_eq!(attempts.load(Ordering::Acquire), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_no_progress_passes_apply_bounded_backoff() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let db_path = crate::sessions::user_sessions_db_path(&profile_root);
        let (runtime, _) =
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
                .unwrap();
        let broker =
            Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
        broker.admit("test:pending", b"pending").await.unwrap();
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let pass_override = Arc::new(|| {
            Box::pin(async { HostAdmissionOutcome::accepted_for_replay() })
                as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
        });

        registry
            .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
            .await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.backoff_count(&db_path).await < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pending no-progress replay must back off instead of spinning");
        assert!(registry.pass_count(&db_path).await <= 3);
        registry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_cancels_and_joins_an_in_flight_pass() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let db_path = crate::sessions::user_sessions_db_path(&profile_root);
        let (runtime, _) =
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
                .unwrap();
        let broker =
            Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
        let registry = ProfileHostAdmissionReplayRegistry::default();
        let started = Arc::new(Notify::new());
        let started_for_override = Arc::clone(&started);
        let pass_override = Arc::new(move || {
            let started = Arc::clone(&started_for_override);
            Box::pin(async move {
                started.notify_one();
                std::future::pending::<HostAdmissionOutcome>().await
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
        });

        registry
            .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
            .await;
        started.notified().await;
        tokio::time::timeout(Duration::from_secs(1), registry.shutdown())
            .await
            .expect("shutdown must cancel and join replay workers");

        assert_eq!(registry.worker_count().await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_worker_is_evicted_after_the_bound() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let db_path = crate::sessions::user_sessions_db_path(&profile_root);
        let (runtime, _) =
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
                .unwrap();
        let broker =
            Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
        let registry =
            ProfileHostAdmissionReplayRegistry::with_idle_eviction_after(Duration::from_millis(20));
        let pass_override = Arc::new(|| {
            Box::pin(async { HostAdmissionOutcome::accepted_for_replay() })
                as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
        });

        registry
            .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
            .await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.worker_count().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("idle replay worker must be evicted");
    }
}
