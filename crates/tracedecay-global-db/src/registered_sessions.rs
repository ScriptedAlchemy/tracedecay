use tracedecay_runtime_core::errors::TraceDecayError;
#[cfg(test)]
pub(crate) use tracedecay_sessions::runtime::SessionRecord;
use tracedecay_sessions::runtime::{
    SessionMessageRecord, SessionMessageSearchResult, SessionStoreAccess,
};

use super::{
    RegisteredGlobalDb, SESSION_MESSAGE_SEARCH_MAX_FETCH, SessionActivityRow, SessionIngestHealth,
    SessionMessageRecord, SessionMessageSearchResult, SessionProviderCoverage,
    SessionProviderCoverageState, SessionRecord, UNIX_TIMESTAMP_MILLIS_THRESHOLD,
    downrank_inventory_messages, global_db_operation_error, global_db_operation_message,
    interleave_workflow_search_results, session_fts_query,
};

#[cfg(test)]
pub(crate) use tracedecay_sessions::runtime::store_access::SESSION_MESSAGES_AFTER_SQL;

impl RegisteredGlobalDb {
    pub async fn cursor_session_ingest_health(&self) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .cursor_session_ingest_health()
            .await
    }

    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .session_ingest_health_for_provider(provider)
            .await
    }

    pub async fn has_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, String> {
        SessionStoreAccess::new(self)
            .has_session_message(provider, message_id)
            .await
    }

    pub async fn session_message_count(&self) -> Result<i64, String> {
        SessionStoreAccess::new(self).session_message_count().await
    }

    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        SessionStoreAccess::new(self)
            .session_message_count_for_project(project_key)
            .await
    }

    pub async fn session_messages_after(
        &self,
        provider: &str,
        session_id: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<SessionActivityRow>, String> {
        SessionStoreAccess::new(self)
            .session_messages_after(provider, session_id, since_ts, limit)
            .await
    }

    /// Unix seconds of the most recent session activity.
    ///
    /// `Ok(None)` is the truthful "this store holds no timestamped messages";
    /// a failed query or an unreadable timestamp stays an error rather than
    /// masquerading as an idle store.
    #[hotpath::measure(future = true, label = "global_db.registered_sessions.activity")]
    pub async fn latest_session_activity_secs(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
        const OPERATION: &str = "read latest session activity";
        let mut rows = self
            .read_connection()
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
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut latest: Option<i64> = None;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let timestamp = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let normalized = if timestamp >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
                timestamp / 1000
            } else {
                timestamp
            };
            latest = Some(latest.map_or(normalized, |current| current.max(normalized)));
        }
        Ok(latest)
    }

    /// Reads one message by provider and id. `Ok(None)` is truthful absence;
    /// snapshot, query, and row-decode failures stay typed errors.
    #[hotpath::measure(future = true, label = "global_db.registered_sessions.get")]
    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<SessionMessageRecord>> {
        const OPERATION: &str = "read registered session message";
        let snapshot = self.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                        model, tool_names, source_path, source_offset, metadata_json
                 FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Ok(None);
        };
        row_to_message(&row, 0)
            .map(Some)
            .map_err(|message| global_db_operation_message(OPERATION, message))
    }

    /// Searches message text for a provider, optionally constrained to one project.
    ///
    /// `Ok(vec![])` is the truthful "nothing matched"; snapshot, query, and
    /// row-decode failures are typed errors instead of an empty result page.
    #[hotpath::measure(future = true, label = "global_db.registered_sessions.search")]
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<Vec<SessionMessageSearchResult>> {
        const OPERATION: &str = "search registered session messages";
        let fts_query = session_fts_query(query);
        if fts_query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let literal_terms = query
            .split_whitespace()
            .filter(|term| term.contains('-'))
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let fetch_limit = rerank_fetch_limit(limit, SESSION_MESSAGE_SEARCH_MAX_FETCH);
        let snapshot = self.read_snapshot().await?;

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
        let mut rows = snapshot
            .query(&sql, query_params)
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let session = row_to_session(&row)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            let message = row_to_message(&row, 13)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            let score = -row
                .get::<f64>(26)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            transcript_results.push(SessionMessageSearchResult {
                session,
                message,
                score,
            });
        }

        let workflow_results =
            search_workflow_facts(&snapshot, provider, project_key, query, fetch_limit).await?;
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
        Ok(results)
    }

    /// Lists each session's latest canonical goal state, newest first.
    /// Goals with no native timestamp rank after all timestamped goals
    /// instead of being assigned a fabricated epoch-zero time.
    #[hotpath::measure(future = true, label = "global_db.registered_sessions.goals")]
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<Vec<SessionMessageSearchResult>> {
        const OPERATION: &str = "list recent registered session goals";
        if limit == 0 {
            return Ok(Vec::new());
        }
        let snapshot = self.read_snapshot().await?;
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
            " ORDER BY (w.native_timestamp IS NULL) ASC, w.native_timestamp DESC,
                       w.observation_sequence DESC, w.fact_ordinal DESC
              LIMIT ?{}",
            query_params.len()
        );

        let mut results = Vec::new();
        let mut rows = snapshot
            .query(&sql, query_params)
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let session = row_to_session(&row)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            let message = row_to_workflow_message(&row, 13)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            results.push(SessionMessageSearchResult {
                session,
                message,
                score: 0.0,
            });
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
            " ORDER BY (m.timestamp IS NULL) ASC, m.timestamp DESC, m.ordinal DESC LIMIT ?{}",
            legacy_params.len()
        );
        let mut rows = snapshot
            .query(&legacy_sql, legacy_params)
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let session = row_to_session(&row)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            let message = row_to_message(&row, 13)
                .map_err(|message| global_db_operation_message(OPERATION, message))?;
            results.push(SessionMessageSearchResult {
                session,
                message,
                score: 0.0,
            });
        }
        results.sort_by(|left, right| {
            descending_timestamp(left.message.timestamp, right.message.timestamp)
                .then_with(|| right.message.ordinal.cmp(&left.message.ordinal))
                .then_with(|| left.message.message_id.cmp(&right.message.message_id))
        });
        results.truncate(limit);
        Ok(results)
    }

    pub async fn workflow_fact_rows(
        &self,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>, TraceDecayError> {
        SessionStoreAccess::new(self).workflow_fact_rows().await
    }
}

