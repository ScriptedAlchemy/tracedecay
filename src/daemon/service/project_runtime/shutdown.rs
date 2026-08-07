use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownState {
    Pending,
    Complete,
    Failed,
}

impl ProjectRuntimeRegistryV1 {
    pub(crate) fn begin_shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.signal_reservation_changed();
    }

    /// Shut every project runtime down and leave the registry empty.
    ///
    /// Routers become unavailable before feedback owners drop, Work providers
    /// are joined, and process-wide semantic handles are unregistered.
    pub(crate) async fn shut_down_all(&self) {
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
                    return;
                }
                ShutdownState::Pending => {
                    if shutdown_complete.changed().await.is_err() {
                        self.shutdown_complete.send_replace(ShutdownState::Failed);
                        self.join_shutdown_task().await;
                        return;
                    }
                }
            }
        }
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
            let mut runtimes = self.lock_runtimes();
            if runtimes
                .values()
                .all(|runtime| runtime.reservations.is_empty())
            {
                break std::mem::take(&mut *runtimes);
            }
            drop(runtimes);
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
        if !clean {
            self.lock_runtimes().extend(runtimes);
            return false;
        }
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
            if let Some(external_acquisition) = runtime.external_acquisition {
                external_acquisition.cancel();
            }
            if let Some(semantic) = runtime.semantic {
                crate::application::semantic_runtime::unregister_project_semantic_runtime(
                    &project_root,
                );
                semantic.cancel();
            }
        }
        clean
    }
}
