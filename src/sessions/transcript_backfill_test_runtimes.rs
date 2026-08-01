use std::ops::Deref;
use std::path::Path;

use tracedecay_domain::ProjectId;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::source::{StoredCursor, TranscriptSource};
use tracedecay_sessions::runtime::transcript_backfill::{
    TranscriptFactsBackfillOutcome, advance_transcript_facts_backfill_with_limit_for_test,
    backfill_structured_rows, insert_absent_session_messages_for_test,
    read_structured_backfill_cursor_for_test, structured_backfill_cursor_key_prefix_for_test,
    structured_backfill_marker_name_for_test, transcript_facts_backfill_status,
    write_structured_backfill_cursor_for_test,
};

/// Opaque registered ProjectSessions fixture for transcript-facts backfill tests.
#[doc(hidden)]
pub struct TranscriptFactsBackfillTestRuntimeV1 {
    authority: crate::application::host_admission::HostAdmissionTestRuntimeV1,
}

impl TranscriptFactsBackfillTestRuntimeV1 {
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Ok(Self {
            authority: crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        })
    }

    fn database(&self) -> &RegisteredGlobalDb {
        match self
            .authority
            .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
        {
            Some(database) => database,
            None => panic!("transcript facts test runtime has ProjectSessions authority"),
        }
    }

    pub async fn transcript_facts_backfill_status_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<TranscriptFactsBackfillOutcome> {
        transcript_facts_backfill_status(self.database()).await
    }

    pub async fn advance_transcript_facts_backfill_for_test(
        &self,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<TranscriptFactsBackfillOutcome> {
        advance_transcript_facts_backfill_with_limit_for_test(self.database(), limit).await
    }
}

impl Deref for TranscriptFactsBackfillTestRuntimeV1 {
    type Target = crate::application::host_admission::HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}

/// Opaque registered ProjectSessions fixture for structured-backfill integration tests.
#[doc(hidden)]
pub struct StructuredBackfillTestRuntimeV1 {
    authority: crate::application::host_admission::HostAdmissionTestRuntimeV1,
}

