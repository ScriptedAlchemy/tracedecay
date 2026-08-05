//! Production lifecycle boundary for the bounded embedding-session pool.
//!
//! A service owns the admitted projection/artifact authority, an owned runtime
//! factory, and the currently published pool. Restart and reload construct a
//! complete replacement before one atomic state swap; callers therefore see
//! either the old pool or the replacement, never a half-reloaded runtime.
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
#[cfg(test)]
use std::sync::RwLockWriteGuard;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinHandle;
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

include!("runtime_service/scheduling.rs");
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

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
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
        let warmed = service.stats();
        assert!(
            warmed.last_cold_load_micros.is_some(),
            "warmup records the cold session load duration"
        );
        assert_eq!(
            warmed,
            SessionPoolStats {
                idle: 1,
                live_sessions: 1,
                resident_bytes: 1024,
                sessions_opened: 1,
                last_cold_load_micros: warmed.last_cold_load_micros,
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

    #[tokio::test]
    async fn publication_installs_runtime_and_current_pointer_before_observation() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let observed_handle = handle.clone();
        let installed = Arc::new(AtomicBool::new(false));
        let installed_for_commit = Arc::clone(&installed);
        let installed_for_observer = Arc::clone(&installed);
        let expected = pointer('e');
        let expected_for_work = expected.clone();
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();

        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            expected.source_generation.clone(),
            1,
            move |_cancellation| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(
                    move || async move { Ok(expected_for_work) },
                )
                .on_success(move |_pointer| {
                    installed_for_commit.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .on_published(move |pointer| async move {
                    let _ = observed_tx.send((
                        installed_for_observer.load(Ordering::SeqCst),
                        observed_handle.current(),
                        pointer,
                    ));
                }))
            },
        )));

        let (runtime_installed, current, observed) =
            tokio::time::timeout(Duration::from_secs(1), observed_rx)
                .await
                .expect("publication observation")
                .expect("publication observer");
        assert!(runtime_installed);
        assert_eq!(current, Some(expected.clone()));
        assert_eq!(observed, expected);
    }

    #[tokio::test]
    async fn shutdown_fences_and_joins_the_active_projection_worker() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.shutdown").expect("source generation"),
            1,
            move |cancellation| async move {
                let _ = started_tx.send(());
                while !cancellation.cancelled() {
                    tokio::task::yield_now().await;
                }
                Err(SemanticRuntimeScheduleFailureV1::Cancelled)
            },
        )));
        started_rx.await.expect("worker started");

        assert!(handle.begin_shutdown());
        assert!(!handle.begin_shutdown(), "shutdown fence is idempotent");
        assert!(!handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.rejected").expect("source generation"),
            1,
            move |_cancellation| async move { Err(SemanticRuntimeScheduleFailureV1::Cancelled) },
        )));

        let receipt = handle
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert!(receipt.is_clean());
        assert_eq!(receipt.joined_workers, 1);
        assert_eq!(receipt.aborted_workers, 0);
        assert_eq!(receipt.remaining_workers, 0);
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Failed {
                reason: SemanticRuntimeScheduleFailureV1::Cancelled,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn expired_shutdown_deadline_reports_retained_unjoined_workers() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.abort-first").expect("source generation"),
            1,
            move |_cancellation| async move {
                let _ = first_started_tx.send(());
                std::future::pending().await
            },
        )));
        first_started_rx.await.expect("first worker started");
        let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.abort-second").expect("source generation"),
            1,
            move |_cancellation| async move {
                let _ = second_started_tx.send(());
                std::future::pending().await
            },
        )));
        second_started_rx.await.expect("second worker started");

        let receipt = handle
            .cancel_and_join_until(tokio::time::Instant::now())
            .await;

        assert!(!receipt.is_clean());
        assert_eq!(receipt.joined_workers, 0);
        assert_eq!(receipt.aborted_workers, 2);
        assert_eq!(receipt.remaining_workers, 2);
        let cleanup = handle
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert!(cleanup.is_clean());
        assert_eq!(cleanup.remaining_workers, 0);
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Failed {
                reason: SemanticRuntimeScheduleFailureV1::Cancelled,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn shutdown_aborts_before_the_shared_deadline_and_joins_cooperative_tasks() {
        let handle = SemanticRuntimeSchedulingHandleV1::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_by_worker = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        assert!(handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.deadline-race").expect("source generation"),
            1,
            move |_cancellation| async move {
                let _drop_signal = DropSignal(dropped_by_worker);
                let _ = started_tx.send(());
                std::future::pending().await
            },
        )));
        started_rx.await.expect("worker started");

        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let shutdown = {
            let handle = handle.clone();
            tokio::spawn(async move {
                tokio::time::timeout_at(deadline, handle.cancel_and_join_until(deadline))
                    .await
                    .expect("shutdown returns before the shared deadline")
            })
        };
        tokio::task::yield_now().await;
        assert!(!dropped.load(Ordering::SeqCst));

        let receipt = shutdown.await.expect("shutdown task joined");
        assert!(dropped.load(Ordering::SeqCst));
        assert!(receipt.is_clean());
        assert_eq!(receipt.aborted_workers, 1);
        assert_eq!(receipt.remaining_workers, 0);
    }
}
