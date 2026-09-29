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

    assert_eq!(
        trigger_name(&conn, "memory_v2_payloads_fts_insert").await,
        Some("memory_v2_payloads_fts_insert".to_owned()),
        "a fresh store still installs the live payload FTS trigger"
    );
    for trigger in ["nodes_fts_insert", "nodes_fts_delete", "nodes_fts_update"] {
        assert_eq!(
            trigger_name(&conn, trigger).await,
            None,
            "retired trigger '{trigger}' must not exist after creation"
        );
    }
}

async fn trigger_name(conn: &crate::db::engine::TestConnection, name: &str) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name=?1",
            (name,),
        )
        .await
        .expect("failed to query sqlite_master for trigger");
    let row = rows.next().await.expect("failed to read trigger row")?;
    Some(row.get::<String>(0).expect("trigger name"))
}
