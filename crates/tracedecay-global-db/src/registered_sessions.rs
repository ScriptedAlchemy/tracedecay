use std::fmt::Write as _;

use serde_json::Value as JsonValue;

use tracedecay_runtime_core::db::engine::Value;
use tracedecay_sessions::compatibility::{
    RelatedMessageCopyIdentity, dedupe_related_message_copies, rerank_fetch_limit,
};

use super::{
    RegisteredGlobalDb, SESSION_MESSAGE_SEARCH_MAX_FETCH, SessionActivityRow, SessionIngestHealth,
    SessionMessageRecord, SessionMessageSearchResult, SessionRecord,
    UNIX_TIMESTAMP_MILLIS_THRESHOLD, downrank_inventory_messages,
    interleave_workflow_search_results, session_fts_query,
};

const SESSION_INGEST_HEALTH_PAGE_SIZE: i64 = 512;

impl RegisteredGlobalDb {
    pub async fn cursor_session_ingest_health(&self) -> Result<SessionIngestHealth, String> {
        self.session_ingest_health_for_provider(Some("cursor"))
            .await
    }

    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<SessionIngestHealth, String> {
        let mut health = SessionIngestHealth::default();
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin session ingest health snapshot: {error}"))?;
        let mut after_path = String::new();
        loop {
            let mut rows = snapshot
                .query(
                    "SELECT paths.transcript_path,
                            COALESCE(offsets.byte_offset, 0),
                            COALESCE(offsets.mtime, 0)
                     FROM (
                         SELECT DISTINCT transcript_path
                         FROM sessions
                         WHERE (?1 IS NULL OR provider = ?1)
                           AND transcript_path IS NOT NULL
                           AND transcript_path != ''
                           AND transcript_path > ?2
                         ORDER BY transcript_path
                         LIMIT ?3
                     ) AS paths
                     LEFT JOIN parse_offsets AS offsets
                       ON offsets.file_path = paths.transcript_path
                     ORDER BY paths.transcript_path",
                    tracedecay_runtime_core::db::engine::params![
                        provider,
                        after_path.as_str(),
                        SESSION_INGEST_HEALTH_PAGE_SIZE
                    ],
                )
                .await
                .map_err(|error| format!("failed to query session ingest health: {error}"))?;
            let mut page = Vec::with_capacity(SESSION_INGEST_HEALTH_PAGE_SIZE as usize);
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| format!("failed to read session ingest health: {error}"))?
            {
                let path = row
                    .get::<String>(0)
                    .map_err(|error| format!("failed to decode transcript path: {error}"))?;
                let byte_offset = u64::try_from(
                    row.get::<i64>(1)
                        .map_err(|error| format!("failed to decode transcript offset: {error}"))?,
                )
                .map_err(|error| format!("invalid transcript offset: {error}"))?;
                let mtime = u64::try_from(
                    row.get::<i64>(2)
                        .map_err(|error| format!("failed to decode transcript mtime: {error}"))?,
                )
                .map_err(|error| format!("invalid transcript mtime: {error}"))?;
                page.push((path, byte_offset, mtime));
            }
            drop(rows);
            if page.is_empty() {
                break;
            }
            for (path, byte_offset, mtime) in &page {
                let Ok(metadata) = std::fs::metadata(path) else {
                    continue;
                };
                health.tracked_transcripts = health.tracked_transcripts.saturating_add(1);
                if *mtime > 0 {
                    let mtime = i64::try_from(*mtime).unwrap_or(i64::MAX);
                    health.last_ingest_unix = Some(
                        health
                            .last_ingest_unix
                            .map_or(mtime, |previous| previous.max(mtime)),
                    );
                }
                let pending = metadata.len().saturating_sub(*byte_offset);
                if pending > 0 {
                    health.pending_transcripts = health.pending_transcripts.saturating_add(1);
                    health.pending_bytes = health.pending_bytes.saturating_add(pending);
                    health.max_transcript_pending_bytes =
                        health.max_transcript_pending_bytes.max(pending);
                }
            }
            after_path = page
                .last()
                .map(|(path, _, _)| path.clone())
                .ok_or_else(|| "session ingest health page unexpectedly empty".to_owned())?;
            if page.len() < SESSION_INGEST_HEALTH_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(health)
    }

