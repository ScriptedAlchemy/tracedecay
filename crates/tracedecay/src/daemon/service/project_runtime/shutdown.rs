use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, atomic::Ordering};

use super::{ProjectRuntime, ProjectRuntimeRegistryV1};
use crate::daemon::service::invocation::UnavailableFeedbackCycleRuntimeV1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownState {
    Pending,
    Complete,
    Failed,
}

impl ProjectRuntimeRegistryV1 {
    #[hotpath::measure(label = "daemon.service.project_runtime.retire_roots", future = true)]
    pub(crate) async fn retire_roots(&self, roots: &BTreeSet<PathBuf>) -> bool {
        {
            let mut fences = self.lock_root_fences();
            fences.retired.extend(roots.iter().cloned());
        }
        self.drain_roots(roots).await
    }

    #[hotpath::measure(label = "daemon.service.project_runtime.quiesce_roots", future = true)]
    pub(crate) async fn quiesce_roots(
        &self,
        roots: &BTreeSet<PathBuf>,
    ) -> Option<ProjectRuntimeRootQuiescenceV1> {
        {
            let mut fences = self.lock_root_fences();
            if roots.iter().any(|root| fences.contains(root)) {
                return None;
            }
            fences.quiesced.extend(roots.iter().cloned());
        }
        if !self.drain_roots(roots).await {
            self.release_quiesced_roots(roots);
            return None;
        }
        Some(ProjectRuntimeRootQuiescenceV1 {
            registry: self.clone(),
            roots: roots.clone(),
        })
    }

    async fn drain_roots(&self, roots: &BTreeSet<PathBuf>) -> bool {
        let retired =
            tokio::time::timeout(super::super::super::DAEMON_TASK_ABORT_DEADLINE, async {
                loop {
                    let mut changed = self.reservation_changed.subscribe();
                    let retired = {
                        let fences = self.lock_root_fences();
                        let mut current = self.lock_runtimes();
                        (fences.requests_drained(roots)
                            && roots.iter().all(|root| {
                                current
                                    .get(root)
                                    .is_none_or(|runtime| runtime.reservations.is_empty())
                            }))
                        .then(|| {
                            roots
                                .iter()
                                .filter_map(|root| {
                                    current.remove(root).map(|runtime| (root.clone(), runtime))
                                })
                                .collect::<BTreeMap<_, _>>()
                        })
                    };
                    if let Some(retired) = retired {
                        break retired;
                    }
                    if changed.changed().await.is_err() {
                        break BTreeMap::new();
                    }
                }
            })
            .await;
        match retired {
            Ok(mut runtimes) => {
                let mut clean = shut_down_advisory(&runtimes).await;
                clean &= shut_down_feedback(&runtimes).await;
                clean &= shut_down_semantic(
                    &mut runtimes,
                    tokio::time::Instant::now() + super::super::super::DAEMON_TASK_ABORT_DEADLINE,
                )
                .await;
                clean &= shut_down_observability(&mut runtimes).await;
                shut_down_runtimes(runtimes);
                clean
            }
            Err(_) => false,
        }
    }

    fn release_quiesced_roots(&self, roots: &BTreeSet<PathBuf>) {
        let mut fences = self.lock_root_fences();
        for root in roots {
            fences.quiesced.remove(root);
        }
        drop(fences);
        self.signal_reservation_changed();
    }

