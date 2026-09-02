use super::DaemonSessionRuntimeRegistryV1;

impl DaemonSessionRuntimeRegistryV1 {
    pub fn cancel_terminal_tasks(&self) {
        self.retained_hook_tasks.begin_shutdown();
        self.registered_schema_convergence.begin_shutdown();
    }

    #[hotpath::measure(label = "daemon.session_registry.shutdown_terminal", future = true)]
    pub async fn shutdown_terminal_tasks(&self) -> Result<(), String> {
        self.cancel_terminal_tasks();
        let mut failures = Vec::new();
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