/// Newest-first ordering where a missing timestamp ranks after every known
/// timestamp instead of being compared as a fabricated epoch-zero time.
fn descending_timestamp(left: Option<i64>, right: Option<i64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[hotpath::measure(future = true, label = "global_db.registered_sessions.workflow_search")]
async fn search_workflow_facts(
    snapshot: &tracedecay_runtime_core::db::DatabaseEngineReadSnapshot,
    provider: &str,
    project_key: Option<&str>,
    query: &str,
    limit: usize,
) -> tracedecay_runtime_core::errors::Result<Vec<SessionMessageSearchResult>> {
    const OPERATION: &str = "search registered workflow facts";
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
        return Ok(Vec::new());
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
                  w.item_order, (w.native_timestamp IS NULL) ASC, w.native_timestamp DESC,
                  w.observation_sequence DESC, w.fact_ordinal
          LIMIT ?{}",
        query_params.len()
    );

    let mut rows = snapshot
        .query(&sql, query_params)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let session = row_to_session(&row)
            .map_err(|message| global_db_operation_message(OPERATION, message))?;
        let message = row_to_workflow_message(&row, 13)
            .map_err(|message| global_db_operation_message(OPERATION, message))?;
        results.push(SessionMessageSearchResult {
            session,
            message,
            score: 0.0,
        });
    }
    Ok(results)
}

fn session_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode session column '{column}': {error}")
}