    pub async fn has_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin session message lookup snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT EXISTS(
                    SELECT 1 FROM session_messages
                    WHERE provider = ?1 AND message_id = ?2
                 )",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| format!("failed to query session message existence: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session message existence: {error}"))?
            .ok_or_else(|| "session message existence returned no row".to_string())?;
        row.get::<i64>(0)
            .map(|exists| exists != 0)
            .map_err(|error| format!("failed to decode session message existence: {error}"))
    }

    pub async fn session_message_count(&self) -> Result<i64, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin session message count snapshot: {error}"))?;
        let mut rows = snapshot
            .query("SELECT COUNT(*) FROM session_messages", ())
            .await
            .map_err(|error| format!("failed to count session messages: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session message count: {error}"))?
            .ok_or_else(|| "session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|error| format!("failed to decode session message count: {error}"))
    }

    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin project message count snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*)
                 FROM session_messages m
                 JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
                 WHERE s.project_key = ?1",
                tracedecay_runtime_core::db::engine::params![project_key],
            )
            .await
            .map_err(|error| format!("failed to count project session messages: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read project session message count: {error}"))?
            .ok_or_else(|| "project session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|error| format!("failed to decode project session message count: {error}"))
    }

    pub async fn session_messages_after(
        &self,
        provider: &str,
        session_id: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<SessionActivityRow>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin session activity snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT timestamp, ordinal, kind, tool_names, metadata_json
                 FROM session_messages
                 WHERE provider = ?1 AND session_id = ?2
                   AND timestamp IS NOT NULL AND timestamp >= ?3
                 ORDER BY timestamp, ordinal
                 LIMIT ?4",
                tracedecay_runtime_core::db::engine::params![
                    provider,
                    session_id,
                    since_ts,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
            )
            .await
            .map_err(|error| format!("failed to query session messages after hint: {error}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session messages after hint: {error}"))?
        {
            out.push(SessionActivityRow {
                timestamp: row.get::<Option<i64>>(0).ok().flatten(),
                ordinal: row.get::<i64>(1).unwrap_or_default(),
                kind: row.get::<Option<String>>(2).ok().flatten(),
                tool_names: row.get::<Option<String>>(3).ok().flatten(),
                metadata_json: row.get::<Option<String>>(4).ok().flatten(),
            });
        }
        Ok(out)
    }

    pub async fn latest_session_activity_secs(&self) -> Option<i64> {
        let snapshot = self.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "WITH latest_seconds AS (
                    SELECT timestamp FROM session_messages
                    WHERE timestamp IS NOT NULL
                      AND timestamp < ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 ),
                 latest_millis AS (
                    SELECT timestamp FROM session_messages
                    WHERE timestamp >= ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 )
                 SELECT timestamp FROM latest_seconds
                 UNION ALL
                 SELECT timestamp FROM latest_millis",
                [UNIX_TIMESTAMP_MILLIS_THRESHOLD],
            )
            .await
            .ok()?;
        let mut latest: Option<i64> = None;
        while let Some(row) = rows.next().await.ok()? {
            let timestamp = row.get::<i64>(0).ok()?;
            let normalized = if timestamp >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
                timestamp / 1000
            } else {
                timestamp
            };
            latest = Some(latest.map_or(normalized, |current| current.max(normalized)));
        }
        latest
    }

    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<SessionMessageRecord> {
        let snapshot = self.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                        model, tool_names, source_path, source_offset, metadata_json
                 FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .ok()?;
        row_to_message(&rows.next().await.ok()??, 0)
    }

    /// Searches message text for a provider, optionally constrained to one project.
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        let fts_query = session_fts_query(query);
        if fts_query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let literal_terms = query
            .split_whitespace()
            .filter(|term| term.contains('-'))
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let fetch_limit = rerank_fetch_limit(limit, SESSION_MESSAGE_SEARCH_MAX_FETCH);
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(_) => return Vec::new(),
        };

        let mut sql = "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                m.provider, m.message_id, m.session_id, m.role, m.timestamp, m.ordinal, m.text,
                m.kind, m.model, m.tool_names, m.source_path, m.source_offset, m.metadata_json,
                bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0) AS rank
             FROM session_messages_fts
             JOIN session_messages m ON session_messages_fts.rowid = m.rowid
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE session_messages_fts MATCH ?1"
            .to_owned();
        let mut query_params = vec![Value::Text(fts_query), Value::Text(provider.to_owned())];
        let _ = write!(sql, " AND m.provider = ?{}", query_params.len());
        if let Some(project_key) = project_key {
            query_params.push(Value::Text(project_key.to_owned()));
            let _ = write!(
                sql,
                " AND (s.project_key = ?{0} OR s.project_path = ?{0})",
                query_params.len()
            );
        }
        for term in &literal_terms {
            query_params.push(Value::Text(term.clone()));
            let _ = write!(
                sql,
                " AND instr(lower(m.text), ?{}) > 0",
                query_params.len()
            );
        }
        query_params.push(Value::Integer(
            i64::try_from(fetch_limit).unwrap_or(i64::MAX),
        ));
        let _ = write!(
            sql,
            " ORDER BY bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0)
              LIMIT ?{}",
            query_params.len()
        );

        let mut transcript_results = Vec::new();
        if let Ok(mut rows) = snapshot.query(&sql, query_params).await {
            while let Ok(Some(row)) = rows.next().await {
                let Some(session) = row_to_session(&row) else {
                    continue;
                };
                let Some(message) = row_to_message(&row, 13) else {
                    continue;
                };
                let score = row.get::<f64>(26).map_or(0.0, |rank| -rank);
                transcript_results.push(SessionMessageSearchResult {
                    session,
                    message,
                    score,
                });
            }
        }

        let workflow_results =
            search_workflow_facts(&snapshot, provider, project_key, query, fetch_limit).await;
        let mut results = interleave_workflow_search_results(transcript_results, workflow_results);
        results = dedupe_related_message_copies(results, |result| RelatedMessageCopyIdentity {
            provider: &result.session.provider,
            family_session_id: result
                .session
                .parent_session_id
                .as_deref()
                .unwrap_or(&result.session.session_id),
            session_id: &result.session.session_id,
            is_subagent: result.session.is_subagent,
            content: &result.message.text,
        });
        downrank_inventory_messages(&mut results);
        results.truncate(limit);
        results
    }

    /// Lists each session's latest canonical goal state, newest first.
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        if limit == 0 {
            return Vec::new();
        }
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(_) => return Vec::new(),
        };
        let mut sql = "WITH ranked_goals AS (
                SELECT w.*,
                       ROW_NUMBER() OVER (
                           PARTITION BY w.provider, w.session_id
                           ORDER BY w.observation_sequence DESC, w.fact_ordinal DESC
                       ) AS goal_rank
                FROM observation_workflow_facts w
                WHERE w.projector_version = 'claude-session-message-v4'
                  AND w.semantic_kind = 'goal'
            )
             SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
                w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
                w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
                w.source_sequence, w.native_timestamp, w.observation_sequence,
                w.ordering_domain, w.content_json, w.content_text
             FROM ranked_goals w
             JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
             WHERE w.goal_rank = 1"
            .to_owned();
        let mut query_params = Vec::new();
        if let Some(project_key) = project_key {
            query_params.push(Value::Text(project_key.to_owned()));
            let _ = write!(
                sql,
                " AND (s.project_key = ?{0} OR s.project_path = ?{0})",
                query_params.len()
            );
        }
        query_params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        let _ = write!(
            sql,
            " ORDER BY COALESCE(w.native_timestamp, 0) DESC,
                       w.observation_sequence DESC, w.fact_ordinal DESC
              LIMIT ?{}",
            query_params.len()
        );

        let mut results = Vec::new();
        if let Ok(mut rows) = snapshot.query(&sql, query_params).await {
            while let Ok(Some(row)) = rows.next().await {
                let Some(session) = row_to_session(&row) else {
                    continue;
                };
                let Some(message) = row_to_workflow_message(&row, 13) else {
                    continue;
                };
                results.push(SessionMessageSearchResult {
                    session,
                    message,
                    score: 0.0,
                });
            }
        }

        let mut legacy_sql = "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                m.provider, m.message_id, m.session_id, m.role, m.timestamp, m.ordinal, m.text,
                m.kind, m.model, m.tool_names, m.source_path, m.source_offset, m.metadata_json
             FROM session_messages m
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE m.kind = 'goal'
               AND m.ordinal = (
                   SELECT MAX(m2.ordinal) FROM session_messages m2
                   WHERE m2.provider = m.provider
                     AND m2.session_id = m.session_id
                     AND m2.kind = 'goal'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM observation_workflow_facts w
                   WHERE w.projector_version = 'claude-session-message-v4'
                     AND w.provider = m.provider
                     AND w.session_id = m.session_id
                     AND w.semantic_kind = 'goal'
               )"
        .to_owned();
        let mut legacy_params = Vec::new();
        if let Some(project_key) = project_key {
            legacy_params.push(Value::Text(project_key.to_owned()));
            let _ = write!(
                legacy_sql,
                " AND (s.project_key = ?{0} OR s.project_path = ?{0})",
                legacy_params.len()
            );
        }
        legacy_params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        let _ = write!(
            legacy_sql,
            " ORDER BY COALESCE(m.timestamp, 0) DESC, m.ordinal DESC LIMIT ?{}",
            legacy_params.len()
        );
        if let Ok(mut rows) = snapshot.query(&legacy_sql, legacy_params).await {
            while let Ok(Some(row)) = rows.next().await {
                let Some(session) = row_to_session(&row) else {
                    continue;
                };
                let Some(message) = row_to_message(&row, 13) else {
                    continue;
                };
                results.push(SessionMessageSearchResult {
                    session,
                    message,
                    score: 0.0,
                });
            }
        }
        results.sort_by(|left, right| {
            right
                .message
                .timestamp
                .unwrap_or_default()
                .cmp(&left.message.timestamp.unwrap_or_default())
                .then_with(|| right.message.ordinal.cmp(&left.message.ordinal))
                .then_with(|| left.message.message_id.cmp(&right.message.message_id))
        });
        results.truncate(limit);
        results
    }

    /// Reads the canonical workflow fact columns used by projection acceptance.
    pub async fn workflow_fact_rows(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<(String, Option<String>, Option<String>)>>
    {
        let snapshot = self.read_snapshot().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "begin registered workflow fact snapshot".to_owned(),
                message: error.to_string(),
            }
        })?;
        let mut rows = snapshot
            .query(
                "SELECT semantic_kind, status, state
                 FROM observation_workflow_facts
                 ORDER BY observation_sequence, fact_ordinal",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered workflow facts".to_owned(),
                    message: error.to_string(),
                },
            )?;
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered workflow fact row".to_owned(),
                message: error.to_string(),
            }
        })? {
            values.push((
                row.get(0).map_err(|error| {
                    tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact kind".to_owned(),
                        message: error.to_string(),
                    }
                })?,
                row.get(1).map_err(|error| {
                    tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact status".to_owned(),
                        message: error.to_string(),
                    }
                })?,
                row.get(2).map_err(|error| {
                    tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact state".to_owned(),
                        message: error.to_string(),
                    }
                })?,
            ));
        }
        Ok(values)
    }
}

