//! Canonical memory authority and derived holographic projection regressions.

use tempfile::TempDir;
use tracedecay::application::memory::{MemoryApplication, MemoryOperationContext};
use tracedecay::db::Database;
use tracedecay::memory::encoding::HolographicEncoder;
use tracedecay::memory::types::{
    AddFactRequest, FactRecord, FeedbackAction, FeedbackRequest, MemoryCategory,
};
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay_domain::{FactOwnerV1, ProjectId};

#[path = "memory_test/compatibility_authority.rs"]
mod compatibility_authority;

async fn make_memory_store() -> (Database, TempDir) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("tracedecay.db");
    // Template copy keeps each test isolated without rebuilding the schema.
    let db = crate::common::open_graph_db_from_template(&db_path).await;
    (db, tmp)
}

fn execute_sql<P>(db: &Database, sql: &str, params: P)
where
    P: rusqlite::Params,
{
    rusqlite::Connection::open(db.database_path())
        .unwrap()
        .execute(sql, params)
        .unwrap();
}

async fn scalar_i64(db: &Database, sql: &str) -> i64 {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(sql, (), |row| row.get(0))
    .unwrap()
}

async fn fact_hrr_blob(db: &Database, fact_id: i64) -> Vec<u8> {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT hrr_vector FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact_id],
        |row| row.get(0),
    )
    .unwrap()
}

async fn fact_has_no_hrr_vector(db: &Database, fact_id: i64) -> bool {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT hrr_vector IS NULL FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        != 0
}

fn fact_request(content: &str, category: MemoryCategory, trust: f64) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category,
        source: Some("test".to_string()),
        tags: Vec::new(),
        entities: Vec::new(),
        trust: Some(trust),
        metadata: serde_json::json!({}),
    }
}
