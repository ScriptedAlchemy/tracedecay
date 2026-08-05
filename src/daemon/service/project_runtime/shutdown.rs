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
        if !self.shutdown_started.swap(true, Ordering::AcqRel) {
            let registry = self.clone();
            self.shutdown_complete.send_replace(ShutdownState::Pending);
            self.shutdown_reaper.submit(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    registry.drain_all_blocking();
                }));
                let state = if result.is_ok() {
                    ShutdownState::Complete
                } else {
                    registry.shutdown_started.store(false, Ordering::Release);
                    ShutdownState::Failed
                };
                registry.shutdown_complete.send_replace(state);
            });
        }
        loop {
            let state = *shutdown_complete.borrow_and_update();
            match state {
                ShutdownState::Complete => return,
                ShutdownState::Failed => return,
                ShutdownState::Pending => {
                    if shutdown_complete.changed().await.is_err() {
                        self.shutdown_complete.send_replace(ShutdownState::Failed);
                        return;
                    }
                }
            }
        }
    }

    fn drain_all_blocking(&self) {
        let runtimes = loop {
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
        };

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
    }
}
