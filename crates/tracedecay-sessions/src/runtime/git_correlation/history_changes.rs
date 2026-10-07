//! The source revision of retained session history, including in-place edits.
//!
//! A message's stable identity is not a revision. This journal keeps one row
//! per session; replacing that row allocates a new SQLite sequence in the
//! same transaction that changes the retained messages or session metadata.

use tracedecay_runtime_core::db::engine::{Executor, params};

use super::GitCorrelationError;

/// Install after the sessions and raw-message authorities exist.
///
/// The caller holds the schema write transaction. Recreate the shipped triggers
/// while preserving their journal: an outer upsert overrides a trigger's
/// `OR REPLACE` policy, so replacement must delete the exact old row first.
pub async fn install_history_change_schema(
    conn: &(impl Executor + ?Sized),
) -> Result<(), GitCorrelationError> {
    let mut tables = conn.query(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'git_history_session_change'",
        (),
    ).await?;
    let existed = tables.next().await?.is_some();
    drop(tables);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS git_history_session_change (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             provider TEXT NOT NULL,
             session_id TEXT NOT NULL,
             UNIQUE(provider, session_id)
         );
         DROP TRIGGER IF EXISTS git_history_session_insert;
         CREATE TRIGGER git_history_session_insert
         AFTER INSERT ON sessions BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = NEW.provider AND session_id = NEW.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (NEW.provider, NEW.session_id);
         END;
         DROP TRIGGER IF EXISTS git_history_session_update;
         CREATE TRIGGER git_history_session_update
         AFTER UPDATE ON sessions
         WHEN OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id
           OR OLD.project_path IS NOT NEW.project_path
           OR OLD.started_at IS NOT NEW.started_at OR OLD.ended_at IS NOT NEW.ended_at
           OR OLD.metadata_json IS NOT NEW.metadata_json
         BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = OLD.provider AND session_id = OLD.session_id
                   AND (OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id);
             INSERT INTO git_history_session_change(provider, session_id)
                 SELECT OLD.provider, OLD.session_id
                 WHERE OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id;
             DELETE FROM git_history_session_change
                 WHERE provider = NEW.provider AND session_id = NEW.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (NEW.provider, NEW.session_id);
         END;
         DROP TRIGGER IF EXISTS git_history_session_delete;
         CREATE TRIGGER git_history_session_delete
         AFTER DELETE ON sessions BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = OLD.provider AND session_id = OLD.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (OLD.provider, OLD.session_id);
         END;
         DROP TRIGGER IF EXISTS git_history_message_insert;
         CREATE TRIGGER git_history_message_insert
         AFTER INSERT ON lcm_raw_messages BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = NEW.provider AND session_id = NEW.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (NEW.provider, NEW.session_id);
         END;
         DROP TRIGGER IF EXISTS git_history_message_update;
         CREATE TRIGGER git_history_message_update
         AFTER UPDATE ON lcm_raw_messages
         WHEN OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id
           OR OLD.timestamp IS NOT NEW.timestamp OR OLD.content_hash IS NOT NEW.content_hash
           OR OLD.metadata_json IS NOT NEW.metadata_json
         BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = OLD.provider AND session_id = OLD.session_id
                   AND (OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id);
             INSERT INTO git_history_session_change(provider, session_id)
                 SELECT OLD.provider, OLD.session_id
                 WHERE OLD.provider IS NOT NEW.provider OR OLD.session_id IS NOT NEW.session_id;
             DELETE FROM git_history_session_change
                 WHERE provider = NEW.provider AND session_id = NEW.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (NEW.provider, NEW.session_id);
         END;
         DROP TRIGGER IF EXISTS git_history_message_delete;
         CREATE TRIGGER git_history_message_delete
         AFTER DELETE ON lcm_raw_messages BEGIN
             DELETE FROM git_history_session_change
                 WHERE provider = OLD.provider AND session_id = OLD.session_id;
             INSERT INTO git_history_session_change(provider, session_id)
                 VALUES (OLD.provider, OLD.session_id);
         END;",
    )
    .await?;
    if !existed {
        conn.execute_batch(
            "INSERT INTO git_history_session_change(provider, session_id)
             SELECT provider, session_id FROM sessions ORDER BY rowid;",
        )
        .await?;
    }
    Ok(())
}

/// Requeue retained history when a captured observation invalidates inference.
/// The caller holds the same write transaction as the evidence retraction.
pub(super) async fn invalidate_session_history(
    conn: &(impl Executor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<(), GitCorrelationError> {
    conn.execute(
        "DELETE FROM git_history_session_change WHERE provider = ?1 AND session_id = ?2",
        params![provider, session_id],
    )
    .await?;
    conn.execute(
        "INSERT INTO git_history_session_change(provider, session_id) VALUES (?1, ?2)",
        params![provider, session_id],
    )
    .await?;
    Ok(())
}
