use std::sync::Arc;
use tracedecay_store::StoreRuntimeBindingV1;

use super::{
    DaemonSessionRuntimeRegistryV1, RegisteredGlobalDb, Result, StoreRuntimeHandle,
    registry_open_error,
};

impl DaemonSessionRuntimeRegistryV1 {
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
        RegisteredGlobalDb::install_and_attach(
            runtime,
            expected_binding,
            expected_locator,
            authority,
        )
        .await
        .map(Arc::new)
    }
}
