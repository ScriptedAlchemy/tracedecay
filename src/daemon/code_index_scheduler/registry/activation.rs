use std::sync::{Arc, Weak};

use super::CodeIndexSchedulerRegistryV1;
use crate::daemon::code_index_scheduler::CodeIndexActivationV1;

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) fn register_activation(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        activation: &Arc<CodeIndexActivationV1>,
    ) -> bool {
        if scope.validate().is_err() {
            return false;
        }
        if activation.identity().is_none() {
            return true;
        }
        if !activation.authorizes_scope(scope) {
            return false;
        }
        let mut activations = self
            .activations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        activations.retain(|_, activation| activation.strong_count() > 0);
        let scope_digest = scope.scope_digest.clone();
        let registered = Arc::downgrade(activation);
        let project_root = activation.project_root().to_path_buf();
        let registry = self.clone();
        activations.insert(scope_digest.clone(), registered.clone());
        drop(activations);
        let activations = Arc::clone(&self.activations);
        activation.install_retirement(Box::new(move || {
            let mut activations = activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if activations
                .get(&scope_digest)
                .is_some_and(|current| Weak::ptr_eq(current, &registered))
            {
                activations.remove(&scope_digest);
            }
            let should_unmount = !activations.values().any(|activation| {
                activation
                    .upgrade()
                    .is_some_and(|activation| activation.project_root() == project_root)
            });
            drop(activations);
            if should_unmount && let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    registry.unmount_worktree(&project_root).await;
                });
            }
        }));
        true
    }

    pub(super) fn activate_for_scope(&self, scope: &tracedecay_application::ResolvedScope) -> bool {
        let activation = {
            let mut activations = self
                .activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let activation = activations.get(&scope.scope_digest).and_then(Weak::upgrade);
            if activation
                .as_ref()
                .is_none_or(|activation| !activation.authorizes_scope(scope))
            {
                activations.remove(&scope.scope_digest);
                None
            } else {
                activation
            }
        };
        activation.is_some_and(|activation| activation.activate())
    }

    #[cfg(test)]
    pub(in crate::daemon::code_index_scheduler) fn activation_count(&self) -> usize {
        self.activations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}
