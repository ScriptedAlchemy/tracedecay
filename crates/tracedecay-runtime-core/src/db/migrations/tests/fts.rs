//! Retired FTS object coverage.

use super::*;

/// The retired code-symbol FTS triggers are absent from a fresh relational
/// store. Symbol search is served from the verified Grafeo generation.
#[tokio::test]
async fn code_symbol_fts_triggers_are_not_recreated() {
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
                .is_none(),
            "retired trigger '{trigger}' must not exist after creation"
        );
    }
}