async fn search_workflow_facts(
    snapshot: &tracedecay_runtime_core::db::engine::ReadSnapshot,
    provider: &str,
    project_key: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<SessionMessageSearchResult> {
    let terms = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|character: char| {
                !character.is_alphanumeric() && character != '-' && character != '_'
            })
            .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut sql = "SELECT
            s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
            s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
            s.is_subagent, s.agent_id, s.parent_tool_use_id,
            w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
            w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
            w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
            w.source_sequence, w.native_timestamp, w.observation_sequence,
            w.ordering_domain, w.content_json, w.content_text
         FROM observation_workflow_facts w
         JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
         WHERE w.projector_version = 'claude-session-message-v4'"
        .to_owned();
    let mut query_params = vec![Value::Text(provider.to_owned())];
    let _ = write!(sql, " AND w.provider = ?{}", query_params.len());
    if let Some(project_key) = project_key {
        query_params.push(Value::Text(project_key.to_owned()));
        let _ = write!(
            sql,
            " AND (s.project_key = ?{0} OR s.project_path = ?{0})",
            query_params.len()
        );
    }
    let mut term_predicates = Vec::with_capacity(terms.len());
    for term in terms {
        query_params.push(Value::Text(term));
        term_predicates.push(format!(
            "instr(lower(w.content_text), ?{}) > 0",
            query_params.len()
        ));
    }
    let _ = write!(sql, " AND ({})", term_predicates.join(" AND "));
    query_params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let _ = write!(
        sql,
        " ORDER BY CASE WHEN w.item_order IS NULL THEN 1 ELSE 0 END,
                  w.item_order, COALESCE(w.native_timestamp, 0) DESC,
                  w.observation_sequence DESC, w.fact_ordinal
          LIMIT ?{}",
        query_params.len()
    );

    let Ok(mut rows) = snapshot.query(&sql, query_params).await else {
        return Vec::new();
    };
    let mut results = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let Some(session) = row_to_session(&row) else {
            continue;
        };
        let Some(message) = row_to_workflow_message(&row, 13) else {
            continue;
        };
        results.push(SessionMessageSearchResult {
            session,
            message,
            score: 0.0,
        });
    }
    results
}

