use std::fmt::Write as _;

use tracedecay_runtime_core::db::engine::Value;

use super::{
    AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts,
    AnalyticsToolCounts, ObservabilityEmissionClaimV1, ObservabilityEmissionOutboxRecordV1,
    RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, analytics_scope_query,
    row_to_analytics_event,
};

const OBSERVABILITY_DETAIL_RETENTION_SECONDS: i64 = 30 * 86_400;
const OBSERVABILITY_ROLLUP_RETENTION_SECONDS: i64 = 395 * 86_400;
const MAX_OBSERVABILITY_OUTBOX_JSON_BYTES: usize = 1_048_576;
const OBSERVABILITY_RETENTION_ROWS_PER_CLASS: usize = 512;
const ACTIVE_DIRTY_ROLLUP_SOURCE_EXCLUSION_SQL: &str = r#"
NOT (
    analytics_events.event_kind IN (
        'work.execution_topology.sampled.v1',
        'work.conflict_prediction.observed.v1',
        'work.conflict_outcome.linked.v1',
        'work.integration.transition.observed.v1',
        'work.github_stack_capability.observed.v1',
        'work.duplicate_effort.observed.v1',
        'work.blocked_interval.observed.v1',
        'work.rerun.observed.v1',
        'work.execution_leak.observed.v1',
        'work.delivery_fanout.observed.v1',
        'telemetry.drop.observed.v1'
    )
    AND EXISTS (
        SELECT 1
        FROM observability_rollup_dirty_days AS dirty
        WHERE dirty.scope_ref = analytics_events.project_id
          AND dirty.day_start_seconds = (analytics_events.timestamp / 86400) * 86400
    )
)"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityRetentionReceiptV1 {
    pub expired_detail: u64,
    pub expired_rollup: u64,
    pub expired_settled_outbox: u64,
    /// A bounded page remains and maintenance should use its retry cadence.
    pub has_more: bool,
}

