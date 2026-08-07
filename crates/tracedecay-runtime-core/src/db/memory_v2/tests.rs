use tempfile::TempDir;
use tracedecay_domain::FactOwnerV1;

use crate::db::engine::{Connection, TestConnection, params};

use super::schema::{table_exists, table_has_column};
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

#[tokio::test]
async fn fresh_store_carries_only_the_final_memory_shape() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    assert_eq!(
        scalar(&conn, "PRAGMA user_version").await,
        i64::from(super::super::migrations::SCHEMA_VERSION)
    );
    for legacy in [
        "memory_v2_legacy_map",
        "memory_v2_legacy_quarantine",
        "memory_v2_backfill_progress",
        "memory_v2_legacy_proposal_map",
        "memory_v2_legacy_feedback_event_map",
        "memory_v2_feedback_history_repair_progress",
        "memory_v2_compatibility_operation_receipts",
        "memory_v2_compatibility_banks",
        "memory_v2_compatibility_bank_dirty",
        // Plan 39 Task 7 (owner decision 2026-08-07, second): derived vector
        // storage is deleted, not relocated. Recall re-encodes from canonical
        // content at query time, so a fresh final store carries no bank rows,
        // no dirty queue, and no per-assertion vector table.
        "memory_v2_banks",
        "memory_v2_bank_dirty",
        "memory_v2_assertion_vectors",
    ] {
        assert!(
            !table_exists(&conn, legacy).await.unwrap(),
            "legacy table {legacy} must not exist in a fresh final store"
        );
    }
    for table in [
        "memory_v2_operation_receipts",
        "memory_v2_feedback_history",
        "memory_v2_fact_relations",
    ] {
        assert!(
            table_exists(&conn, table).await.unwrap(),
            "final table {table} is missing"
        );
    }
    assert!(
        table_has_column(&conn, "memory_facts", "canonical_fact_id", "final_shape")
            .await
            .unwrap()
    );
    assert!(
        !table_has_column(&conn, "memory_v2_proposals", "origin", "final_shape")
            .await
            .unwrap(),
        "proposal origin column is import machinery and must be gone"
    );
}

#[tokio::test]
async fn fact_relations_enforce_owner_evidence_and_identity() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner_key(&owner()).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES
            ('relation.source', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('relation.target', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('relation.evidence', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
         INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(
            '{kind}', '{project_id}', 'relation.source', 'relation.target',
            'supports', 0.8, 'fixture', '{{}}', '[\"relation.evidence\"]', 1, 1
         );",
        kind = owner.kind,
        project_id = owner.project_id,
        owner_json = owner.json,
    ))
    .await
    .unwrap();
    // The final shape accepts every canonical relation without an upgrade
    // dance.
    conn.execute(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(?1, ?2, 'relation.source', 'relation.target',
                   'contradicts', 0.8, 'fixture', '{}', '[\"relation.evidence\"]', 2, 2)",
        params![owner.kind, owner.project_id.as_str()],
    )
    .await
    .unwrap();
    // Evidence outside the owner's facts is refused by the validation trigger.
    let unknown_evidence = conn
        .execute(
            "INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
             ) VALUES(?1, ?2, 'relation.source', 'relation.target',
                       'supersedes', 0.8, 'fixture', '{}', '[\"relation.unknown\"]', 3, 3)",
            params![owner.kind, owner.project_id.as_str()],
        )
        .await;
    assert!(unknown_evidence.is_err());
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_fact_relations").await,
        2
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0
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
