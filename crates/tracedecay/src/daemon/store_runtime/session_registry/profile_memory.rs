use super::DaemonSessionRuntimeRegistryV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_domain::errors::Result;

pub(crate) async fn open_user_memory_db(
    registry: &DaemonSessionRuntimeRegistryV1,
) -> Result<Database> {
    registry
        .profile_memory()
        .await
        .map(|database| database.as_ref().clone())
}
