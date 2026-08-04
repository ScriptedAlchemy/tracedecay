//! Profile-level durable memory for conversations without a code project.

use std::path::{Path, PathBuf};

use crate::db::Database;
use crate::errors::Result;

pub const USER_MEMORY_DB_FILENAME: &str = "user-memory.db";

pub fn user_memory_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_MEMORY_DB_FILENAME)
}

pub async fn open_user_memory_db(profile_root: &Path) -> Result<Database> {
    let path = user_memory_db_path(profile_root);
    let authority = crate::db::DatabaseAuthority::for_runtime(&path, "open user memory")?;
    if path.is_file() {
        return Database::open(&path, &authority).await.map(|(db, _)| db);
    }
    Database::initialize(&path, &authority)
        .await
        .map(|(db, _)| db)
}
