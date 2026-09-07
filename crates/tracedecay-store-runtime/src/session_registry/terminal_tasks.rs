use super::DaemonSessionRuntimeRegistryV1;

impl DaemonSessionRuntimeRegistryV1 {
    pub fn cancel_terminal_tasks(&self) {
        self.semantic_lifecycle_closed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(owners) = self.retained_semantic_lifecycle_owners() {
            for owner in owners {
                owner.cancel_background_acquisition();
            }
        }
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
        match self.retained_semantic_lifecycle_cells() {
            Ok(owners) => {
                let joined = tokio::task::spawn_blocking(move || {
                    owners
                        .into_iter()
                        .filter_map(|cell| cell.shutdown().err())
                        .collect::<Vec<_>>()
                })
                .await;
                match joined {
                    Ok(errors) => failures.extend(
                        errors
                            .into_iter()
                            .map(|error| format!("semantic acquisition shutdown: {error:?}")),
                    ),
                    Err(error) => {
                        failures.push(format!("semantic acquisition shutdown worker: {error}"))
                    }
                }
            }
            Err(error) => failures.push(error.to_string()),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