impl RegisteredGlobalDb {
    /// Read an existing exact owner claim without allocating a new delivery.
    pub async fn observability_emission_claim(
        &self,
        project_id: &str,
        owner_event_id: &str,
        owner_fact_json: &str,
    ) -> Result<Option<ObservabilityEmissionClaimV1>, String> {
        if project_id.is_empty()
            || owner_event_id.is_empty()
            || project_id.len() > 256
            || owner_event_id.len() > 256
            || owner_fact_json.len() > MAX_OBSERVABILITY_OUTBOX_JSON_BYTES
            || serde_json::from_str::<serde_json::Value>(owner_fact_json).is_err()
        {
            return Err("invalid observability outbox lookup".to_owned());
        }
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin observability outbox lookup: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT owner_fact_json, delivery_envelope_json, analytics_event_id
                 FROM observability_emission_outbox
                 WHERE project_id = ?1 AND owner_event_id = ?2
                 LIMIT 1",
                tracedecay_runtime_core::db::engine::params![project_id, owner_event_id],
            )
            .await
            .map_err(|error| format!("failed to query observability outbox lookup: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read observability outbox lookup: {error}"))?
        else {
            return Ok(None);
        };
        let stored_owner: String = row
            .get(0)
            .map_err(|error| format!("failed to decode observability owner fact: {error}"))?;
        if stored_owner != owner_fact_json {
            return Err("observability owner fact conflict".to_owned());
        }
        let delivery_envelope_json = row
            .get(1)
            .map_err(|error| format!("failed to decode observability delivery: {error}"))?;
        let analytics_event_id: Option<i64> = row
            .get(2)
            .map_err(|error| format!("failed to decode observability receipt: {error}"))?;
        Ok(Some(match analytics_event_id {
            Some(analytics_event_id) => ObservabilityEmissionClaimV1::Settled {
                delivery_envelope_json,
                analytics_event_id,
            },
            None => ObservabilityEmissionClaimV1::Pending {
                delivery_envelope_json,
            },
        }))
    }

    pub async fn append_analytics_event(
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
    pub async fn append_observability_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> Result<i64, String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability transaction: {error}"))?;
        let id = append_observability_event_in_existing_tx(&transaction, event).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability event: {error}"))?;
        Ok(id)
    }

    /// Claim one stable owner fact without replacing a prior delivery.
    pub async fn claim_observability_emission(
        &self,
        project_id: &str,
        owner_event_id: &str,
        owner_fact_json: &str,
        delivery_envelope_json: &str,
    ) -> Result<ObservabilityEmissionClaimV1, String> {
        validate_outbox_input(
            project_id,
            owner_event_id,
            owner_fact_json,
            delivery_envelope_json,
        )?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability outbox claim: {error}"))?;
        if let Some(stored) = read_outbox_record(&transaction, project_id, owner_event_id).await? {
            if stored.owner_fact_json != owner_fact_json {
                return Err("observability owner fact conflict".to_owned());
            }
            let outcome = if let Some(analytics_event_id) = stored.analytics_event_id {
                ObservabilityEmissionClaimV1::Settled {
                    delivery_envelope_json: stored.delivery_envelope_json,
                    analytics_event_id,
                }
            } else {
                ObservabilityEmissionClaimV1::Pending {
                    delivery_envelope_json: stored.delivery_envelope_json,
                }
            };
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close observability outbox replay: {error}"))?;
            return Ok(outcome);
        }
        transaction
            .execute(
                "INSERT INTO observability_emission_outbox
                     (project_id, owner_event_id, owner_fact_json,
                      delivery_envelope_json, state, analytics_event_id)
                 VALUES (?1, ?2, ?3, ?4, 'pending', NULL)",
                tracedecay_runtime_core::db::engine::params![
                    project_id,
                    owner_event_id,
                    owner_fact_json,
                    delivery_envelope_json
                ],
            )
            .await
            .map_err(|error| format!("failed to claim observability outbox event: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability outbox claim: {error}"))?;
        Ok(ObservabilityEmissionClaimV1::Claimed {
            delivery_envelope_json: delivery_envelope_json.to_owned(),
        })
    }

    /// CAS a pending delivery to its delayed-coverage representation.
    pub async fn delay_observability_emission(
        &self,
        project_id: &str,
        owner_event_id: &str,
        owner_fact_json: &str,
        expected_delivery_envelope_json: &str,
        delayed_delivery_envelope_json: &str,
    ) -> Result<ObservabilityEmissionClaimV1, String> {
        validate_outbox_input(
            project_id,
            owner_event_id,
            owner_fact_json,
            delayed_delivery_envelope_json,
        )?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability outbox delay: {error}"))?;
        let stored = read_outbox_record(&transaction, project_id, owner_event_id)
            .await?
            .ok_or_else(|| "observability outbox event is unavailable".to_owned())?;
        if stored.owner_fact_json != owner_fact_json {
            return Err("observability owner fact conflict".to_owned());
        }
        if let Some(analytics_event_id) = stored.analytics_event_id {
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close settled outbox delay: {error}"))?;
            return Ok(ObservabilityEmissionClaimV1::Settled {
                delivery_envelope_json: stored.delivery_envelope_json,
                analytics_event_id,
            });
        }
        if stored.delivery_envelope_json != expected_delivery_envelope_json {
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close replayed outbox delay: {error}"))?;
            return Ok(ObservabilityEmissionClaimV1::Pending {
                delivery_envelope_json: stored.delivery_envelope_json,
            });
        }
        transaction
            .execute(
                "UPDATE observability_emission_outbox
                 SET delivery_envelope_json = ?4
                 WHERE project_id = ?1 AND owner_event_id = ?2
                   AND owner_fact_json = ?3 AND state = 'pending'
                   AND delivery_envelope_json = ?5",
                tracedecay_runtime_core::db::engine::params![
                    project_id,
                    owner_event_id,
                    owner_fact_json,
                    delayed_delivery_envelope_json,
                    expected_delivery_envelope_json
                ],
            )
            .await
            .map_err(|error| format!("failed to mark observability delivery delayed: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability outbox delay: {error}"))?;
        Ok(ObservabilityEmissionClaimV1::Pending {
            delivery_envelope_json: delayed_delivery_envelope_json.to_owned(),
        })
    }

    /// Append the exact delivery and settle its outbox row in one transaction.
    pub async fn settle_observability_emission(
        &self,
        project_id: &str,
        owner_event_id: &str,
        owner_fact_json: &str,
        delivery_envelope_json: &str,
        event: &AnalyticsEventInsert,
    ) -> Result<i64, String> {
        validate_outbox_input(
            project_id,
            owner_event_id,
            owner_fact_json,
            delivery_envelope_json,
        )?;
        if event.project_id != project_id
            || event.hint_id.as_deref() != Some(owner_event_id)
            || event.metadata_json.as_deref() != Some(delivery_envelope_json)
        {
            return Err("observability outbox delivery binding conflict".to_owned());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability outbox settlement: {error}"))?;
        let stored = read_outbox_record(&transaction, project_id, owner_event_id)
            .await?
            .ok_or_else(|| "observability outbox event is unavailable".to_owned())?;
        if stored.owner_fact_json != owner_fact_json
            || stored.delivery_envelope_json != delivery_envelope_json
        {
            return Err("observability outbox settlement conflict".to_owned());
        }
        let id = append_observability_event_in_existing_tx(&transaction, event).await?;
        if let Some(settled_id) = stored.analytics_event_id {
            if settled_id != id {
                return Err("observability outbox receipt conflict".to_owned());
            }
        } else {
            transaction
                .execute(
                    "UPDATE observability_emission_outbox
                     SET state = 'settled', analytics_event_id = ?5
                     WHERE project_id = ?1 AND owner_event_id = ?2
                       AND owner_fact_json = ?3 AND delivery_envelope_json = ?4
                       AND state = 'pending'",
                    tracedecay_runtime_core::db::engine::params![
                        project_id,
                        owner_event_id,
                        owner_fact_json,
                        delivery_envelope_json,
                        id
                    ],
                )
                .await
                .map_err(|error| format!("failed to settle observability outbox event: {error}"))?;
        }
        transaction.commit().await.map_err(|error| {
            format!("failed to commit observability outbox settlement: {error}")
        })?;
        Ok(id)
    }

    /// Reads only producer-stamped [`tracedecay_domain::ObservabilityEnvelopeV1`]
    /// carriers. This table is not a generic delivery outbox: recovery decodes
    /// every pending row through that exact envelope before settlement.
    pub async fn pending_observability_emissions(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<ObservabilityEmissionOutboxRecordV1>, String> {
        if project_id.is_empty() || limit == 0 || limit > 1_024 {
            return Err("invalid observability outbox query".to_owned());
        }
        let limit = i64::try_from(limit)
            .map_err(|_| "observability outbox query bound is invalid".to_owned())?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin observability outbox snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT project_id, owner_event_id, owner_fact_json, delivery_envelope_json
                 FROM observability_emission_outbox
                 WHERE project_id = ?1 AND state = 'pending'
                 ORDER BY json_extract(delivery_envelope_json, '$.producer_sequence'),
                          owner_event_id
                 LIMIT ?2",
                tracedecay_runtime_core::db::engine::params![project_id, limit],
            )
            .await
            .map_err(|error| format!("failed to query observability outbox: {error}"))?;
        let mut pending = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read observability outbox: {error}"))?
        {
            pending.push(ObservabilityEmissionOutboxRecordV1 {
                project_id: row.get(0).map_err(|error| {
                    format!("failed to decode observability outbox project: {error}")
                })?,
                owner_event_id: row.get(1).map_err(|error| {
                    format!("failed to decode observability outbox owner event: {error}")
                })?,
                owner_fact_json: row.get(2).map_err(|error| {
                    format!("failed to decode observability outbox owner fact: {error}")
                })?,
                delivery_envelope_json: row.get(3).map_err(|error| {
                    format!("failed to decode observability outbox delivery: {error}")
                })?,
            });
        }
        Ok(pending)
    }

    /// Expires only Plan 26 optional detail and rollup rows through the
    /// registered writer. Product receipts retain their owning lifecycle.
    pub async fn prune_observability_events(
        &self,
        now_seconds: i64,
    ) -> Result<ObservabilityRetentionReceiptV1, String> {
        if now_seconds < 0 {
            return Err("invalid observability retention time".to_owned());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability retention: {error}"))?;
        let (expired_detail_outbox, expired_detail) = prune_observability_retention_class(
            &transaction,
            now_seconds.saturating_sub(OBSERVABILITY_DETAIL_RETENTION_SECONDS),
            "optional_local_detail30d",
        )
        .await?;
        let (expired_rollup_outbox, expired_rollup) = prune_observability_retention_class(
            &transaction,
            now_seconds.saturating_sub(OBSERVABILITY_ROLLUP_RETENTION_SECONDS),
            "local_rollup395d",
        )
        .await?;
        let has_more = observability_retention_has_more(
            &transaction,
            now_seconds.saturating_sub(OBSERVABILITY_DETAIL_RETENTION_SECONDS),
            now_seconds.saturating_sub(OBSERVABILITY_ROLLUP_RETENTION_SECONDS),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability retention: {error}"))?;
        Ok(ObservabilityRetentionReceiptV1 {
            expired_detail,
            expired_rollup,
            expired_settled_outbox: expired_detail_outbox.saturating_add(expired_rollup_outbox),
            has_more,
        })
    }

    pub async fn append_analytics_events(
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
    /// batch from being replayed when cursor persistence fails. `expected_cursor`
    /// is the durable cursor the caller read before parsing: the append is
    /// refused when another importer has already advanced it, so two concurrent
    /// importers can never both claim the same byte range.
    pub async fn append_analytics_events_with_cursor(
        &self,
        events: &[AnalyticsEventInsert],
        cursor_path: &str,
        expected_cursor: super::ParseOffset,
        cursor: super::ParseOffset,
    ) -> Result<Vec<i64>, String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin analytics import transaction: {error}"))?;
        super::transcript::require_expected_offset(&transaction, cursor_path, expected_cursor)
            .await
            .map_err(|error| format!("failed to claim analytics import cursor: {error}"))?;
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

    pub async fn query_analytics_events(
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

    pub async fn count_analytics_events(
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

    pub async fn query_analytics_tool_counts(
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

    pub async fn query_analytics_hint_counts(
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
            tracedecay_runtime_core::db::engine::params![
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

struct StoredObservabilityOutboxRecord {
    owner_fact_json: String,
    delivery_envelope_json: String,
    analytics_event_id: Option<i64>,
}

fn validate_outbox_input(
    project_id: &str,
    owner_event_id: &str,
    owner_fact_json: &str,
    delivery_envelope_json: &str,
) -> Result<(), String> {
    if project_id.is_empty()
        || owner_event_id.is_empty()
        || project_id.len() > 256
        || owner_event_id.len() > 256
        || owner_fact_json.len() > MAX_OBSERVABILITY_OUTBOX_JSON_BYTES
        || delivery_envelope_json.len() > MAX_OBSERVABILITY_OUTBOX_JSON_BYTES
        || serde_json::from_str::<serde_json::Value>(owner_fact_json).is_err()
        || serde_json::from_str::<serde_json::Value>(delivery_envelope_json).is_err()
    {
        return Err("invalid observability outbox input".to_owned());
    }
    Ok(())
}

async fn read_outbox_record(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    owner_event_id: &str,
) -> Result<Option<StoredObservabilityOutboxRecord>, String> {
    let mut rows = transaction
        .query(
            "SELECT owner_fact_json, delivery_envelope_json, analytics_event_id
             FROM observability_emission_outbox
             WHERE project_id = ?1 AND owner_event_id = ?2
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![project_id, owner_event_id],
        )
        .await
        .map_err(|error| format!("failed to read observability outbox event: {error}"))?;
    let stored = match rows
        .next()
        .await
        .map_err(|error| format!("failed to decode observability outbox event: {error}"))?
    {
        Some(row) => Some(StoredObservabilityOutboxRecord {
            owner_fact_json: row.get(0).map_err(|error| {
                format!("failed to decode observability outbox owner fact: {error}")
            })?,
            delivery_envelope_json: row.get(1).map_err(|error| {
                format!("failed to decode observability outbox delivery: {error}")
            })?,
            analytics_event_id: row.get(2).map_err(|error| {
                format!("failed to decode observability outbox receipt: {error}")
            })?,
        }),
        None => None,
    };
    Ok(stored)
}

async fn append_observability_event_in_existing_tx(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    event: &AnalyticsEventInsert,
) -> Result<i64, String> {
    if event.provider != "tracedecay-observability"
        || event.hint_id.as_deref().is_none_or(str::is_empty)
        || event.metadata_json.is_none()
    {
        return Err("invalid canonical observability event".to_string());
    }
    let mut rows = transaction
        .query(
            "SELECT id, provider, project_id, session_id, timestamp, event_kind,
                    hook_name, tool_name, tool_category, skill_name, hint_category,
                    hint_id, outcome, metadata_json
             FROM analytics_events
             WHERE provider = ?1 AND project_id = ?2 AND hint_id = ?3
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
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
        let stored = row_to_analytics_event(&row)
            .ok_or_else(|| "failed to decode observability canonical input".to_owned())?;
        if !analytics_record_matches_insert(&stored, event) {
            return Err("observability idempotency conflict".to_string());
        }
        return Ok(stored.id);
    }
    drop(rows);
    append_analytics_event_in_existing_tx(transaction, event).await
}

fn analytics_record_matches_insert(
    stored: &AnalyticsEventRecord,
    event: &AnalyticsEventInsert,
) -> bool {
    stored.provider == event.provider
        && stored.project_id == event.project_id
        && stored.session_id == event.session_id
        && stored.timestamp == event.timestamp
        && stored.event_kind == event.event_kind
        && stored.hook_name == event.hook_name
        && stored.tool_name == event.tool_name
        && stored.tool_category == event.tool_category
        && stored.skill_name == event.skill_name
        && stored.hint_category == event.hint_category
        && stored.hint_id == event.hint_id
        && stored.outcome == event.outcome
        && stored.metadata_json == event.metadata_json
}

async fn prune_observability_retention_class(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    cutoff_seconds: i64,
    retention_class: &str,
) -> Result<(u64, u64), String> {
    let limit = i64::try_from(OBSERVABILITY_RETENTION_ROWS_PER_CLASS)
        .map_err(|_| "observability retention page bound is invalid".to_owned())?;
    // Settled outbox entries are replay transport state, not permanent product
    // evidence. Remove them before their exact analytics row so foreign-key
    // enforcement never requires a fail-open cascade. Pending claims have no
    // analytics id and cannot match this deletion.
    let eligible_ids = format!(
        "SELECT id FROM analytics_events
         WHERE provider = 'tracedecay-observability'
           AND timestamp < ?1
           AND json_extract(metadata_json, '$.retention_class') = ?2
           AND {ACTIVE_DIRTY_ROLLUP_SOURCE_EXCLUSION_SQL}
         ORDER BY id
         LIMIT ?3"
    );
    let expired_outbox = transaction
        .execute(
            &format!(
                "DELETE FROM observability_emission_outbox
                 WHERE state = 'settled' AND analytics_event_id IN ({eligible_ids})"
            ),
            tracedecay_runtime_core::db::engine::params![cutoff_seconds, retention_class, limit],
        )
        .await
        .map_err(|error| format!("failed to expire settled observability outbox: {error}"))?;
    let expired_events = transaction
        .execute(
            &format!("DELETE FROM analytics_events WHERE id IN ({eligible_ids})"),
            tracedecay_runtime_core::db::engine::params![cutoff_seconds, retention_class, limit],
        )
        .await
        .map_err(|error| format!("failed to expire observability analytics page: {error}"))?;
    Ok((expired_outbox, expired_events))
}

async fn observability_retention_has_more(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    detail_cutoff_seconds: i64,
    rollup_cutoff_seconds: i64,
) -> Result<bool, String> {
    let query = format!(
        "SELECT EXISTS(
             SELECT 1 FROM analytics_events
             WHERE provider = 'tracedecay-observability'
               AND (
                   (timestamp < ?1 AND json_extract(metadata_json, '$.retention_class')
                       = 'optional_local_detail30d')
                   OR
                   (timestamp < ?2 AND json_extract(metadata_json, '$.retention_class')
                       = 'local_rollup395d')
               )
               AND {ACTIVE_DIRTY_ROLLUP_SOURCE_EXCLUSION_SQL}
             LIMIT 1
         )"
    );
    let mut rows = transaction
        .query(
            &query,
            tracedecay_runtime_core::db::engine::params![
                detail_cutoff_seconds,
                rollup_cutoff_seconds
            ],
        )
        .await
        .map_err(|error| {
            format!("failed to query remaining observability retention work: {error}")
        })?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("failed to read remaining observability retention work: {error}"))?
        .ok_or_else(|| "observability retention work query returned no row".to_owned())?;
    row.get::<i64>(0)
        .map(|remaining| remaining != 0)
        .map_err(|error| {
            format!("failed to decode remaining observability retention work: {error}")
        })
}
