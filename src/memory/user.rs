//! Profile-level durable memory for conversations without a code project.

use std::path::{Path, PathBuf};

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::Result;

pub const USER_MEMORY_DB_FILENAME: &str = "user-memory.db";

pub fn user_memory_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_MEMORY_DB_FILENAME)
}

pub(crate) async fn open_user_memory_db(
    registry: &DaemonSessionRuntimeRegistryV1,
) -> Result<Database> {
    registry
        .profile_memory()
        .await
        .map(|database| database.as_ref().clone())
}
