use tempfile::TempDir;
use tracedecay_domain::FactOwnerV1;

use crate::db::engine::{Connection, TestConnection};

use super::*;

async fn database() -> (TestConnection, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2.db");
    let conn = TestConnection::open(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    crate::db::migrations::create_schema_connection(&conn)
        .await
        .unwrap();
    (conn, dir)
}

fn owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("project.memory-v2-test").unwrap(),
    }
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn string_column(conn: &Connection, sql: &str) -> Vec<String> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(row.get(0).unwrap());
    }
    values
}

#[tokio::test]
async fn fresh_store_carries_only_the_final_memory_shape() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    assert_eq!(
        scalar(&conn, "PRAGMA user_version").await,
        i64::from(super::super::migrations::SCHEMA_VERSION)
    );
    assert_eq!(
        string_column(
            &conn,
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name GLOB 'memory_v2_*'
             ORDER BY name",
        )
        .await,
        [
            "memory_v2_assertion_evidence",
            "memory_v2_assertion_payloads",
            "memory_v2_assertion_payloads_fts",
            "memory_v2_assertion_payloads_fts_config",
            "memory_v2_assertion_payloads_fts_data",
            "memory_v2_assertion_payloads_fts_docsize",
            "memory_v2_assertion_payloads_fts_idx",
            "memory_v2_assertion_supersession",
            "memory_v2_assertions",
            "memory_v2_automatic_fact_receipts",
            "memory_v2_current_facts",
            "memory_v2_evidence",
            "memory_v2_facts",
            "memory_v2_feedback_history",
            "memory_v2_lineage_events",
            "memory_v2_operation_receipts",
        ],
        "a fresh store must contain exactly the final memory tables and FTS shadows",
    );
    assert_eq!(
        string_column(
            &conn,
            "SELECT name FROM pragma_table_xinfo('memory_v2_current_facts')
             ORDER BY cid",
        )
        .await,
        [
            "fact_id",
            "owner_kind",
            "project_id",
            "payload_access",
            "trust_score",
            "active_assertion_id",
            "last_event_id",
            "updated_at",
            "retrieval_count",
            "access_count",
            "helpful_count",
            "unhelpful_count",
            "last_retrieved_at",
            "last_recalled_at",
            "last_feedback_at",
        ],
        "the current projection must expose only the exact final columns",
    );
}

#[tokio::test]
async fn feedback_history_permits_only_detail_redaction() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner_key(&owner()).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES('history.fact', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
         INSERT INTO memory_v2_lineage_events(
            event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
         ) VALUES('history.event', 'history.fact', '{kind}', '{project_id}', '{{}}', 1, 1);
         INSERT INTO memory_v2_feedback_history(
            owner_kind, project_id, fact_id, event_id, action, old_trust,
            new_trust, occurred_at, source, note, details_availability
         ) VALUES('{kind}', '{project_id}', 'history.fact', 'history.event',
                   'helpful', 0.5, 0.6, 1, 'mcp', 'note', 'available');",
        kind = owner.kind,
        project_id = owner.project_id,
        owner_json = owner.json,
    ))
    .await
    .unwrap();
    // Redaction is the only accepted update: details go NULL and availability
    // moves available -> redacted.
    conn.execute(
        "UPDATE memory_v2_feedback_history
         SET source = NULL, note = NULL, details_availability = 'redacted'
         WHERE fact_id = 'history.fact'",
        (),
    )
    .await
    .unwrap();
    // Any other rewrite (here: trust tampering) aborts.
    let tampered = conn
        .execute(
            "UPDATE memory_v2_feedback_history
             SET new_trust = 0.9
             WHERE fact_id = 'history.fact'",
            (),
        )
        .await;
    assert!(tampered.is_err());
    // Deleting recorded history aborts.
    let deleted = conn
        .execute(
            "DELETE FROM memory_v2_feedback_history WHERE fact_id = 'history.fact'",
            (),
        )
        .await;
    assert!(deleted.is_err());
}
