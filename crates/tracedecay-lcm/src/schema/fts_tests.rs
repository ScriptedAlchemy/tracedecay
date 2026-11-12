use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};

use super::{ensure_lcm_schema, util};

async fn fixture() -> Result<(tempfile::TempDir, TestConnection), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let connection = TestConnection::open(&temp.path().join("sessions.db"));
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL, session_id TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             INSERT INTO sessions VALUES ('codex', 'session');",
        )
        .await
        .map_err(|error| error.to_string())?;
    ensure_lcm_schema(&connection)
        .await
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, content,
                content_hash, storage_kind
             ) VALUES ('codex', 'message', 'session', 'assistant', 1,
                       'originalword', 'hash', 'inline');",
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok((temp, connection))
}

async fn matches(connection: &TestConnection, query: &str) -> Result<i64, String> {
    util::fetch_i64(
        &**connection,
        "SELECT COUNT(*) FROM lcm_raw_messages_fts WHERE lcm_raw_messages_fts MATCH ?1",
        params![query],
        "FTS match count",
    )
    .await
    .map_err(|error| error.to_string())
}

#[tokio::test]
async fn metadata_and_noop_updates_do_not_write_search_segments() -> Result<(), String> {
    let (_temp, connection) = fixture().await?;
    connection
        .execute_batch(
            "CREATE TRIGGER reject_search_segment_insert
                BEFORE INSERT ON lcm_raw_messages_fts_data
                BEGIN SELECT RAISE(ABORT, 'unexpected search segment write'); END;
             CREATE TRIGGER reject_search_segment_update
                BEFORE UPDATE ON lcm_raw_messages_fts_data
                BEGIN SELECT RAISE(ABORT, 'unexpected search segment write'); END;
             CREATE TRIGGER reject_search_segment_delete
                BEFORE DELETE ON lcm_raw_messages_fts_data
                BEGIN SELECT RAISE(ABORT, 'unexpected search segment write'); END;",
        )
        .await
        .map_err(|error| error.to_string())?;
    for statement in [
        "UPDATE lcm_raw_messages SET metadata_json = '{\"receipt\":1}'",
        "UPDATE lcm_raw_messages SET ordinal = ordinal + 1, source_path = 'changed'",
        "UPDATE lcm_raw_messages SET content = content, role = role, kind = kind,
                                    model = model, tool_names = tool_names",
        "UPDATE lcm_raw_messages SET placeholder_text = 'unused while content is inline'",
    ] {
        connection
            .execute_batch(statement)
            .await
            .map_err(|error| format!("unrelated update wrote search segments: {error}"))?;
    }
    assert_eq!(matches(&connection, "originalword").await?, 1);
    assert!(
        connection
            .execute_batch("UPDATE lcm_raw_messages SET content = 'replacementword'")
            .await
            .is_err(),
        "the write rejection must detect a real indexed change"
    );
    assert_eq!(matches(&connection, "originalword").await?, 1);

    // Model a current store whose derived trigger still rewrites every update.
    connection
        .execute_batch(
            "DROP TRIGGER lcm_raw_messages_fts_update;
             CREATE TRIGGER lcm_raw_messages_fts_update
             AFTER UPDATE ON lcm_raw_messages BEGIN
                INSERT INTO lcm_raw_messages_fts(
                    lcm_raw_messages_fts, rowid, index_text, role, kind, model, tool_names
                ) VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role,
                          OLD.kind, OLD.model, OLD.tool_names);
                INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, kind, model, tool_names)
                VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
             END;",
        )
        .await
        .map_err(|error| error.to_string())?;
    assert!(
        connection
            .execute_batch("UPDATE lcm_raw_messages SET metadata_json = NULL")
            .await
            .is_err()
    );
    ensure_lcm_schema(&connection)
        .await
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("UPDATE lcm_raw_messages SET metadata_json = NULL")
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(matches(&connection, "originalword").await?, 1);
    let schema_version =
        util::fetch_i64(&*connection, "PRAGMA schema_version", (), "schema version")
            .await
            .map_err(|error| error.to_string())?;
    ensure_lcm_schema(&connection)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        util::fetch_i64(&*connection, "PRAGMA schema_version", (), "schema version")
            .await
            .map_err(|error| error.to_string())?,
        schema_version,
        "admitting the current trigger must not write schema again"
    );
    Ok(())
}

#[tokio::test]
async fn changed_generated_text_fields_nulls_and_rowids_still_reindex() -> Result<(), String> {
    let (_temp, connection) = fixture().await?;
    for (statement, absent, present) in [
        (
            "UPDATE lcm_raw_messages SET content = 'replacementword'",
            "originalword",
            "replacementword",
        ),
        (
            "UPDATE lcm_raw_messages SET content = NULL, placeholder_text = 'placeholderword'",
            "replacementword",
            "placeholderword",
        ),
        (
            "UPDATE lcm_raw_messages SET placeholder_text = 'changedplaceholder'",
            "placeholderword",
            "changedplaceholder",
        ),
        (
            "UPDATE lcm_raw_messages SET role = 'tool', kind = 'command',
                                         model = 'testmodel', tool_names = 'testtool'",
            "role:assistant",
            "role:tool AND kind:command AND model:testmodel AND tool_names:testtool",
        ),
        (
            "UPDATE lcm_raw_messages SET kind = NULL, model = NULL, tool_names = NULL",
            "kind:command OR model:testmodel OR tool_names:testtool",
            "role:tool",
        ),
    ] {
        connection
            .execute_batch(statement)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(matches(&connection, absent).await?, 0, "{statement}");
        assert_eq!(matches(&connection, present).await?, 1, "{statement}");
    }
    connection
        .execute_batch("UPDATE lcm_raw_messages SET store_id = 42")
        .await
        .map_err(|error| error.to_string())?;
    let mut rows = connection
        .query(
            "SELECT rowid FROM lcm_raw_messages_fts
             WHERE lcm_raw_messages_fts MATCH 'changedplaceholder'",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        42
    );
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    connection
        .execute_batch("DELETE FROM lcm_raw_messages")
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(matches(&connection, "changedplaceholder").await?, 0);
    Ok(())
}