fn message_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode session message column '{column}': {error}")
}

fn row_to_session(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> std::result::Result<SessionRecord, String> {
    Ok(SessionRecord {
        provider: row
            .get(0)
            .map_err(|error| session_column_error("provider", &error))?,
        session_id: row
            .get(1)
            .map_err(|error| session_column_error("session_id", &error))?,
        project_key: row
            .get(2)
            .map_err(|error| session_column_error("project_key", &error))?,
        project_path: row
            .get(3)
            .map_err(|error| session_column_error("project_path", &error))?,
        title: row
            .get(4)
            .map_err(|error| session_column_error("title", &error))?,
        started_at: row
            .get(5)
            .map_err(|error| session_column_error("started_at", &error))?,
        ended_at: row
            .get(6)
            .map_err(|error| session_column_error("ended_at", &error))?,
        transcript_path: row
            .get(7)
            .map_err(|error| session_column_error("transcript_path", &error))?,
        metadata_json: row
            .get(8)
            .map_err(|error| session_column_error("metadata_json", &error))?,
        parent_session_id: row
            .get(9)
            .map_err(|error| session_column_error("parent_session_id", &error))?,
        is_subagent: row
            .get::<i64>(10)
            .map_err(|error| session_column_error("is_subagent", &error))?
            != 0,
        agent_id: row
            .get(11)
            .map_err(|error| session_column_error("agent_id", &error))?,
        parent_tool_use_id: row
            .get(12)
            .map_err(|error| session_column_error("parent_tool_use_id", &error))?,
    })
}

fn row_to_message(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> std::result::Result<SessionMessageRecord, String> {
    Ok(SessionMessageRecord {
        provider: row
            .get(offset)
            .map_err(|error| message_column_error("provider", &error))?,
        message_id: row
            .get(offset + 1)
            .map_err(|error| message_column_error("message_id", &error))?,
        session_id: row
            .get(offset + 2)
            .map_err(|error| message_column_error("session_id", &error))?,
        role: row
            .get(offset + 3)
            .map_err(|error| message_column_error("role", &error))?,
        timestamp: row
            .get(offset + 4)
            .map_err(|error| message_column_error("timestamp", &error))?,
        ordinal: row
            .get(offset + 5)
            .map_err(|error| message_column_error("ordinal", &error))?,
        text: row
            .get(offset + 6)
            .map_err(|error| message_column_error("text", &error))?,
        kind: row
            .get(offset + 7)
            .map_err(|error| message_column_error("kind", &error))?,
        model: row
            .get(offset + 8)
            .map_err(|error| message_column_error("model", &error))?,
        tool_names: row
            .get(offset + 9)
            .map_err(|error| message_column_error("tool_names", &error))?,
        source_path: row
            .get(offset + 10)
            .map_err(|error| message_column_error("source_path", &error))?,
        source_offset: row
            .get(offset + 11)
            .map_err(|error| message_column_error("source_offset", &error))?,
        metadata_json: row
            .get(offset + 12)
            .map_err(|error| message_column_error("metadata_json", &error))?,
    })
}

fn workflow_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode workflow fact column '{column}': {error}")
}

fn row_to_workflow_message(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> std::result::Result<SessionMessageRecord, String> {
    let provider: String = row
        .get(offset)
        .map_err(|error| workflow_column_error("provider", &error))?;
    let observation_id: String = row
        .get(offset + 1)
        .map_err(|error| workflow_column_error("observation_id", &error))?;
    let fact_ordinal: i64 = row
        .get(offset + 2)
        .map_err(|error| workflow_column_error("fact_ordinal", &error))?;
    let session_id: String = row
        .get(offset + 3)
        .map_err(|error| workflow_column_error("session_id", &error))?;
    let semantic_kind: String = row
        .get(offset + 4)
        .map_err(|error| workflow_column_error("semantic_kind", &error))?;
    let provider_reference: Option<String> = row
        .get(offset + 5)
        .map_err(|error| workflow_column_error("provider_reference", &error))?;
    let item_id: Option<String> = row
        .get(offset + 6)
        .map_err(|error| workflow_column_error("item_id", &error))?;
    let parent_reference: Option<String> = row
        .get(offset + 7)
        .map_err(|error| workflow_column_error("parent_reference", &error))?;
    let list_reference: Option<String> = row
        .get(offset + 8)
        .map_err(|error| workflow_column_error("list_reference", &error))?;
    let state: Option<String> = row
        .get(offset + 9)
        .map_err(|error| workflow_column_error("state", &error))?;
    let status: Option<String> = row
        .get(offset + 10)
        .map_err(|error| workflow_column_error("status", &error))?;
    let item_order: Option<i64> = row
        .get(offset + 11)
        .map_err(|error| workflow_column_error("item_order", &error))?;
    let revision: Option<String> = row
        .get(offset + 12)
        .map_err(|error| workflow_column_error("native_revision", &error))?;
    let event_sequence: Option<i64> = row
        .get(offset + 13)
        .map_err(|error| workflow_column_error("event_sequence", &error))?;
    let source_sequence: Option<i64> = row
        .get(offset + 14)
        .map_err(|error| workflow_column_error("source_sequence", &error))?;
    let native_timestamp: Option<i64> = row
        .get(offset + 15)
        .map_err(|error| workflow_column_error("native_timestamp", &error))?;
    let observation_sequence: i64 = row
        .get(offset + 16)
        .map_err(|error| workflow_column_error("observation_sequence", &error))?;
    let ordering_domain: String = row
        .get(offset + 17)
        .map_err(|error| workflow_column_error("ordering_domain", &error))?;
    let content_json: Option<String> = row
        .get(offset + 18)
        .map_err(|error| workflow_column_error("content_json", &error))?;
    let content_text: String = row
        .get(offset + 19)
        .map_err(|error| workflow_column_error("content_text", &error))?;

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
    if let Some(content_json) = content_json {
        let content = serde_json::from_str(&content_json)
            .map_err(|error| workflow_column_error("content_json", &error))?;
        metadata.insert("content".to_owned(), content);
    }

    Ok(SessionMessageRecord {
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
    use tracedecay_runtime_core::db::engine::Value;

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

    async fn query_plan(
        database: &RegisteredGlobalDb,
        sql: &str,
        params: Vec<Value>,
    ) -> Vec<String> {
        let snapshot = database.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        details
    }

    fn assert_scoped_index_plan(details: &[String], index: &str) {
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "query plan did not use {index}: {details:?}"
        );
        assert!(
            !details
                .iter()
                .any(|detail| detail.contains("INTEGER PRIMARY KEY")),
            "query plan regressed to a global rowid scan: {details:?}"
        );
        assert!(
            !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
            "query plan regressed to a sort: {details:?}"
        );
    }

    async fn insert_interleaved_session_messages(database: &RegisteredGlobalDb, rows: i64) {
        let final_value = rows - 1;
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute_batch(&format!(
                "WITH RECURSIVE rows(value) AS (
                    SELECT 0
                    UNION ALL
                    SELECT value + 1 FROM rows WHERE value < {final_value}
                 )
                 INSERT INTO session_messages(
                    provider, message_id, session_id, role, timestamp, ordinal, text,
                    kind, model, tool_names, source_path, source_offset, metadata_json
                 )
                 SELECT
                    'claude',
                    printf('message-%04d', value),
                    CASE WHEN value % 2 = 0 THEN 'target' ELSE 'noise' END,
                    'assistant',
                    1700000000 + ({final_value} - value / 8),
                    CASE WHEN value % 2 = 0 THEN value / 4 ELSE value / 2 END,
                    'payload', NULL, NULL, printf('tool-%04d', {final_value} - value), NULL, NULL, NULL
                 FROM rows;"
            ))
            .await
            .unwrap();
        transaction.commit().await.unwrap();
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
        for frontier in [
            "host-frontier://kimi/discovery/v1",
            "host-frontier://opencode/sql-rowid/v1",
        ] {
            database
                .set_parse_offset(
                    frontier,
                    ParseOffset {
                        byte_offset: 1,
                        mtime: 0,
                        file_id: 1,
                    },
                )
                .await
                .unwrap();
        }
        for (provider, state, deferred_units) in
            [("kimi", 1, 0), ("opencode", 2, 3), ("claude", 3, 1)]
        {
            database
                .set_parse_offset(
                    &format!("host-coverage://{provider}/v1"),
                    ParseOffset {
                        byte_offset: deferred_units,
                        mtime: 1,
                        file_id: state,
                    },
                )
                .await
                .unwrap();
        }

        let runtime_surface = database.cursor_session_ingest_health().await.unwrap();
        let status_surface = database.cursor_session_ingest_health().await.unwrap();
        let all_providers = database
            .session_ingest_health_for_provider(None)
            .await
            .unwrap();

        assert_eq!(runtime_surface, status_surface);
        assert_eq!(runtime_surface.observed_providers, ["cursor"]);
        assert_eq!(runtime_surface.tracked_transcripts, 1);
        assert_eq!(runtime_surface.pending_transcripts, 1);
        assert_eq!(runtime_surface.pending_bytes, 6);
        assert_eq!(runtime_surface.max_transcript_pending_bytes, 6);
        assert_eq!(runtime_surface.last_ingest_unix, Some(100));
        assert_eq!(
            all_providers.observed_providers,
            ["claude", "cursor", "kimi", "opencode"]
        );
        assert_eq!(
            all_providers.provider_coverage,
            [
                SessionProviderCoverage {
                    provider: "claude".into(),
                    state: SessionProviderCoverageState::Unavailable,
                    deferred_units: 1,
                },
                SessionProviderCoverage {
                    provider: "kimi".into(),
                    state: SessionProviderCoverageState::Complete,
                    deferred_units: 0,
                },
                SessionProviderCoverage {
                    provider: "opencode".into(),
                    state: SessionProviderCoverageState::Partial,
                    deferred_units: 3,
                },
            ]
        );
    }

    #[tokio::test]
    async fn interleaved_session_activity_reads_use_covering_index() {
        let profile = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap();
        assert!(
            database
                .upsert_session(&session("claude", "target", "/tmp/target.jsonl"))
                .await
        );
        assert!(
            database
                .upsert_session(&session("claude", "noise", "/tmp/noise.jsonl"))
                .await
        );

        insert_interleaved_session_messages(database, 2_048).await;

        let activities = database
            .session_messages_after("claude", "target", 1_700_000_000, 512)
            .await
            .unwrap();
        assert_eq!(activities.len(), 512);
        assert!(activities.windows(2).all(|window| {
            (window[0].timestamp, window[0].ordinal) <= (window[1].timestamp, window[1].ordinal)
        }));
        for window in activities.windows(2) {
            if (window[0].timestamp, window[0].ordinal) == (window[1].timestamp, window[1].ordinal)
            {
                assert!(
                    window[0].tool_names > window[1].tool_names,
                    "message_id tie-break did not produce a stable order: {window:?}"
                );
            }
        }

        let activity_plan = query_plan(
            database,
            SESSION_MESSAGES_AFTER_SQL,
            vec![
                Value::Text("claude".to_owned()),
                Value::Text("target".to_owned()),
                Value::Integer(1_700_000_000),
                Value::Integer(512),
            ],
        )
        .await;
        assert_scoped_index_plan(&activity_plan, "idx_session_messages_session_activity");
        assert!(
            activity_plan
                .iter()
                .any(|detail| detail.contains("COVERING INDEX")),
            "activity query plan is not covering: {activity_plan:?}"
        );
    }
}