impl StructuredBackfillTestRuntimeV1 {
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Ok(Self {
            authority: crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        })
    }

    fn database(&self) -> &RegisteredGlobalDb {
        match self
            .authority
            .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
        {
            Some(database) => database,
            None => panic!("structured backfill test runtime has ProjectSessions authority"),
        }
    }

    pub fn database_path(&self) -> &Path {
        self.database().db_path()
    }

    pub async fn seed_source(
        &self,
        source: &dyn TranscriptSource,
        project_root: &Path,
    ) -> Result<crate::sessions::shared::TranscriptIngestStats, String> {
        let discovery = source.discover_transcript_paths(
            project_root,
            crate::sessions::source::TranscriptDiscoveryBounds::default_walk(),
        );
        let mut stats = crate::sessions::shared::TranscriptIngestStats::default();
        for path in discovery.paths {
            let Some(parsed) = source
                .try_parse_new(&path, StoredCursor::default(), project_root, None)
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            let started_at = parsed
                .messages
                .iter()
                .filter_map(|message| message.timestamp)
                .min();
            let ended_at = parsed
                .messages
                .iter()
                .filter_map(|message| message.timestamp)
                .max();
            let transaction = self
                .database()
                .begin_write_transaction()
                .await
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO sessions
                         (provider, session_id, project_key, project_path, title, started_at,
                          ended_at, transcript_path, metadata_json, parent_session_id,
                          is_subagent, agent_id, parent_tool_use_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(provider, session_id) DO UPDATE SET
                        project_key = excluded.project_key,
                        project_path = excluded.project_path,
                        title = excluded.title,
                        started_at = excluded.started_at,
                        ended_at = excluded.ended_at,
                        transcript_path = excluded.transcript_path,
                        metadata_json = excluded.metadata_json,
                        parent_session_id = excluded.parent_session_id,
                        is_subagent = excluded.is_subagent,
                        agent_id = excluded.agent_id,
                        parent_tool_use_id = excluded.parent_tool_use_id",
                    params![
                        source.provider(),
                        parsed.draft.session_id.as_str(),
                        parsed.draft.project_key.as_str(),
                        parsed.draft.project_path.as_str(),
                        parsed.draft.title.as_deref(),
                        started_at,
                        ended_at,
                        path.to_string_lossy().as_ref(),
                        parsed.draft.metadata_json.as_deref(),
                        parsed.draft.parent_session_id.as_deref(),
                        i64::from(parsed.draft.is_subagent),
                        parsed.draft.agent_id.as_deref(),
                        parsed.draft.parent_tool_use_id.as_deref(),
                    ],
                )
                .await
                .map_err(|error| error.to_string())?;
            let inserted = insert_absent_session_messages_for_test(&transaction, &parsed.messages)
                .await
                .ok_or_else(|| "seed structured transcript messages".to_string())?;
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
            stats.sessions_upserted = stats.sessions_upserted.saturating_add(1);
            stats.messages_upserted = stats.messages_upserted.saturating_add(inserted);
        }
        Ok(stats)
    }

    pub async fn run(&self) -> Option<u64> {
        backfill_structured_rows(self.database())
            .await
            .map(|stats| stats.inserted)
    }

    pub async fn count_kind(&self, provider: &str, kind: &str) -> Result<i64, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM session_messages WHERE provider = ?1 AND kind = ?2",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing structured kind count".to_string())?
            .get(0)
            .map_err(|error| error.to_string())
    }

    pub async fn remove_kind_and_reset(&self, provider: &str, kind: &str) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM lcm_raw_messages
                 WHERE provider = ?1
                   AND message_id IN (
                       SELECT message_id FROM session_messages
                       WHERE provider = ?1 AND kind = ?2)",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = ?1 AND kind = ?2",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .ok();
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn goal_row(&self) -> Result<(String, Option<String>, Option<String>), String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT text, kind, metadata_json FROM session_messages
                 WHERE provider = 'codex' AND kind = 'goal'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let row = rows
            .next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing Codex goal row".to_string())?;
        Ok((
            row.get(0).map_err(|error| error.to_string())?,
            row.get(1).map_err(|error| error.to_string())?,
            row.get(2).map_err(|error| error.to_string())?,
        ))
    }

    pub async fn marker_version(&self, provider: Option<&str>) -> Result<Option<i64>, String> {
        let name = structured_backfill_marker_name_for_test(provider);
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations WHERE name = ?1",
                params![name],
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())
            .map(|row| row.and_then(|row| row.get(0).ok()))
    }

    pub async fn session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<crate::sessions::SessionRecord>, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id,
                        is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .map_err(|error| error.to_string())?;
        let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        Ok(Some(crate::sessions::SessionRecord {
            provider: row.get(0).map_err(|error| error.to_string())?,
            session_id: row.get(1).map_err(|error| error.to_string())?,
            project_key: row.get(2).map_err(|error| error.to_string())?,
            project_path: row.get(3).map_err(|error| error.to_string())?,
            title: row.get(4).map_err(|error| error.to_string())?,
            started_at: row.get(5).map_err(|error| error.to_string())?,
            ended_at: row.get(6).map_err(|error| error.to_string())?,
            transcript_path: row.get(7).map_err(|error| error.to_string())?,
            metadata_json: row.get(8).map_err(|error| error.to_string())?,
            parent_session_id: row.get(9).map_err(|error| error.to_string())?,
            is_subagent: row.get::<i64>(10).map_err(|error| error.to_string())? != 0,
            agent_id: row.get(11).map_err(|error| error.to_string())?,
            parent_tool_use_id: row.get(12).map_err(|error| error.to_string())?,
        }))
    }

    pub async fn seed_stale_unversioned_cursor(&self) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM lcm_raw_messages
                 WHERE provider = 'codex'
                   AND message_id IN (
                       SELECT message_id FROM session_messages
                       WHERE provider = 'codex' AND kind = 'goal')",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = 'codex' AND kind = 'goal'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = transaction
            .query(
                "SELECT MAX(source_path) FROM session_messages WHERE provider = 'codex'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let last_path: String = rows
            .next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing Codex source path".to_string())?
            .get(0)
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO session_backfill_meta(key, value)
                 VALUES ('structured_backfill_cursor', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![last_path],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn seed_legacy_global_marker(&self, version: i64) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO session_schema_migrations(name, version)
                 VALUES (?1, ?2)",
                params![structured_backfill_marker_name_for_test(None), version],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let cursor_key_prefix = structured_backfill_cursor_key_prefix_for_test();
        for key in [
            cursor_key_prefix.to_string(),
            format!("{cursor_key_prefix}:v{version}"),
        ] {
            transaction
                .execute(
                    "INSERT INTO session_backfill_meta(key, value)
                     VALUES (?1, 'legacy/path.jsonl')",
                    params![key],
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn cursor_count(&self) -> Result<i64, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing structured cursor count".to_string())?
            .get(0)
            .map_err(|error| error.to_string())
    }

    pub async fn write_cursor(&self, value: &str) -> Option<()> {
        write_structured_backfill_cursor_for_test(self.database(), value).await
    }

    pub async fn read_cursor(&self) -> String {
        read_structured_backfill_cursor_for_test(self.database()).await
    }
}
