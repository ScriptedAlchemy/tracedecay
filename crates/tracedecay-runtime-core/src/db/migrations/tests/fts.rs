//! FTS trigger and memory-facts FTS tracking coverage.

use super::*;

/// FTS triggers exist on a freshly created store.
#[tokio::test]
async fn test_fts_triggers_exist_after_creation() {
    let (conn, _dir) = create_raw_db().await;

    ensure_schema_current_connection(&conn)
        .await
        .expect("creating the schema on an empty file should succeed");

    let triggers = ["nodes_fts_insert", "nodes_fts_delete", "nodes_fts_update"];
    for trigger in &triggers {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND name=?1",
                (*trigger,),
            )
            .await
            .expect("failed to query sqlite_master for trigger");
        assert!(
            rows.next()
                .await
                .expect("failed to read trigger row")
                .is_some(),
            "trigger '{trigger}' should exist after creation"
        );
    }
}

#[tokio::test]
async fn memory_facts_fts_triggers_track_insert_update_delete() {
    let (conn, _dir) = create_schema_db().await;

    conn.execute(
        "INSERT INTO memory_facts (content, category, tags)
         VALUES ('Use orbital retrieval for context', 'test', '[\"retrieval\"]')",
        (),
    )
    .await
    .expect("failed to insert memory fact");
    let fact_id = scalar_i64(&conn, "SELECT fact_id FROM memory_facts").await;
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'orbital'"
        )
        .await,
        1
    );

    conn.execute(
        "UPDATE memory_facts SET content='Use semantic banana storage', tags='[\"banana\"]' WHERE fact_id=?1",
        (fact_id,),
    )
    .await
    .expect("failed to update memory fact");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'orbital'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'banana'"
        )
        .await,
        1
    );

    conn.execute("DELETE FROM memory_facts WHERE fact_id=?1", (fact_id,))
        .await
        .expect("failed to delete memory fact");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'banana'"
        )
        .await,
        0
    );
}
