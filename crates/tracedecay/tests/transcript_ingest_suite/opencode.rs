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
