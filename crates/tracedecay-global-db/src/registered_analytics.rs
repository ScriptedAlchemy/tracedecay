use std::fmt::Write as _;

use crate::db::engine::Value;

use super::{
    AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts,
    AnalyticsToolCounts, RegisteredGlobalDb, analytics_scope_query, row_to_analytics_event,
};

impl RegisteredGlobalDb {
    pub(crate) async fn append_analytics_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> Result<i64, String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin analytics event transaction: {error}"))?;
        let id = append_analytics_event_in_existing_tx(&transaction, event).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit analytics event transaction: {error}"))?;
        Ok(id)
    }

    /// Canonical observability append with replay-safe idempotency.
    ///
    /// The registered writer serializes the lookup and insert; the partial
    /// unique index remains the cross-process backstop. Reusing a key with
    /// changed canonical input is an explicit conflict.
    pub(crate) async fn append_observability_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> Result<i64, String> {
        if event.provider != "tracedecay-observability"
            || event.hint_id.as_deref().is_none_or(str::is_empty)
            || event.metadata_json.is_none()
        {
            return Err("invalid canonical observability event".to_string());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability transaction: {error}"))?;
        let mut rows = transaction
            .query(
                "SELECT id, metadata_json
                 FROM analytics_events
                 WHERE provider = ?1 AND project_id = ?2 AND hint_id = ?3
                 LIMIT 1",
                crate::db::engine::params![
                    event.provider.as_str(),
                    event.project_id.as_str(),
                    event.hint_id.as_deref()
                ],
            )
            .await
            .map_err(|error| format!("failed to read observability idempotency key: {error}"))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to decode observability idempotency row: {error}"))?
        {
            let id = row
                .get::<i64>(0)
                .map_err(|error| format!("failed to decode observability event id: {error}"))?;
            let stored = row.get::<Option<String>>(1).map_err(|error| {
                format!("failed to decode observability canonical input: {error}")
            })?;
            if stored.as_deref() != event.metadata_json.as_deref() {
                return Err("observability idempotency conflict".to_string());
            }
            drop(row);
            drop(rows);
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close observability replay: {error}"))?;
            return Ok(id);
        }
        drop(rows);
        let id = append_analytics_event_in_existing_tx(&transaction, event).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability event: {error}"))?;
        Ok(id)
    }

    pub(crate) async fn append_analytics_events(
        &self,
        events: &[AnalyticsEventInsert],
    ) -> Result<Vec<i64>, String> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin analytics event batch: {error}"))?;
        let mut ids = Vec::with_capacity(events.len());
        for event in events {
            ids.push(append_analytics_event_in_existing_tx(&transaction, event).await?);
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit analytics event batch: {error}"))?;
        Ok(ids)
    }

    /// Atomically appends one imported JSONL frontier and advances its cursor.
    ///
    /// Keeping the cursor in the same transaction prevents a committed event
    /// batch from being replayed when cursor persistence fails.
    pub(crate) async fn append_analytics_events_with_cursor(
        &self,
        events: &[AnalyticsEventInsert],
        cursor_path: &str,
        cursor: super::ParseOffset,
    ) -> Result<Vec<i64>, String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin analytics import transaction: {error}"))?;
        let mut ids = Vec::with_capacity(events.len());
        for event in events {
            ids.push(append_analytics_event_in_existing_tx(&transaction, event).await?);
        }
        super::transcript::set_parse_offset(&transaction, cursor_path, cursor)
            .await
            .map_err(|error| format!("failed to persist analytics import cursor: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit analytics import transaction: {error}"))?;
        Ok(ids)
    }

    pub(crate) async fn query_analytics_events(
        &self,
        query: &AnalyticsEventQuery,
    ) -> Result<Vec<AnalyticsEventRecord>, String> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT id, provider, project_id, session_id, timestamp, event_kind,
                    hook_name, tool_name, tool_category, skill_name, hint_category,
                    hint_id, outcome, metadata_json
             FROM analytics_events",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::new();
        for (column, value) in [
            ("provider", query.provider.as_deref()),
            ("project_id", query.project_id.as_deref()),
            ("session_id", query.session_id.as_deref()),
            ("event_kind", query.event_kind.as_deref()),
        ] {
            super::push_optional_analytics_filter(&mut clauses, &mut values, column, value);
        }
        if let Some(since) = query.since {
            values.push(Value::Integer(since));
            clauses.push(format!("timestamp >= ?{}", values.len()));
        }
        if let Some(until) = query.until {
            values.push(Value::Integer(until));
            clauses.push(format!("timestamp < ?{}", values.len()));
        }
        if let Some(before_id) = query.before_id {
            values.push(Value::Integer(before_id));
            clauses.push(format!("id < ?{}", values.len()));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        values.push(Value::Integer(
            i64::try_from(query.limit).unwrap_or(i64::MAX),
        ));
        let limit_param = values.len();
        if query.provider.as_deref() == Some("tracedecay-observability") {
            let _ = write!(sql, " ORDER BY id DESC LIMIT ?{limit_param}");
        } else {
            let _ = write!(
                sql,
                " ORDER BY timestamp DESC, id DESC LIMIT ?{limit_param}"
            );
        }

        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin analytics event snapshot: {error}"))?;
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query analytics events: {error}"))?;
        let mut events = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read analytics events: {error}"))?
        {
            events.push(
                row_to_analytics_event(&row)
                    .ok_or_else(|| "failed to decode analytics event row".to_string())?,
            );
        }
        events.reverse();
        Ok(events)
    }

    pub(crate) async fn count_analytics_events(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<i64, String> {
        let (sql, values) = analytics_scope_query(
            "SELECT COUNT(*) FROM analytics_events",
            project_id,
            since,
            &[],
        );
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin analytics count snapshot: {error}"))?;
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to count analytics events: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read analytics event count: {error}"))?
        else {
            return Ok(0);
        };
        row.get::<i64>(0)
            .map_err(|error| format!("failed to decode analytics event count: {error}"))
    }

    pub(crate) async fn query_analytics_tool_counts(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<Vec<AnalyticsToolCounts>, String> {
        let (mut sql, values) = analytics_scope_query(
            "SELECT tool_name,
                    COUNT(*) AS calls,
                    SUM(CASE WHEN outcome = 'error' THEN 1 ELSE 0 END) AS errors
             FROM analytics_events",
            project_id,
            since,
            &[
                "event_kind = 'mcp_tool_call'",
                "tool_name IS NOT NULL",
                "tool_name <> ''",
            ],
        );
        sql.push_str(" GROUP BY tool_name ORDER BY calls DESC, tool_name");
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin analytics tool snapshot: {error}"))?;
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query analytics tool counts: {error}"))?;
        let mut counts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read analytics tool counts: {error}"))?
        {
            counts.push(AnalyticsToolCounts {
                tool_name: row
                    .get::<String>(0)
                    .map_err(|error| format!("failed to decode analytics tool name: {error}"))?,
                calls: row
                    .get::<i64>(1)
                    .map_err(|error| format!("failed to decode analytics tool calls: {error}"))?,
                errors: row
                    .get::<i64>(2)
                    .map_err(|error| format!("failed to decode analytics tool errors: {error}"))?,
            });
        }
        Ok(counts)
    }

    pub(crate) async fn query_analytics_hint_counts(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<Vec<AnalyticsHintCounts>, String> {
        let (mut sql, values) = analytics_scope_query(
            "SELECT hint_category,
                    SUM(CASE WHEN event_kind IN ('hint_emitted', 'hint_escalated', 'missing_session') THEN 1 ELSE 0 END) AS emitted,
                    SUM(CASE WHEN event_kind = 'hint_outcome' AND LOWER(TRIM(COALESCE(outcome, ''))) = 'acted' THEN 1 ELSE 0 END) AS followed,
                    SUM(CASE WHEN event_kind = 'hint_outcome' AND LOWER(TRIM(COALESCE(outcome, ''))) = 'ignored' THEN 1 ELSE 0 END) AS ignored,
                    SUM(CASE WHEN event_kind LIKE 'suppressed_%' THEN 1 ELSE 0 END) AS suppressed
             FROM analytics_events",
            project_id,
            since,
            &[
                "hint_category IS NOT NULL",
                "hint_category <> ''",
                "(event_kind IN ('hint_emitted', 'hint_escalated', 'missing_session', 'hint_outcome') OR event_kind LIKE 'suppressed_%')",
            ],
        );
        sql.push_str(" GROUP BY hint_category ORDER BY hint_category");
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin analytics hint snapshot: {error}"))?;
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query analytics hint counts: {error}"))?;
        let mut counts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read analytics hint counts: {error}"))?
        {
            counts.push(AnalyticsHintCounts {
                category: row.get::<String>(0).map_err(|error| {
                    format!("failed to decode analytics hint category: {error}")
                })?,
                emitted: row.get::<i64>(1).map_err(|error| {
                    format!("failed to decode analytics emitted count: {error}")
                })?,
                followed: row.get::<i64>(2).map_err(|error| {
                    format!("failed to decode analytics followed count: {error}")
                })?,
                ignored: row.get::<i64>(3).map_err(|error| {
                    format!("failed to decode analytics ignored count: {error}")
                })?,
                suppressed: row.get::<i64>(4).map_err(|error| {
                    format!("failed to decode analytics suppressed count: {error}")
                })?,
            });
        }
        Ok(counts)
    }
}

async fn append_analytics_event_in_existing_tx(
    transaction: &super::RegisteredGlobalDbWriteTransaction<'_>,
    event: &AnalyticsEventInsert,
) -> Result<i64, String> {
    let mut rows = transaction
        .query(
            "INSERT INTO analytics_events
                 (provider, project_id, session_id, timestamp, event_kind, hook_name,
                  tool_name, tool_category, skill_name, hint_category, hint_id, outcome,
                  metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 RETURNING id",
            crate::db::engine::params![
                event.provider.as_str(),
                event.project_id.as_str(),
                event.session_id.as_deref(),
                event.timestamp,
                event.event_kind.as_str(),
                event.hook_name.as_deref(),
                event.tool_name.as_deref(),
                event.tool_category.as_deref(),
                event.skill_name.as_deref(),
                event.hint_category.as_deref(),
                event.hint_id.as_deref(),
                event.outcome.as_deref(),
                event.metadata_json.as_deref(),
            ],
        )
        .await
        .map_err(|error| format!("failed to append analytics event: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("failed to read appended analytics event id: {error}"))?
        .ok_or_else(|| "append analytics event returned no id".to_string())?;
    row.get::<i64>(0)
        .map_err(|error| format!("failed to decode appended analytics event id: {error}"))
}
