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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_profile_memory_database_installs_latest_schema() {
        let profile_root = tempfile::tempdir().expect("create profile root");
        let database = open_user_memory_db(profile_root.path())
            .await
            .expect("open fresh profile memory database");
        assert!(user_memory_db_path(profile_root.path()).is_file());

        let mut rows = database
            .conn()
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN (
                       'memory_v2_fact_relations',
                       'memory_v2_compatibility_banks',
                       'memory_v2_compatibility_bank_dirty'
                   )",
                (),
            )
            .await
            .expect("query fresh profile V23 tables");
        let row = rows
            .next()
            .await
            .expect("read fresh profile V23 table count")
            .expect("fresh profile V23 table count row");
        let count: i64 = row.get(0).expect("decode fresh profile V23 table count");
        assert_eq!(count, 3);
    }
}
