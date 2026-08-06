//! Memory store regression suite, split by fact authority.
//!
//! - `legacy_store`: legacy fact-store, retrieval, grooming, and status tests.
//! - `compatibility_authority`: compatibility fact authority suite.
//!
//! Shared fixtures live in this parent module; each child pulls them in with `use super::*`.

use rusqlite::OptionalExtension;
use tempfile::TempDir;
use tracedecay::application::memory::{MemoryApplication, MemoryOperationContext};
use tracedecay::db::Database;
use tracedecay::memory::diff::vector_similarity;
use tracedecay::memory::encoding::HolographicEncoder;
use tracedecay::memory::entities::{extract_entities, normalize_entity};
use tracedecay::memory::trust::{
    DEFAULT_TRUST, apply_feedback, clamp_trust, trust_bucket, trust_distribution,
};
use tracedecay::memory::types::{
    AddFactDiffKind, AddFactRequest, FactRecord, FactRelationKind, FeedbackAction, FeedbackRequest,
    MemoryCategory, MemoryGroomingOperation, SearchFactsRequest, UpdateFactRequest,
};
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::{FactOwnerV1, ProjectId};

#[path = "memory_test/compatibility_authority.rs"]
mod compatibility_authority;

#[path = "memory_test/legacy_store.rs"]
mod legacy_store;

// Future candidate: seed from the cross-process store template like
// `make_memory_store` below and tests/mcp_suite/fixture.rs do, instead of
// paying full schema creation per test. Only worth it once that fixture
// moves to tests/common — seven call sites run in parallel today.
async fn make_project() -> (TempDir, TraceDecay) {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn hello() {}").unwrap();
    let cg = TraceDecay::init(tmp.path()).await.unwrap();
    (tmp, cg)
}

async fn make_memory_store() -> (Database, TempDir) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("tracedecay.db");
    // Template copy instead of rebuilding the registered test runtime: 23 tests in this module
    // each need a fresh graph-schema store, and re-running the schema DDL per
    // test is a large fixed cost on Windows CI.
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

async fn seed_newer_unrelated_memory_facts(
    db: &Database,
    category: MemoryCategory,
    content_prefix: &str,
    entity_prefix: &str,
    count: usize,
) {
    let mut conn = rusqlite::Connection::open(db.database_path()).unwrap();
    let transaction = conn.transaction().unwrap();
    for i in 0..count {
        let fact_id = 10_000 + i as i64;
        let entity_id = 20_000 + i as i64;
        let content = format!("{content_prefix} {i}");
        let entity = format!("{entity_prefix}{i}");
        let normalized = normalize_entity(&entity).to_ascii_lowercase();
        let updated_at = 1_900_000_000 + i as i64;

        if let Err(err) = transaction.execute(
                "INSERT INTO memory_facts
                    (fact_id, content, category, trust_score, created_at, updated_at, source, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
                rusqlite::params![
                    fact_id,
                    content,
                    category.as_str(),
                    0.9_f64,
                    updated_at,
                    "test",
                    "{}"
                ],
            ) {
            panic!("failed to insert unrelated memory fact: {err}");
        }

        if let Err(err) = transaction.execute(
            "INSERT INTO memory_entities
                    (entity_id, name, normalized_name, entity_type, aliases, created_at)
                 VALUES (?1, ?2, ?3, 'unknown', '[]', ?4)",
            rusqlite::params![entity_id, entity, normalized, updated_at],
        ) {
            panic!("failed to insert unrelated memory entity: {err}");
        }

        if let Err(err) = transaction.execute(
            "INSERT INTO memory_fact_entities (fact_id, entity_id) VALUES (?1, ?2)",
            rusqlite::params![fact_id, entity_id],
        ) {
            panic!("failed to link unrelated memory entity: {err}");
        }
    }
    transaction.commit().unwrap();
}

async fn dirty_bank_names(db: &Database) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = conn
        .prepare("SELECT bank_name FROM memory_bank_dirty ORDER BY bank_name")
        .unwrap();
    statement
        .query_map((), |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

async fn entity_id(db: &Database, normalized_name: &str) -> i64 {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
        rusqlite::params![normalized_name],
        |row| row.get(0),
    )
    .unwrap()
}

async fn dirty_bank_updated_at(db: &Database, bank_name: &str) -> i64 {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT updated_at FROM memory_bank_dirty WHERE bank_name = ?1",
        rusqlite::params![bank_name],
        |row| row.get(0),
    )
    .expect("dirty bank should exist")
}

async fn memory_bank_count(db: &Database) -> i64 {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row("SELECT COUNT(*) FROM memory_banks", (), |row| row.get(0))
    .unwrap()
}

async fn memory_bank_fact_count(db: &Database, bank_name: &str) -> Option<i64> {
    rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT fact_count FROM memory_banks WHERE bank_name = ?1",
        rusqlite::params![bank_name],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

async fn clear_fact_vector(cg: &TraceDecay, fact_id: i64) {
    rusqlite::Connection::open(&cg.store_layout().graph_db_path)
        .unwrap()
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = NULL, hrr_algebra = 'legacy', hrr_dim = 8
             WHERE fact_id = ?1",
            rusqlite::params![fact_id],
        )
        .unwrap();
}

async fn set_fact_updated_at(cg: &TraceDecay, fact_id: i64, updated_at: i64) {
    rusqlite::Connection::open(&cg.store_layout().graph_db_path)
        .unwrap()
        .execute(
            "UPDATE memory_facts SET updated_at = ?2 WHERE fact_id = ?1",
            rusqlite::params![fact_id, updated_at],
        )
        .unwrap();
}

async fn fact_updated_at(cg: &TraceDecay, fact_id: i64) -> i64 {
    rusqlite::Connection::open_with_flags(
        &cg.store_layout().graph_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT updated_at FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact_id],
        |row| row.get(0),
    )
    .unwrap()
}

async fn fact_hrr_vector(db: &Database, fact_id: i64) -> Vec<f64> {
    let bytes: Vec<u8> = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT hrr_vector FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact_id],
        |row| row.get(0),
    )
    .unwrap();
    HolographicEncoder::deserialize(&bytes).unwrap()
}

fn assert_vector_matches_with_f32_tolerance(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    let max_abs_error = actual
        .iter()
        .zip(expected)
        .map(|(actual, expected)| (actual - expected).abs())
        .fold(0.0_f64, f64::max);
    assert!(
        max_abs_error <= 3.0e-8,
        "stored f32 vector drifted from f64 baseline; max_abs_error={max_abs_error:e}"
    );
    assert!(
        HolographicEncoder.similarity(actual, expected) > 0.999_999_999,
        "stored f32 vector should preserve phase-cosine similarity"
    );
}
