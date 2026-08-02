//! Production lifecycle boundary for the bounded embedding-session pool.
//!
//! A service owns the admitted projection/artifact authority, an owned runtime
//! factory, and the currently published pool. Restart and reload construct a
//! complete replacement before one atomic state swap; callers therefore see
//! either the old pool or the replacement, never a half-reloaded runtime.
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
#[cfg(test)]
use std::sync::RwLockWriteGuard;

use serde::Serialize;
use tracedecay_domain::{CodeGenerationId, ProjectionKeyV1, VectorGenerationIdV1};

use super::fastembed_adapter::FastEmbedEmbeddingRuntime;
use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, CancellationSignal, EmbedError, EmbeddingRuntime,
};
use super::session_pool::{
    PooledSession, SessionAcquireError, SessionPool, SessionPoolConfigError, SessionPoolConfigV1,
    SessionPoolStats, SystemMonotonicClock,
};

/// An owned factory for the root-private production embedding adapter.
///
/// The factory creates runtime adapters only. Artifact selection and
/// admission remain explicit inputs to [`SemanticRuntimeService::reload`].
pub type SharedEmbeddingRuntimeFactory<R> =
    Arc<dyn Fn() -> Result<R, EmbedError> + Send + Sync + 'static>;

/// Owned factory for the only production model implementation. Without the
/// `semantic-fastembed` feature this yields the unavailable stand-in runtime,
/// whose operations fail with a typed runtime failure.
pub fn fastembed_runtime_factory() -> SharedEmbeddingRuntimeFactory<FastEmbedEmbeddingRuntime> {
    Arc::new(|| Ok(FastEmbedEmbeddingRuntime))
}

#[derive(Debug)]
pub enum SemanticRuntimeServiceError {
    Factory(EmbedError),
    PoolConfig(SessionPoolConfigError),
    #[cfg(test)]
    WorkerTerminated,
}

impl fmt::Display for SemanticRuntimeServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factory(error) => write!(f, "embedding runtime factory failed: {error}"),
            Self::PoolConfig(error) => write!(f, "embedding session pool rejected config: {error}"),
            #[cfg(test)]
            Self::WorkerTerminated => write!(f, "embedding runtime worker did not complete"),
        }
    }
}

impl Error for SemanticRuntimeServiceError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub struct RuntimeReloadReportV1 {
    pub prior_generation: u64,
    pub current_generation: u64,
    pub closed_idle_sessions: usize,
}

type SemanticRuntimePrepareFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    >,
>;
type SemanticRuntimeCommitFutureV1 = Pin<
    Box<
        dyn Future<Output = Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1>>
            + Send
            + 'static,
    >,
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticGenerationPointerV1 {
    pub generation: VectorGenerationIdV1,
    pub source_generation: CodeGenerationId,
    pub projection_key: ProjectionKeyV1,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleFailureV1 {
    Artifact,
    Runtime,
    Projection,
    Publication,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleStatusV1 {
    Unavailable,
    Indexing {
        target_generation: CodeGenerationId,
        completed_units: u64,
        total_units: u64,
        prior_generation: Option<VectorGenerationIdV1>,
    },
    Current {
        generation: VectorGenerationIdV1,
    },
    Failed {
        reason: SemanticRuntimeScheduleFailureV1,
        prior_generation: Option<VectorGenerationIdV1>,
    },
}

#[derive(Debug)]
pub struct SemanticRuntimeScheduleCancellationV1 {
    cancelled: AtomicBool,
    completed_units: AtomicU64,
    total_units: u64,
}

impl SemanticRuntimeScheduleCancellationV1 {
    pub fn new(total_units: u64) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            completed_units: AtomicU64::new(0),
            total_units,
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn set_completed_units(&self, completed_units: u64) {
        self.completed_units
            .fetch_max(completed_units.min(self.total_units), Ordering::AcqRel);
    }

    fn completed_units(&self) -> u64 {
        self.completed_units.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl CancellationSignal for SemanticRuntimeScheduleCancellationV1 {
    fn cancelled(&self) -> bool {
        Self::cancelled(self)
    }
}

pub struct PreparedSemanticRuntimeCommitV1 {
    commit: Box<dyn FnOnce() -> SemanticRuntimeCommitFutureV1 + Send + 'static>,
}

impl PreparedSemanticRuntimeCommitV1 {
    pub fn new<Commit, CommitFuture>(commit: Commit) -> Self
    where
        Commit: FnOnce() -> CommitFuture + Send + 'static,
        CommitFuture: Future<Output = Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1>>
            + Send
            + 'static,
    {
        Self {
            commit: Box::new(move || Box::pin(commit())),
        }
    }

    async fn commit(self) -> Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1> {
        (self.commit)().await
    }

    pub fn on_success<Publish>(self, publish: Publish) -> Self
    where
        Publish: FnOnce(&SemanticGenerationPointerV1) -> Result<(), SemanticRuntimeScheduleFailureV1>
            + Send
            + 'static,
    {
        Self::new(move || async move {
            let pointer = self.commit().await?;
            publish(&pointer)?;
            Ok(pointer)
        })
    }
}

pub struct SemanticRuntimeWorkV1 {
    target_generation: CodeGenerationId,
    total_units: u64,
    prepare: Box<
        dyn FnOnce(Arc<SemanticRuntimeScheduleCancellationV1>) -> SemanticRuntimePrepareFutureV1
            + Send
            + 'static,
    >,
}

impl SemanticRuntimeWorkV1 {
    pub fn new<Prepare, PrepareFuture>(
        target_generation: CodeGenerationId,
        total_units: u64,
        prepare: Prepare,
    ) -> Self
    where
        Prepare:
            FnOnce(Arc<SemanticRuntimeScheduleCancellationV1>) -> PrepareFuture + Send + 'static,
        PrepareFuture: Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    {
        Self {
            target_generation,
            total_units: total_units.max(1),
            prepare: Box::new(move |cancellation| Box::pin(prepare(cancellation))),
        }
    }

    pub fn total_units(&self) -> u64 {
        self.total_units
    }
}

struct SemanticRuntimeSchedulingStateV1 {
    sequence: u64,
    status: SemanticRuntimeScheduleStatusV1,
    current: Option<SemanticGenerationPointerV1>,
    cancellation: Option<Arc<SemanticRuntimeScheduleCancellationV1>>,
    committing: bool,
}

impl Default for SemanticRuntimeSchedulingStateV1 {
    fn default() -> Self {
        Self {
            sequence: 0,
            status: SemanticRuntimeScheduleStatusV1::Unavailable,
            current: None,
            cancellation: None,
            committing: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct SemanticRuntimeSchedulingHandleV1 {
    state: Arc<Mutex<SemanticRuntimeSchedulingStateV1>>,
}

impl SemanticRuntimeSchedulingHandleV1 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start one bounded preparation task without waiting for artifact I/O,
    /// model loading, projection, or publication.
    ///
    /// A task already inside its serialized atomic commit is not displaced;
    /// callers receive `false` and may schedule the newer generation again.
    pub fn schedule(&self, work: SemanticRuntimeWorkV1) -> bool {
        let (sequence, cancellation) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.committing {
                return false;
            }
            if let Some(cancellation) = state.cancellation.take() {
                cancellation.cancel();
            }
            state.sequence = state.sequence.wrapping_add(1);
            let sequence = state.sequence;
            let cancellation =
                Arc::new(SemanticRuntimeScheduleCancellationV1::new(work.total_units));
            state.status = SemanticRuntimeScheduleStatusV1::Indexing {
                target_generation: work.target_generation.clone(),
                completed_units: 0,
                total_units: work.total_units,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
            state.cancellation = Some(Arc::clone(&cancellation));
            (sequence, cancellation)
        };

        let handle = self.clone();
        let watcher = self.clone();
        tokio::spawn(async move {
            let worker = tokio::spawn(async move {
                let prepared = (work.prepare)(Arc::clone(&cancellation)).await;
                let prepared = match prepared {
                    Ok(prepared) if !cancellation.cancelled() => prepared,
                    Ok(_) => {
                        handle
                            .finish_failure(sequence, SemanticRuntimeScheduleFailureV1::Cancelled);
                        return;
                    }
                    Err(reason) => {
                        handle.finish_failure(sequence, reason);
                        return;
                    }
                };

                {
                    let mut state = handle.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.sequence != sequence || cancellation.cancelled() || state.committing {
                        return;
                    }
                    state.committing = true;
                }

                let committed = prepared.commit().await;
                let mut state = handle.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.sequence != sequence {
                    state.committing = false;
                    return;
                }
                state.committing = false;
                state.cancellation = None;
                match committed {
                    Ok(pointer) => {
                        state.current = Some(pointer.clone());
                        state.status = SemanticRuntimeScheduleStatusV1::Current {
                            generation: pointer.generation,
                        };
                    }
                    Err(reason) => {
                        state.status = SemanticRuntimeScheduleStatusV1::Failed {
                            reason,
                            prior_generation: state
                                .current
                                .as_ref()
                                .map(|pointer| pointer.generation.clone()),
                        };
                    }
                }
            });
            if worker.await.is_err() {
                watcher.finish_worker_terminated(sequence);
            }
        });
        true
    }

    pub fn status(&self) -> SemanticRuntimeScheduleStatusV1 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut status = state.status.clone();
        if let (
            SemanticRuntimeScheduleStatusV1::Indexing {
                completed_units, ..
            },
            Some(cancellation),
        ) = (&mut status, state.cancellation.as_ref())
        {
            *completed_units = cancellation.completed_units();
        }
        status
    }

    pub fn current(&self) -> Option<SemanticGenerationPointerV1> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current
            .clone()
    }

    pub fn restore_current(&self, pointer: SemanticGenerationPointerV1) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        state.sequence = state.sequence.wrapping_add(1);
        state.committing = false;
        state.current = Some(pointer.clone());
        state.status = SemanticRuntimeScheduleStatusV1::Current {
            generation: pointer.generation,
        };
    }

    pub fn cancel(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.committing {
            return false;
        }
        let Some(cancellation) = state.cancellation.take() else {
            return false;
        };
        cancellation.cancel();
        state.sequence = state.sequence.wrapping_add(1);
        state.status = SemanticRuntimeScheduleStatusV1::Failed {
            reason: SemanticRuntimeScheduleFailureV1::Cancelled,
            prior_generation: state
                .current
                .as_ref()
                .map(|pointer| pointer.generation.clone()),
        };
        true
    }

    fn finish_failure(&self, sequence: u64, reason: SemanticRuntimeScheduleFailureV1) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.sequence == sequence && !state.committing {
            state.cancellation = None;
            state.status = SemanticRuntimeScheduleStatusV1::Failed {
                reason,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
        }
    }

    fn finish_worker_terminated(&self, sequence: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.sequence == sequence {
            state.committing = false;
            state.cancellation = None;
            state.status = SemanticRuntimeScheduleStatusV1::Failed {
                reason: SemanticRuntimeScheduleFailureV1::Runtime,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
        }
    }
}

struct ActiveRuntime<R: EmbeddingRuntime> {
    generation: u64,
    authority: Arc<AdmittedProjectionArtifactV1>,
    #[cfg(test)]
    factory: SharedEmbeddingRuntimeFactory<R>,
    pool: Arc<SessionPool<R, SystemMonotonicClock>>,
}

/// Atomically replaceable production session-pool owner.
pub struct SemanticRuntimeService<R: EmbeddingRuntime> {
    #[cfg(test)]
    config: SessionPoolConfigV1,
    #[cfg(test)]
    lifecycle: Mutex<()>,
    active: RwLock<ActiveRuntime<R>>,
}

impl<R> SemanticRuntimeService<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    /// Build and publish generation one, returning an owned shared service.
    pub fn new_owned(
        authority: Arc<AdmittedProjectionArtifactV1>,
        factory: SharedEmbeddingRuntimeFactory<R>,
        config: SessionPoolConfigV1,
    ) -> Result<Arc<Self>, SemanticRuntimeServiceError> {
        let pool = Self::build_pool(&authority, &factory, &config)?;
        Ok(Arc::new(Self {
            #[cfg(test)]
            config,
            #[cfg(test)]
            lifecycle: Mutex::new(()),
            active: RwLock::new(ActiveRuntime {
                generation: 1,
                authority,
                #[cfg(test)]
                factory,
                pool,
            }),
        }))
    }

    #[cfg(test)]
    pub fn generation(&self) -> u64 {
        self.read_active().generation
    }

    pub fn stats(&self) -> SessionPoolStats {
        self.read_active().pool.stats()
    }

    /// Acquire from one coherent authority/pool snapshot.
    pub fn acquire(&self) -> Result<PooledSession<R, SystemMonotonicClock>, SessionAcquireError> {
        let (authority, pool) = {
            let active = self.read_active();
            (Arc::clone(&active.authority), Arc::clone(&active.pool))
        };
        pool.acquire(&authority)
    }

    /// Open and return one compatible session to the idle pool.
    ///
    /// Callers use this before publishing a replacement runtime so request
    /// threads never pay model/session startup cost.
    pub fn warm_query_session(&self) -> Result<(), SessionAcquireError> {
        let session = self.acquire()?;
        drop(session);
        Ok(())
    }

    /// Recreate the active runtime from its current owned factory.
    #[cfg(test)]
    pub fn restart(&self) -> Result<RuntimeReloadReportV1, SemanticRuntimeServiceError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (authority, factory) = {
            let active = self.read_active();
            (Arc::clone(&active.authority), Arc::clone(&active.factory))
        };
        self.replace_locked(authority, factory)
    }

    /// Recreate the runtime away from retrieval executor threads.
    #[cfg(test)]
    pub async fn restart_async(
        self: &Arc<Self>,
    ) -> Result<RuntimeReloadReportV1, SemanticRuntimeServiceError> {
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || service.restart())
            .await
            .map_err(|_| SemanticRuntimeServiceError::WorkerTerminated)?
    }

    /// Atomically publish a newly admitted authority and runtime factory.
    ///
    /// Construction completes before the write lock is taken. If construction
    /// fails, the current generation remains untouched and usable.
    #[cfg(test)]
    pub fn reload(
        &self,
        authority: Arc<AdmittedProjectionArtifactV1>,
        factory: SharedEmbeddingRuntimeFactory<R>,
    ) -> Result<RuntimeReloadReportV1, SemanticRuntimeServiceError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.replace_locked(authority, factory)
    }

    /// Construct and verify a replacement away from retrieval executor
    /// threads, then publish it with the same atomic swap as [`Self::reload`].
    #[cfg(test)]
    pub async fn reload_async(
        self: &Arc<Self>,
        authority: Arc<AdmittedProjectionArtifactV1>,
        factory: SharedEmbeddingRuntimeFactory<R>,
    ) -> Result<RuntimeReloadReportV1, SemanticRuntimeServiceError> {
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || service.reload(authority, factory))
            .await
            .map_err(|_| SemanticRuntimeServiceError::WorkerTerminated)?
    }

    pub fn active_snapshot(
        &self,
    ) -> (
        u64,
        Arc<AdmittedProjectionArtifactV1>,
        Arc<SessionPool<R, SystemMonotonicClock>>,
    ) {
        let active = self.read_active();
        (
            active.generation,
            Arc::clone(&active.authority),
            Arc::clone(&active.pool),
        )
    }

    #[cfg(test)]
    fn replace_locked(
        &self,
        authority: Arc<AdmittedProjectionArtifactV1>,
        factory: SharedEmbeddingRuntimeFactory<R>,
    ) -> Result<RuntimeReloadReportV1, SemanticRuntimeServiceError> {
        let replacement = Self::build_pool(&authority, &factory, &self.config)?;
        let (prior_generation, old_pool) = {
            let mut active = self.write_active();
            let prior_generation = active.generation;
            let old_pool = Arc::clone(&active.pool);
            *active = ActiveRuntime {
                generation: prior_generation.wrapping_add(1),
                authority,
                #[cfg(test)]
                factory,
                pool: replacement,
            };
            (prior_generation, old_pool)
        };
        let closed_idle_sessions = old_pool.close();
        Ok(RuntimeReloadReportV1 {
            prior_generation,
            current_generation: prior_generation.wrapping_add(1),
            closed_idle_sessions,
        })
    }

    fn build_pool(
        authority: &AdmittedProjectionArtifactV1,
        factory: &SharedEmbeddingRuntimeFactory<R>,
        config: &SessionPoolConfigV1,
    ) -> Result<Arc<SessionPool<R, SystemMonotonicClock>>, SemanticRuntimeServiceError> {
        let runtime = factory().map_err(SemanticRuntimeServiceError::Factory)?;
        runtime
            .verify_artifact_compatibility(authority)
            .map_err(SemanticRuntimeServiceError::Factory)?;
        SessionPool::new(runtime, SystemMonotonicClock::default(), config.clone())
            .map(Arc::new)
            .map_err(SemanticRuntimeServiceError::PoolConfig)
    }

    fn read_active(&self) -> RwLockReadGuard<'_, ActiveRuntime<R>> {
        self.active.read().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn write_active(&self) -> RwLockWriteGuard<'_, ActiveRuntime<R>> {
        self.active.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use super::super::fastembed_adapter::FakeEmbeddingRuntime;
    use super::super::session_pool::test_support::{authority, config};
    use super::*;
    use tracedecay_domain::ManifestDigest;

    fn pointer(value: char) -> SemanticGenerationPointerV1 {
        SemanticGenerationPointerV1 {
            generation: VectorGenerationIdV1::new(
                ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                    .expect("vector generation"),
            ),
            source_generation: CodeGenerationId::new(format!("code-generation.{value}"))
                .expect("source generation"),
            projection_key: authority().projection().projection_key().clone(),
        }
    }

    async fn wait_until(
        handle: &SemanticRuntimeSchedulingHandleV1,
        predicate: impl Fn(&SemanticRuntimeScheduleStatusV1) -> bool,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if predicate(&handle.status()) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduler reached expected state");
    }

    #[test]
    fn query_warmup_opens_and_releases_one_reusable_session() {
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::new(authority()),
            factory,
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");

        service.warm_query_session().expect("warm query session");
        assert_eq!(
            service.stats(),
            SessionPoolStats {
                idle: 1,
                live_sessions: 1,
                resident_bytes: 1024,
                sessions_opened: 1,
                ..SessionPoolStats::default()
            }
        );

        let session = service.acquire().expect("reuse warmed session");
        assert_eq!(service.stats().sessions_opened, 1);
        drop(session);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_reload_keeps_prior_runtime_available_until_atomic_swap() {
        let initial: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::new(authority()),
            initial,
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("initial runtime");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let first_factory_call = Arc::new(AtomicBool::new(true));
        let blocking_factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> = {
            let release_rx = Arc::clone(&release_rx);
            let first_factory_call = Arc::clone(&first_factory_call);
            Arc::new(move || {
                if first_factory_call.swap(false, Ordering::SeqCst) {
                    started_tx.send(()).expect("report replacement start");
                    release_rx
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .recv()
                        .expect("release replacement factory");
                }
                Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024))
            })
        };
        let reload = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .reload_async(Arc::new(authority()), blocking_factory)
                    .await
            })
        };

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("replacement factory started");
        service
            .acquire()
            .expect("prior runtime remains available while replacement loads");
        assert_eq!(service.generation(), 1);
        release_tx.send(()).expect("release replacement factory");
        let report = reload
            .await
            .expect("reload task joined")
            .expect("reload completed");
        assert_eq!(report.prior_generation, 1);
        assert_eq!(report.current_generation, 2);
        assert_eq!(service.generation(), 2);

        let restart = service.restart_async().await.expect("async restart");
        assert_eq!(restart.prior_generation, 2);
        assert_eq!(restart.current_generation, 3);
    }

    #[tokio::test]
    // The injected panic means the commit closure never reads its captured
    // pointer; liveness resolves this lint at the binding, not at `Ok(next)`.
    #[allow(unused_assignments)]
    async fn panicked_publication_worker_fails_closed_and_retains_prior_generation() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let prior_pointer = pointer('a');
        let prior_generation = prior_pointer.generation.clone();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            prior_pointer.source_generation.clone(),
            1,
            move |_cancellation| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(prior_pointer)
                }))
            },
        )));
        wait_until(&handle, |status| {
            matches!(
                status,
                SemanticRuntimeScheduleStatusV1::Current { generation }
                    if generation == &prior_generation
            )
        })
        .await;

        let next = pointer('b');
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            next.source_generation.clone(),
            1,
            move |_cancellation| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    panic!("injected publication worker failure");
                    #[allow(unreachable_code)]
                    Ok(next)
                }))
            },
        )));
        wait_until(&handle, |status| {
            matches!(
                status,
                SemanticRuntimeScheduleStatusV1::Failed {
                    reason: SemanticRuntimeScheduleFailureV1::Runtime,
                    prior_generation: Some(generation),
                } if generation == &prior_generation
            )
        })
        .await;

        assert_eq!(
            handle.current().map(|pointer| pointer.generation),
            Some(prior_generation)
        );
        assert!(
            handle.schedule(SemanticRuntimeWorkV1::new(
                CodeGenerationId::new("code-generation.c").expect("source generation"),
                1,
                move |_cancellation| async move {
                    Err(SemanticRuntimeScheduleFailureV1::Cancelled)
                },
            )),
            "worker termination must not leave publication permanently locked"
        );
    }

    #[tokio::test]
    async fn cancelled_superseding_work_retains_prior_generation_without_publication() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let prior = pointer('a');
        let prior_generation = prior.generation.clone();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            prior.source_generation.clone(),
            1,
            move |_cancellation| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(prior)
                }))
            },
        )));
        wait_until(&handle, |status| {
            matches!(
                status,
                SemanticRuntimeScheduleStatusV1::Current { generation }
                    if generation == &prior_generation
            )
        })
        .await;

        let next = pointer('b');
        let publication_ran = Arc::new(AtomicBool::new(false));
        let publication_ran_for_commit = Arc::clone(&publication_ran);
        let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            next.source_generation.clone(),
            1,
            move |_cancellation| async move {
                let _ = prepared_tx.send(());
                let _ = release_rx.await;
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    publication_ran_for_commit.store(true, Ordering::SeqCst);
                    Ok(next)
                }))
            },
        )));
        prepared_rx.await.expect("superseding work prepared");
        assert!(handle.cancel(), "superseding work remains cancellable");
        let _ = release_tx.send(());
        wait_until(&handle, |status| {
            matches!(
                status,
                SemanticRuntimeScheduleStatusV1::Failed {
                    reason: SemanticRuntimeScheduleFailureV1::Cancelled,
                    prior_generation: Some(generation),
                } if generation == &prior_generation
            )
        })
        .await;

        assert_eq!(
            handle.current().map(|pointer| pointer.generation),
            Some(prior_generation)
        );
        assert!(
            !publication_ran.load(Ordering::SeqCst),
            "cancelled preparation must not enter atomic publication"
        );
    }
}
