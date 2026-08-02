use tempfile::TempDir;
use tracedecay_domain::{
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    ProvenanceId, UtcMicros,
};

use crate::db::engine::{Connection, TestConnection, params};

use super::schema::{table_exists, table_has_column};
use super::writers::insert_event;
use super::*;

// Identity, legacy-mapping, and current-projection rows used to be written by
// the V1→V2 backfill writer layer. That layer had no production writer left
// after the fresh-store cutover and was removed, so these tests seed the same
// rows directly. `insert_event` is *not* duplicated here: it is still a live
// production writer (the legacy-payload purge path appends through it), so the
// tests keep exercising the real function.

async fn seed_fact_identity(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
    identity_json: &str,
    created_at: i64,
) {
    conn.execute(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            identity_json,
            created_at
        ],
    )
    .await
    .unwrap();
}

async fn seed_current_fact(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &tracedecay_domain::FactEventId,
    updated_at: i64,
) {
    conn.execute(
        "INSERT INTO memory_v2_current_facts(
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES(?1, ?2, ?3, 'unavailable', NULL, NULL, ?4, ?5)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_id.as_str(),
            updated_at
        ],
    )
    .await
    .unwrap();
}

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

fn source_store_id() -> SourceStoreId {
    SourceStoreId::new(V1_COMPATIBILITY_SOURCE_STORE).unwrap()
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap()
}

#[tokio::test]
async fn schema_install_does_not_start_unowned_backfill() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_backfill_progress").await,
        0
    );
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
async fn fresh_v23_fact_relations_carry_provenance_and_referential_integrity() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner_key(&owner()).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES
            ('v23.relation.source', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('v23.relation.target', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('v23.relation.evidence', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
         INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(
            '{kind}', '{project_id}', 'v23.relation.source', 'v23.relation.target',
            'supports', 0.8, 'fixture', '{{}}', '[\"v23.relation.evidence\"]', 1, 1
         );",
        kind = owner.kind,
        project_id = owner.project_id,
        owner_json = owner.json,
    ))
    .await
    .unwrap();

    assert_eq!(
        optional_i64(&conn, "PRAGMA user_version", ())
            .await
            .unwrap(),
        Some(i64::from(super::super::migrations::SCHEMA_VERSION))
    );
    assert!(
        table_exists(&conn, "memory_v2_compatibility_banks")
            .await
            .unwrap()
    );
    assert!(
        table_exists(&conn, "memory_v2_compatibility_bank_dirty")
            .await
            .unwrap()
    );
    assert!(
        table_has_column(
            &conn,
            "memory_v2_fact_relations",
            "provenance_json",
            "memory_v2_v23_relation_upgrade_test",
        )
        .await
        .unwrap()
    );
    assert_eq!(
        optional_string(
            &conn,
            "SELECT provenance_json FROM memory_v2_fact_relations
             WHERE source_fact_id = 'v23.relation.source'
               AND target_fact_id = 'v23.relation.target' AND relation = 'supports'",
            (),
        )
        .await
        .unwrap(),
        Some("{}".to_owned())
    );
    conn.execute(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(?1, ?2, 'v23.relation.source', 'v23.relation.target',
                   'contradicts', 0.8, 'fixture', '{}',
                   '[\"v23.relation.evidence\"]', 2, 2)",
        params![owner.kind, owner.project_id.as_str()],
    )
    .await
    .unwrap();
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
async fn purge_clears_runtime_fact_payload_without_a_legacy_mapping() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner();
    let owner_key = owner_key(&owner).unwrap();
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("memory-v2.runtime-purge").unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&material).unwrap();
    let identity_json = json_text(&material).unwrap();
    seed_fact_identity(&conn, &owner_key, &fact_id, &identity_json, 10).await;
    let initial = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Unavailable,
            current: PayloadAccessState::Eligible,
        },
        UtcMicros(10),
        None,
    )
    .unwrap();
    insert_event(&conn, &owner_key, &initial, 10).await.unwrap();
    seed_current_fact(&conn, &owner_key, &fact_id, initial.event_id(), 10).await;
    conn.execute(
        "INSERT INTO memory_v2_assertions(
            assertion_id, fact_id, owner_kind, project_id, owner_json,
            assertion_header_json, kind_json, payload_reference_json,
            receipt_json, asserted_at, actor_id
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3, ?4,
            '{\"assertion_id\":\"assertion.runtime-purge\"}', '{}', '{}', '{}', 10, NULL
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str(),
            owner_key.json.as_str()
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memory_v2_assertion_payloads(
            assertion_id, fact_id, owner_kind, project_id, payload_json, content
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3,
            '{\"content\":\"runtime-purge-canary\"}', 'runtime-purge-canary'
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memory_v2_assertion_vectors(
            assertion_id, fact_id, owner_kind, project_id, vector, algebra, dimensions, precision
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3, x'0102', 'fixture', 2, 'f32'
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .unwrap();

    let source = source_store_id();
    assert!(
        purge_memory_v2_fact(
            &conn,
            &owner,
            &source,
            &fact_id,
            initial.event_id(),
            UtcMicros(20),
        )
        .await
        .unwrap()
        .payload_purged()
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_payloads").await,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_vectors").await,
        0
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
             WHERE memory_v2_assertion_payloads_fts MATCH '\"runtime-purge-canary\"'"
        )
        .await,
        0
    );
    assert_eq!(
        current_fact_state(&conn, &owner_key, &fact_id)
            .await
            .unwrap()
            .access,
        PayloadAccessState::Deleted
    );
}
