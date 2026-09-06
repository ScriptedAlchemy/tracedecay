use std::sync::atomic::Ordering;

use super::DaemonSessionRuntimeRegistryV1;

impl DaemonSessionRuntimeRegistryV1 {
    pub fn cancel_terminal_tasks(&self) {
        self.graph_lifecycle_cancelled
            .store(true, Ordering::Release);
        self.semantic_vector_operation_task_owner.begin_shutdown();
        self.retained_hook_tasks.begin_shutdown();
        self.registered_schema_convergence.begin_shutdown();
    }

    /// Joins semantic-vector blocking settlement and the registry's other
    /// terminal task owners before retained graph runtimes may be closed.
    #[hotpath::measure(label = "daemon.session_registry.shutdown_terminal", future = true)]
    pub async fn shutdown_terminal_tasks(&self) -> Result<(), String> {
        self.cancel_terminal_tasks();
        let mut failures = Vec::new();
        if let Err(error) = self.semantic_vector_operation_task_owner.shutdown().await {
            failures.push(error);
        }
        if let Err(error) = self.retained_hook_tasks.shutdown().await {
            failures.push(error);
        }
        if let Err(error) = self.registered_schema_convergence.shutdown().await {
            failures.push(error);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