fn row_to_session(row: &tracedecay_runtime_core::db::engine::Row) -> Option<SessionRecord> {
    Some(SessionRecord {
        provider: row.get(0).ok()?,
        session_id: row.get(1).ok()?,
        project_key: row.get(2).ok()?,
        project_path: row.get(3).ok()?,
        title: row.get(4).ok(),
        started_at: row.get(5).ok(),
        ended_at: row.get(6).ok(),
        transcript_path: row.get(7).ok(),
        metadata_json: row.get(8).ok(),
        parent_session_id: row.get(9).ok(),
        is_subagent: row.get::<i64>(10).ok()? != 0,
        agent_id: row.get(11).ok(),
        parent_tool_use_id: row.get(12).ok(),
    })
}

fn row_to_message(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> Option<SessionMessageRecord> {
    Some(SessionMessageRecord {
        provider: row.get(offset).ok()?,
        message_id: row.get(offset + 1).ok()?,
        session_id: row.get(offset + 2).ok()?,
        role: row.get(offset + 3).ok()?,
        timestamp: row.get(offset + 4).ok(),
        ordinal: row.get(offset + 5).ok()?,
        text: row.get(offset + 6).ok()?,
        kind: row.get(offset + 7).ok(),
        model: row.get(offset + 8).ok(),
        tool_names: row.get(offset + 9).ok(),
        source_path: row.get(offset + 10).ok(),
        source_offset: row.get(offset + 11).ok(),
        metadata_json: row.get(offset + 12).ok(),
    })
}

fn row_to_workflow_message(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> Option<SessionMessageRecord> {
    let provider: String = row.get(offset).ok()?;
    let observation_id: String = row.get(offset + 1).ok()?;
    let fact_ordinal: i64 = row.get(offset + 2).ok()?;
    let session_id: String = row.get(offset + 3).ok()?;
    let semantic_kind: String = row.get(offset + 4).ok()?;
    let provider_reference: Option<String> = row.get(offset + 5).ok();
    let item_id: Option<String> = row.get(offset + 6).ok();
    let parent_reference: Option<String> = row.get(offset + 7).ok();
    let list_reference: Option<String> = row.get(offset + 8).ok();
    let state: Option<String> = row.get(offset + 9).ok();
    let status: Option<String> = row.get(offset + 10).ok();
    let item_order: Option<i64> = row.get(offset + 11).ok();
    let revision: Option<String> = row.get(offset + 12).ok();
    let event_sequence: Option<i64> = row.get(offset + 13).ok();
    let source_sequence: Option<i64> = row.get(offset + 14).ok();
    let native_timestamp: Option<i64> = row.get(offset + 15).ok();
    let observation_sequence: i64 = row.get(offset + 16).ok()?;
    let ordering_domain: String = row.get(offset + 17).ok()?;
    let content_json: Option<String> = row.get(offset + 18).ok();
    let content_text: String = row.get(offset + 19).ok()?;

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "observation_id".to_owned(),
        JsonValue::String(observation_id.clone()),
    );
    metadata.insert("fact_ordinal".to_owned(), JsonValue::from(fact_ordinal));
    metadata.insert(
        "ordering_domain".to_owned(),
        JsonValue::String(ordering_domain),
    );
    for (key, value) in [
        ("provider_reference", provider_reference),
        ("item_id", item_id),
        ("parent_reference", parent_reference),
        ("list_reference", list_reference),
        ("state", state),
        ("status", status),
        ("revision", revision),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), JsonValue::String(value));
        }
    }
    for (key, value) in [
        ("item_order", item_order),
        ("event_sequence", event_sequence),
        ("source_sequence", source_sequence),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), JsonValue::from(value));
        }
    }
    if let Some(content_json) = content_json
        && let Ok(content) = serde_json::from_str(&content_json)
    {
        metadata.insert("content".to_owned(), content);
    }

    Some(SessionMessageRecord {
        provider,
        message_id: format!("workflow/{observation_id}/{fact_ordinal}"),
        session_id,
        role: "system".to_owned(),
        timestamp: native_timestamp,
        ordinal: event_sequence
            .or(source_sequence)
            .unwrap_or(observation_sequence),
        text: content_text,
        kind: Some(semantic_kind),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: Some(JsonValue::Object(metadata).to_string()),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::ParseOffset;
    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    fn session(provider: &str, session_id: &str, transcript_path: &str) -> SessionRecord {
        SessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            project_key: "/project".to_owned(),
            project_path: "/project".to_owned(),
            title: None,
            started_at: None,
            ended_at: None,
            transcript_path: Some(transcript_path.to_owned()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    #[tokio::test]
    async fn cursor_ingest_authority_is_shared_and_provider_scoped() {
        let profile = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap();
        let cursor_path = profile.path().join("cursor.jsonl");
        let claude_path = profile.path().join("claude.jsonl");
        std::fs::write(&cursor_path, b"0123456789").unwrap();
        std::fs::write(&claude_path, b"01234567890123456789").unwrap();
        assert!(
            database
                .upsert_session(&session(
                    "cursor",
                    "session.cursor",
                    cursor_path.to_str().unwrap()
                ))
                .await
        );
        assert!(
            database
                .upsert_session(&session(
                    "claude",
                    "session.claude",
                    claude_path.to_str().unwrap()
                ))
                .await
        );
        database
            .set_parse_offset(
                cursor_path.to_str().unwrap(),
                ParseOffset {
                    byte_offset: 4,
                    mtime: 100,
                    file_id: 0,
                },
            )
            .await
            .unwrap();
        database
            .set_parse_offset(
                claude_path.to_str().unwrap(),
                ParseOffset {
                    byte_offset: 20,
                    mtime: 200,
                    file_id: 0,
                },
            )
            .await
            .unwrap();

        let runtime_surface = database.cursor_session_ingest_health().await.unwrap();
        let status_surface = database.cursor_session_ingest_health().await.unwrap();

        assert_eq!(runtime_surface, status_surface);
        assert_eq!(runtime_surface.tracked_transcripts, 1);
        assert_eq!(runtime_surface.pending_transcripts, 1);
        assert_eq!(runtime_surface.pending_bytes, 6);
        assert_eq!(runtime_surface.max_transcript_pending_bytes, 6);
        assert_eq!(runtime_surface.last_ingest_unix, Some(100));
    }
}
