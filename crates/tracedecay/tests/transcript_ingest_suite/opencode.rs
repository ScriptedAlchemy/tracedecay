use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;

use crate::restart_atomicity::{ingest_global_sources_for_provider, open_project_session_db};
use crate::support::{assert_path_text_eq, setup};

/// An opencode session's durable source is the profile `opencode.db` its
/// message rows were read from.
#[tokio::test]
async fn opencode_state_db_is_the_session_transcript_path() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let data_dir = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/opencode"
    } else if cfg!(target_os = "windows") {
        "AppData/Local/opencode"
    } else {
        ".local/share/opencode"
    });
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                directory TEXT NOT NULL
             );
             CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX message_session_time_created_id_idx
                ON message(session_id, time_created, id);
             CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX part_message_id_id_idx ON part(message_id, id);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session(id, directory) VALUES ('ses_project', ?1)",
            rusqlite::params![project.to_string_lossy()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('msg_1', 'ses_project', 1, ?1)",
            rusqlite::params![
                serde_json::json!({
                    "role": "user", "time": {"created": 1}
                })
                .to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_1', 'msg_1', 'ses_project', ?1)",
            rusqlite::params![
                serde_json::json!({
                    "type": "text",
                    "text": "Investigate the billing pipeline regression"
                })
                .to_string()
            ],
        )
        .unwrap();
    drop(connection);

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::OpenCode)).await;

    let session = db
        .get_session("opencode", "ses_project")
        .await
        .expect("opencode session should be stored");
    assert_path_text_eq(
        session
            .transcript_path
            .as_deref()
            .expect("session transcript path"),
        &database,
    );
    assert_eq!(
        db.search_session_messages("opencode", None, "billing pipeline", 10)
            .await
            .len(),
        1
    );
}

/// Two messages in one OpenCode session share one source. Per-record
/// generation hashes made the second message a `cursor_conflict` and kept
/// project catch-up on HistoricalRetry.
#[tokio::test]
async fn opencode_second_message_uses_the_batch_generation() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let data_dir = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/opencode"
    } else if cfg!(target_os = "windows") {
        "AppData/Local/opencode"
    } else {
        ".local/share/opencode"
    });
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                directory TEXT NOT NULL
             );
             CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX message_session_time_created_id_idx
                ON message(session_id, time_created, id);
             CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX part_message_id_id_idx ON part(message_id, id);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session(id, directory) VALUES ('ses_project', ?1)",
            rusqlite::params![project.to_string_lossy()],
        )
        .unwrap();
    for (message_id, created, part_id, text) in [
        (
            "msg_1",
            1,
            "part_1",
            "Investigate the billing pipeline regression",
        ),
        (
            "msg_2",
            2,
            "part_2",
            "The billing pipeline regression is fixed.",
        ),
    ] {
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES (?1, 'ses_project', ?2, ?3)",
                rusqlite::params![
                    message_id,
                    created,
                    serde_json::json!({
                        "role": "user", "time": {"created": created}
                    })
                    .to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part(id, message_id, session_id, data)
                 VALUES (?1, ?2, 'ses_project', ?3)",
                rusqlite::params![
                    part_id,
                    message_id,
                    serde_json::json!({ "type": "text", "text": text }).to_string()
                ],
            )
            .unwrap();
    }
    drop(connection);

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::OpenCode)).await;

    assert_eq!(
        db.search_session_messages("opencode", None, "billing pipeline", 10)
            .await
            .len(),
        2,
        "both OpenCode messages must project instead of CAS-conflicting"
    );
}

/// An in-place part edit changes no database file identity, so covered-range
/// admission would skip the rewritten message forever. The record generation
/// must derive from record content so the edit re-projects while unchanged
/// siblings stay covered.
#[tokio::test]
async fn opencode_in_place_part_edit_reprojects_its_message() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let data_dir = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/opencode"
    } else if cfg!(target_os = "windows") {
        "AppData/Local/opencode"
    } else {
        ".local/share/opencode"
    });
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                directory TEXT NOT NULL
             );
             CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX message_session_time_created_id_idx
                ON message(session_id, time_created, id);
             CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX part_message_id_id_idx ON part(message_id, id);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session(id, directory) VALUES ('ses_project', ?1)",
            rusqlite::params![project.to_string_lossy()],
        )
        .unwrap();
    for (message_id, created, part_id, text) in [
        (
            "msg_1",
            1,
            "part_1",
            "Investigate the billing pipeline regression",
        ),
        (
            "msg_2",
            2,
            "part_2",
            "The billing pipeline regression is fixed.",
        ),
    ] {
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES (?1, 'ses_project', ?2, ?3)",
                rusqlite::params![
                    message_id,
                    created,
                    serde_json::json!({
                        "role": "user", "time": {"created": created}
                    })
                    .to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part(id, message_id, session_id, data)
                 VALUES (?1, ?2, 'ses_project', ?3)",
                rusqlite::params![
                    part_id,
                    message_id,
                    serde_json::json!({ "type": "text", "text": text }).to_string()
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "UPDATE part SET data = ?1 WHERE id = 'part_1'",
            rusqlite::params![
                serde_json::json!({
                    "type": "text",
                    "text": "Re-draft the billing pipeline regression analysis"
                })
                .to_string()
            ],
        )
        .unwrap();
    drop(connection);

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::OpenCode)).await;

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE part SET data = ?1 WHERE id = 'part_1'",
            rusqlite::params![
                serde_json::json!({
                    "type": "text",
                    "text": "Rewrite the billing pipeline regression analysis"
                })
                .to_string()
            ],
        )
        .unwrap();
    drop(connection);

    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::OpenCode)).await;

    let rewritten = db
        .search_session_messages("opencode", None, "Rewrite", 10)
        .await;
    assert_eq!(
        rewritten
            .iter()
            .map(|result| result.message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["msg_1"],
        "the edited part must re-project its message after the rewrite sweep"
    );
    assert!(
        rewritten[0].message.text.contains("Rewrite the billing"),
        "the projected text must carry the edited part content"
    );
    assert_eq!(
        db.search_session_messages("opencode", None, "Re-draft", 10)
            .await
            .len(),
        0,
        "the superseded part text must not survive projection"
    );
    let sibling = db
        .search_session_messages("opencode", None, "fixed", 10)
        .await;
    assert_eq!(
        sibling
            .iter()
            .map(|result| result.message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["msg_2"],
        "the unchanged sibling message must remain projected"
    );
}
