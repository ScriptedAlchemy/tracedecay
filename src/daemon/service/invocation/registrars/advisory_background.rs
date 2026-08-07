//! Deferred post-open gateway registration for the advisory runtime.

use super::*;

impl DaemonAdvisoryRuntimeRegistrar {
    /// Registers the retained warming gateway for one project's advisory and
    /// Scout work before provider/model setup begins. Registration is
    /// idempotent for the exact locator pair; retirement of the project
    /// runtime cancels the gateway through its registered component.
    pub(crate) async fn register_deferred_hook_orchestrator(
        &self,
        project_root: PathBuf,
        project_id: [u8; 16],
        worktree_id: [u8; 16],
    ) -> Result<Arc<DeferredHookOrchestratorV1>, DaemonAdvisoryRuntimeRegistrationError> {
        if project_id == [0; 16]
            || worktree_id == [0; 16]
            || !self
                .service
                .project_runtimes
                .holds::<Arc<FeedbackCycleRuntime>>(&project_root)
                .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        let candidate = DeferredHookOrchestratorV1::new(now_micros());
        let component = RegisteredHookOrchestrationRuntimeV1::new(
            project_id,
            worktree_id,
            Arc::clone(&candidate),
        );
        let inserted = match self
            .service
            .project_runtimes
            .register(project_root.clone(), component)
            .await
        {
            Ok(()) => true,
            Err(ProjectRuntimeRegistryError::AlreadyRegistered) => false,
            Err(error) => return Err(error.into()),
        };
        let runtime = if inserted {
            candidate
        } else {
            self.service
                .project_runtimes
                .read::<RegisteredHookOrchestrationRuntimeV1, _, _>(&project_root, |incumbent| {
                    incumbent
                        .matches(project_id, worktree_id)
                        .then(|| incumbent.runtime())
                })
                .await
                .flatten()
                .ok_or(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered)?
        };
        let runtime_port: Arc<dyn HookOrchestrationPortV1> = runtime.clone();
        let runtime_weak: Weak<dyn HookOrchestrationPortV1> = Arc::downgrade(&runtime_port);
        let registered = match hook_orchestration_registry().lock() {
            Ok(mut registry) => {
                registry.retain(|_, runtime| runtime.strong_count() > 0);
                let key = (project_id, worktree_id);
                if registry
                    .get(&key)
                    .and_then(Weak::upgrade)
                    .is_some_and(|existing| !Arc::ptr_eq(&existing, &runtime_port))
                {
                    false
                } else {
                    registry.insert(key, runtime_weak);
                    true
                }
            }
            Err(_) => false,
        };
        if registered {
            Ok(runtime)
        } else {
            if inserted {
                self.service
                    .project_runtimes
                    .withdraw::<RegisteredHookOrchestrationRuntimeV1>(&project_root)
                    .await;
            }
            Err(DaemonAdvisoryRuntimeRegistrationError::HookOrchestrationUnavailable)
        }
    }
}
