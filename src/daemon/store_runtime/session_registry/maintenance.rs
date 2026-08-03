use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_store::StoreRuntimeBindingV1;

use super::{
    DaemonSessionRuntimeRegistryV1, LONG_LIVED_SESSION_MAINTENANCE, RegisteredGlobalDb, Result,
    StoreRuntimeHandle, registry_open_error, release_process_allocator_memory,
};

impl DaemonSessionRuntimeRegistryV1 {
    fn long_lived_session_maintenance(&self) -> bool {
        LONG_LIVED_SESSION_MAINTENANCE.load(Ordering::Relaxed)
    }

    pub(super) async fn attach_registered(
        &self,
        runtime: StoreRuntimeHandle,
        operation: &'static str,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        let expected_binding: StoreRuntimeBindingV1 = runtime.binding().clone();
        let expected_locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority(operation)
            .map_err(|failure| registry_open_error(operation, failure))?;
        let database = Arc::new(
            RegisteredGlobalDb::attach_exact(
                runtime,
                expected_binding,
                expected_locator,
                authority,
            )
            .await?,
        );
        if self.long_lived_session_maintenance() {
            if let Err(error) = database.release_connection_memory().await {
                crate::daemon::log_daemon_event(
                    "registered_schema_admission_memory_release",
                    &[
                        ("outcome", "degraded".to_owned()),
                        ("database", database.db_path().display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            release_process_allocator_memory();
        }
        Ok(database)
    }
}
