use tempfile::TempDir;

use crate::db::engine::{Connection, TestConnection};

use super::*;

async fn database() -> (TestConnection, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2.db");
    let conn = TestConnection::open(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    crate::db::schema::create_schema_connection(&conn)
        .await
        .unwrap();
    (conn, dir)
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap()
}

#[tokio::test]
async fn final_schema_installs_retrieval_anchor_authority() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM retrieval_anchors").await,
        0
    );
    assert!(
        !row_exists(
            &conn,
            "SELECT 1 FROM sqlite_master WHERE name = 'memory_v2_retrieval_anchors'",
            (),
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn final_schema_omits_superseded_memory_storage() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();

    assert!(
        row_exists(
            &conn,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'memory_v2_feedback_history'",
            (),
        )
        .await
        .unwrap()
    );
    for table in [
        "memory_v2_legacy_map",
        "memory_v2_assertion_vectors",
        "memory_v2_fact_relations",
        "memory_v2_proposals",
        "memory_v2_proposal_transitions",
        "memory_v2_proposal_current",
        "memory_v2_compatibility_operation_receipts",
        "memory_v2_compatibility_banks",
        "memory_v2_compatibility_bank_dirty",
    ] {
        assert!(
            !row_exists(
                &conn,
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                crate::db::engine::params![table],
            )
            .await
            .unwrap(),
            "superseded table {table} was installed"
        );
    }
}