    pub(crate) fn begin_shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.signal_reservation_changed();
    }

    /// Shut every project runtime down and leave the registry empty.
    ///
    /// Routers become unavailable before feedback owners drop, Work providers
    /// are joined, and process-wide semantic handles are unregistered.
    #[hotpath::measure(label = "daemon.service.project_runtime.shutdown", future = true)]
    pub(crate) async fn shut_down_all(&self) -> bool {
        self.begin_shutdown();
        let mut shutdown_complete = self.shutdown_complete.subscribe();
        let mut shutdown_task = self.shutdown_task.lock().await;
        if !self.shutdown_started.swap(true, Ordering::AcqRel) {
            let registry = self.clone();
            self.shutdown_complete.send_replace(ShutdownState::Pending);
            let task = tokio::spawn(async move {
                let drain_registry = registry.clone();
                let drained = tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        drain_registry.take_all_blocking()
                    }))
                })
                .await;
                let state = match drained {
                    Ok(Ok(runtimes)) => {
                        if registry.finish_drained_runtimes(runtimes).await {
                            ShutdownState::Complete
                        } else {
                            registry.shutdown_started.store(false, Ordering::Release);
                            ShutdownState::Failed
                        }
                    }
                    _ => {
                        registry.shutdown_started.store(false, Ordering::Release);
                        ShutdownState::Failed
                    }
                };
                registry.shutdown_complete.send_replace(state);
            });
            *shutdown_task = Some(task);
        }
        drop(shutdown_task);
        loop {
            let state = *shutdown_complete.borrow_and_update();
            match state {
                ShutdownState::Complete | ShutdownState::Failed => {
                    self.join_shutdown_task().await;
                    if state == ShutdownState::Complete {
                        return true;
                    }
                    self.force_drop_failed_shutdown_runtimes();
                    return false;
                }
                ShutdownState::Pending => {
                    if shutdown_complete.changed().await.is_err() {
                        self.shutdown_complete.send_replace(ShutdownState::Failed);
                        self.join_shutdown_task().await;
                        self.force_drop_failed_shutdown_runtimes();
                        return false;
                    }
                }
            }
        }
    }

    fn force_drop_failed_shutdown_runtimes(&self) {
        let runtimes = std::mem::take(&mut *self.lock_runtimes());
        shut_down_runtimes(runtimes);
    }

    async fn join_shutdown_task(&self) {
        let result = {
            let mut retained = self.shutdown_task.lock().await;
            let Some(task) = retained.as_mut() else {
                return;
            };
            let result = task.await;
            retained.take();
            result
        };
        if let Err(error) = result {
            self.shutdown_started.store(false, Ordering::Release);
            self.shutdown_complete.send_replace(ShutdownState::Failed);
            tracing::error!(%error, "project runtime shutdown drain task failed");
        }
    }

    fn take_all_blocking(&self) -> BTreeMap<PathBuf, ProjectRuntime> {
        loop {
            let (version, changed) = &*self.reservation_blocking_changed;
            let observed_version = *version
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fences = self.lock_root_fences();
            let mut runtimes = self.lock_runtimes();
            if fences.request_leases.is_empty()
                && runtimes
                    .values()
                    .all(|runtime| runtime.reservations.is_empty())
            {
                break std::mem::take(&mut *runtimes);
            }
            drop(runtimes);
            drop(fences);
            #[cfg(test)]
            if let Some(drain_waiting) = self
                .drain_waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                drain_waiting.send(()).expect("drain-waiting receiver");
            }
            let mut current_version = version
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while *current_version == observed_version {
                current_version = changed
                    .wait(current_version)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    async fn finish_drained_runtimes(
        &self,
        mut runtimes: BTreeMap<PathBuf, ProjectRuntime>,
    ) -> bool {
        let deadline =
            tokio::time::Instant::now() + crate::daemon::core_lifecycle::DAEMON_SHUTDOWN_DEADLINE;
        let mut clean = shut_down_advisory(&runtimes).await;
        clean &= shut_down_feedback(&runtimes).await;
        clean &= shut_down_semantic(&mut runtimes, deadline).await;
        clean &= shut_down_observability(&mut runtimes).await;
        shut_down_runtimes(runtimes);
        clean
    }
}

/// Cancel and join every semantic owner before its project database can enter
/// retirement. Targeted project retirement and whole-daemon shutdown share
/// this exact sequence so neither path can leave a worker holding a counted
/// database client after the runtime map entry has been removed.
async fn shut_down_semantic(
    runtimes: &mut BTreeMap<PathBuf, ProjectRuntime>,
    deadline: tokio::time::Instant,
) -> bool {
    let mut clean = true;
    for runtime in runtimes.values_mut() {
        if let Some(reconciler) = runtime.semantic_activation_reconciler.take() {
            reconciler.cancel_and_join().await;
        }
        if let Some(configuration) = runtime.configuration.as_ref() {
            clean &= configuration
                .semantic_evaluation_workers()
                .cancel_and_join_until(deadline)
                .await
                .is_clean();
        }
        if let Some(semantic) = runtime.semantic.as_ref() {
            clean &= semantic.cancel_and_join_until(deadline).await.is_clean();
        }
    }
    clean
}

async fn shut_down_advisory(runtimes: &BTreeMap<PathBuf, ProjectRuntime>) -> bool {
    let mut clean = true;
    for runtime in runtimes.values() {
        if let Some(advisory) = runtime.advisory.as_ref() {
            clean &= advisory.shutdown().await;
        }
    }
    clean
}

async fn shut_down_feedback(runtimes: &BTreeMap<PathBuf, ProjectRuntime>) -> bool {
    let mut clean = true;
    for (project_root, runtime) in runtimes {
        let Some(feedback) = runtime.feedback.as_ref() else {
            continue;
        };
        if let Err(error) = feedback.runtime().close_and_drain_observations().await {
            tracing::warn!(
                project = %project_root.display(),
                %error,
                "project feedback observation shutdown was incomplete"
            );
            clean = false;
        }
    }
    clean
}

pub(crate) struct ProjectRuntimeRootQuiescenceV1 {
    registry: ProjectRuntimeRegistryV1,
    roots: BTreeSet<PathBuf>,
}

impl Drop for ProjectRuntimeRootQuiescenceV1 {
    fn drop(&mut self) {
        self.registry.release_quiesced_roots(&self.roots);
    }
}

/// Stop Work recovery producers, then close and flush each project producer
/// before its registered database and dependent runtime owners are dropped. A
/// failed flush is terminal: the producer has entered shutdown and is removed
/// rather than being republished as a live component on a retry.
async fn shut_down_observability(runtimes: &mut BTreeMap<PathBuf, ProjectRuntime>) -> bool {
    let mut clean = true;
    for (project_root, runtime) in runtimes.iter_mut() {
        if let Some(work) = runtime.work.as_ref() {
            work.shut_down_background_recovery().await;
        }
        let Some(observability) = runtime.observability.take() else {
            continue;
        };
        if let Err(error) = observability.shutdown().await {
            tracing::warn!(
                project = %project_root.display(),
                %error,
                "project observability shutdown was incomplete"
            );
            clean = false;
        }
    }
    clean
}

/// Terminal teardown for a set of drained runtimes. Shared by full daemon
/// shutdown and by targeted retirement (`retire_roots`), so a deletion cleanup
/// tears a project's runtimes down exactly the way shutdown does.
fn shut_down_runtimes(runtimes: BTreeMap<PathBuf, ProjectRuntime>) {
    for runtime in runtimes.values() {
        let (Some(router), Some(feedback)) = (&runtime.feedback_cycle_input, &runtime.feedback)
        else {
            continue;
        };
        let _ = router.replace(Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
            feedback.project_id().clone(),
            feedback.source_observation_port(),
        )));
    }

    for (project_root, runtime) in runtimes {
        if let Some(semantic) = runtime.semantic {
            tracedecay_usecases::semantic_runtime::unregister_project_semantic_runtime(
                &project_root,
            );
            semantic.cancel();
        }
    }
}
