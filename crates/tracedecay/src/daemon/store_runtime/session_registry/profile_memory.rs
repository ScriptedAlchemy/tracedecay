use super::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::Result;

pub(crate) async fn open_user_memory_db(
    registry: &DaemonSessionRuntimeRegistryV1,
) -> Result<Database> {
    registry
        .profile_memory()
        .await
        .map(|database| database.as_ref().clone())
}
