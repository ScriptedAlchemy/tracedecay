use tracedecay_domain::{
    CanonicalObservationIdV1, CanonicalUnknownStateV1, ObservationId, ObservationOrderingDomainV1,
    ObservationScopeV1, ObservationSourceRangeV1, ProjectId, ProviderId,
    ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1, ProviderUsageCursorV1,
    ProviderUsageModelV1, ProviderUsageObservationV1, ProviderUsageReadV1, ProviderUsageScopeV1,
    SessionId,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{PROVIDER_USAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION};

use super::RegisteredGlobalDb;

const MAX_PROVIDER_USAGE_READ: usize = 1_000;

impl RegisteredGlobalDb {
    /// Reads immutable provider usage observations without consulting
    /// conversational rows or the Claude-only accounting import.
    pub async fn provider_usage_observations(
        &self,
        scope: &ObservationScopeV1,
        provider: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<ProviderUsageReadV1, String> {
        self.provider_usage_observations_after(scope, provider, session_id, None, limit)
            .await
    }

    /// Reads the next immutable page after an exact observation/usage ordinal.
    pub async fn provider_usage_observations_after(
        &self,
        scope: &ObservationScopeV1,
        provider: Option<&str>,
        session_id: Option<&str>,
        cursor: Option<&ProviderUsageCursorV1>,
        limit: usize,
    ) -> Result<ProviderUsageReadV1, String> {
        if limit == 0 || limit > MAX_PROVIDER_USAGE_READ {
            return Err(format!(
                "provider usage limit must be between 1 and {MAX_PROVIDER_USAGE_READ}"
            ));
        }
        scope
            .validate()
            .map_err(|error| format!("invalid provider usage read scope: {error}"))?;
        if cursor.is_some_and(|cursor| {
            &cursor.scope != scope
                || cursor.provider.as_deref() != provider
                || cursor.session_id.as_deref() != session_id
        }) {
            return Err(
                "provider usage cursor scope does not match the requested scope".to_owned(),
            );
        }
        let (after_sequence, after_ordinal) = match cursor {
            Some(cursor) => (
                Some(
                    i64::try_from(cursor.observation_sequence)
                        .map_err(|_| "provider usage cursor exceeds integer range".to_owned())?,
                ),
                Some(i64::from(cursor.usage_ordinal)),
            ),
            None => (None, None),
        };
        let reader = self.read_connection();
        let mut watermark_rows = reader
            .query(
                "SELECT last_sequence
                 FROM observation_projection_checkpoints
                 WHERE projector_version = ?1",
                (SESSION_MESSAGE_PROJECTOR_VERSION,),
            )
            .await
            .map_err(|error| format!("failed to read provider usage upper watermark: {error}"))?;
        let current_upper_observation_sequence = watermark_rows
            .next()
            .await
            .map_err(|error| format!("failed to read provider usage upper watermark: {error}"))?
            .map(|row| {
                row.get::<i64>(0)
                    .map_err(|error| {
                        format!("failed to decode provider usage upper watermark: {error}")
                    })
                    .and_then(|value| {
                        u64::try_from(value)
                            .map_err(|_| "provider usage upper watermark is negative".to_owned())
                    })
            })
            .transpose()?;
        let Some(current_upper_observation_sequence) = current_upper_observation_sequence else {
            return Ok(ProviderUsageReadV1::Unknown {
                reason: CanonicalUnknownStateV1::Absent,
                upper_observation_sequence: 0,
            });
        };
        let upper_observation_sequence = cursor
            .map_or(current_upper_observation_sequence, |cursor| {
                cursor.upper_observation_sequence
            });
        if upper_observation_sequence > current_upper_observation_sequence {
            return Err("provider usage cursor upper watermark is not published".to_owned());
        }
        if cursor
            .is_some_and(|cursor| cursor.observation_sequence > cursor.upper_observation_sequence)
        {
            return Err("provider usage cursor is beyond its upper watermark".to_owned());
        }
        let upper_sequence_i64 = i64::try_from(upper_observation_sequence)
            .map_err(|_| "provider usage upper watermark exceeds integer range".to_owned())?;
        let (scope_kind, project_id) = persisted_scope(scope);
        let mut rows = reader
            .query(
                "SELECT observation_id, usage_ordinal, receipt_id, observation_sequence,
                        scope_kind, project_id, provider, model_json, native_scope,
                        counter_semantics, counters_json, session_id, turn_id, message_id,
                        request_id, native_kind, native_field, ordering_domain, source_start,
                        source_end, native_timestamp
                 FROM observation_provider_usage
                 WHERE projector_version = ?1
                   AND scope_kind = ?2
                   AND project_id IS ?3
                   AND (?4 IS NULL OR provider = ?4)
                   AND (?5 IS NULL OR session_id = ?5)
                   AND (
                       ?6 IS NULL
                       OR observation_sequence > ?6
                       OR (observation_sequence = ?6 AND usage_ordinal > ?7)
                   )
                   AND observation_sequence <= ?8
                 ORDER BY observation_sequence, usage_ordinal
                 LIMIT ?9",
                params![
                    PROVIDER_USAGE_PROJECTOR_VERSION,
                    scope_kind,
                    project_id,
                    provider,
                    session_id,
                    after_sequence,
                    after_ordinal,
                    upper_sequence_i64,
                    i64::try_from(limit.saturating_add(1))
                        .map_err(|_| "provider usage limit exceeds integer range".to_owned())?,
                ],
            )
            .await
            .map_err(|error| format!("failed to query provider usage observations: {error}"))?;
        let mut observations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read provider usage observation: {error}"))?
        {
            let observation_id = CanonicalObservationIdV1::new(
                row.get::<String>(0)
                    .map_err(|error| format!("failed to decode provider usage id: {error}"))?,
            )
            .map_err(|error| format!("invalid provider usage observation id: {error}"))?;
            let usage_ordinal =
                u32::try_from(row.get::<i64>(1).map_err(|error| {
                    format!("failed to decode provider usage ordinal: {error}")
                })?)
                .map_err(|_| "provider usage ordinal exceeds integer range".to_owned())?;
            let receipt_id = row
                .get::<String>(2)
                .map_err(|error| format!("failed to decode provider usage receipt: {error}"))?;
            let observation_sequence = u64::try_from(row.get::<i64>(3).map_err(|error| {
                format!("failed to decode provider usage observation sequence: {error}")
            })?)
            .map_err(|_| "provider usage observation sequence is negative".to_owned())?;
            let scope = observation_scope(&row, 4, 5)?;
            let provider =
                ProviderId::new(row.get::<String>(6).map_err(|error| {
                    format!("failed to decode provider usage provider: {error}")
                })?)
                .map_err(|error| format!("invalid provider usage provider: {error}"))?;
            let model = serde_json::from_str::<ProviderUsageModelV1>(
                &row.get::<String>(7)
                    .map_err(|error| format!("failed to decode provider usage model: {error}"))?,
            )
            .map_err(|error| format!("invalid provider usage model: {error}"))?;
            let native_scope_raw = row
                .get::<String>(8)
                .map_err(|error| format!("failed to decode provider usage scope: {error}"))?;
            let native_scope = ProviderUsageScopeV1::from_durable_str(&native_scope_raw)
                .ok_or_else(|| format!("invalid provider usage scope: {native_scope_raw}"))?;
            let semantics_raw = row
                .get::<String>(9)
                .map_err(|error| format!("failed to decode provider usage semantics: {error}"))?;
            let counter_semantics = ProviderUsageCounterSemanticsV1::from_durable_str(
                &semantics_raw,
            )
            .ok_or_else(|| format!("invalid provider usage counter semantics: {semantics_raw}"))?;
            let counters =
                serde_json::from_str::<ProviderUsageCountersV1>(&row.get::<String>(10).map_err(
                    |error| format!("failed to decode provider usage counters: {error}"),
                )?)
                .map_err(|error| format!("invalid provider usage counters: {error}"))?;
            let session_id =
                SessionId::new(row.get::<String>(11).map_err(|error| {
                    format!("failed to decode provider usage session: {error}")
                })?)
                .map_err(|error| format!("invalid provider usage session: {error}"))?;
            let turn_id = optional_observation_id(&row, 12, "turn")?;
            let message_id = optional_observation_id(&row, 13, "message")?;
            let request_id = optional_observation_id(&row, 14, "request")?;
            let native_kind = row
                .get::<String>(15)
                .map_err(|error| format!("failed to decode provider usage native kind: {error}"))?;
            let native_field = row.get::<String>(16).map_err(|error| {
                format!("failed to decode provider usage native field: {error}")
            })?;
            let ordering_raw = row.get::<String>(17).map_err(|error| {
                format!("failed to decode provider usage ordering domain: {error}")
            })?;
            let ordering_domain = ordering_domain(&ordering_raw)?;
            let source_start = u64::try_from(
                row.get::<i64>(18)
                    .map_err(|error| format!("failed to decode provider usage start: {error}"))?,
            )
            .map_err(|_| "provider usage source start is negative".to_owned())?;
            let source_end = u64::try_from(
                row.get::<i64>(19)
                    .map_err(|error| format!("failed to decode provider usage end: {error}"))?,
            )
            .map_err(|_| "provider usage source end is negative".to_owned())?;
            let source_range = ObservationSourceRangeV1::new(source_start, source_end)
                .map_err(|error| format!("invalid provider usage source range: {error}"))?;
            let native_timestamp = row.get::<Option<i64>>(20).map_err(|error| {
                format!("failed to decode provider usage native timestamp: {error}")
            })?;
            observations.push(ProviderUsageObservationV1 {
                observation_id,
                usage_ordinal,
                receipt_id,
                observation_sequence,
                scope,
                provider,
                model,
                native_scope,
                counter_semantics,
                counters,
                session_id,
                turn_id,
                message_id,
                request_id,
                native_kind,
                native_field,
                ordering_domain,
                source_range,
                native_timestamp,
            });
        }
        let next_cursor = if observations.len() > limit {
            observations.truncate(limit);
            observations
                .last()
                .map(|observation| ProviderUsageCursorV1 {
                    observation_sequence: observation.observation_sequence,
                    usage_ordinal: observation.usage_ordinal,
                    upper_observation_sequence,
                    scope: scope.clone(),
                    provider: provider.map(str::to_owned),
                    session_id: session_id.map(str::to_owned),
                })
        } else {
            None
        };
        Ok(ProviderUsageReadV1::Known {
            observations,
            upper_observation_sequence,
            next_cursor,
        })
    }
}

fn persisted_scope(scope: &ObservationScopeV1) -> (&'static str, Option<&str>) {
    match scope {
        ObservationScopeV1::Profile => ("profile", None),
        ObservationScopeV1::Project { project_id } => ("project", Some(project_id.as_str())),
    }
}

fn observation_scope(
    row: &tracedecay_runtime_core::db::engine::Row,
    kind_column: i32,
    project_column: i32,
) -> Result<ObservationScopeV1, String> {
    let kind = row
        .get::<String>(kind_column)
        .map_err(|error| format!("failed to decode provider usage scope kind: {error}"))?;
    let project_id = row
        .get::<Option<String>>(project_column)
        .map_err(|error| format!("failed to decode provider usage project id: {error}"))?;
    match (kind.as_str(), project_id) {
        ("profile", None) => Ok(ObservationScopeV1::Profile),
        ("project", Some(project_id)) => ProjectId::new(project_id)
            .map(|project_id| ObservationScopeV1::Project { project_id })
            .map_err(|error| format!("invalid provider usage project id: {error}")),
        _ => Err("invalid provider usage persisted scope".to_owned()),
    }
}

fn optional_observation_id(
    row: &tracedecay_runtime_core::db::engine::Row,
    column: i32,
    field: &str,
) -> Result<Option<ObservationId>, String> {
    row.get::<Option<String>>(column)
        .map_err(|error| format!("failed to decode provider usage {field} id: {error}"))?
        .map(ObservationId::new)
        .transpose()
        .map_err(|error| format!("invalid provider usage {field} id: {error}"))
}

fn ordering_domain(value: &str) -> Result<ObservationOrderingDomainV1, String> {
    match value {
        "file_bytes" => Ok(ObservationOrderingDomainV1::FileBytes),
        "sqlite_row_id" => Ok(ObservationOrderingDomainV1::SqliteRowId),
        "snapshot_order" => Ok(ObservationOrderingDomainV1::SnapshotOrder),
        "daemon_sequence" => Ok(ObservationOrderingDomainV1::DaemonSequence),
        _ => Err(format!("invalid provider usage ordering domain: {value}")),
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
        CanonicalObservationRelationsV1, ComponentVersion, DurableObservationV1,
        ObservationIdentityMaterialV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, PayloadReferenceV1,
        ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1, ProviderUsageModelV1,
        ProviderUsageReadV1, ProviderUsageScopeV1, RetentionClass, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    };
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};
    use tracedecay_store::ProjectionStoreError;

    use super::*;
    use crate::observation_projection::apply_provider_usage_effects;
    use crate::tests::harness::RegisteredGlobalDbHarness;
    use crate::{ensure_registered_schema, rebuild_projection_with_engine};

    fn fixture(
        index: u64,
        fact: CanonicalObservationFactV1,
    ) -> (DurableObservationV1, ObservationSourceCursorV1) {
        fixture_in_scope(index, fact, ObservationScopeV1::Profile)
    }

    fn fixture_in_scope(
        index: u64,
        fact: CanonicalObservationFactV1,
        scope: ObservationScopeV1,
    ) -> (DurableObservationV1, ObservationSourceCursorV1) {
        let provider = ProviderId::new("codex").unwrap();
        let session_id = SessionId::new("session.provider-usage").unwrap();
        let record_id = ObservationId::new(format!("record.provider-usage-{index}")).unwrap();
        let range = ObservationSourceRangeV1::new(index * 10, index * 10 + 5).unwrap();
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider.clone(),
            "event_msg",
            record_id.clone(),
            CanonicalObservationRelationsV1::new(session_id.clone())
                .with_turn_id(ObservationId::new("turn.provider-usage").unwrap())
                .with_message_id(record_id.clone()),
            vec![fact],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
        )
        .unwrap();
        let payload = serde_json::to_value(envelope).unwrap();
        let source = ObservationSourceIdentityV1::for_provider(provider, session_id).unwrap();
        let generation = ObservationSourceGenerationV1::new(1).unwrap();
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ObservationOrderingDomainV1::FileBytes,
            record_id,
        )
        .unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.provider-usage-{index}")).unwrap(),
                ComponentVersion::new("sanitizer.provider-usage.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        let observation = DurableObservationV1::new(
            identity,
            receipt,
            RetentionClass::new("retention.provider-usage").unwrap(),
            payload,
        )
        .unwrap();
        let cursor =
            ObservationSourceCursorV1::new(source, scope, generation, range.end()).unwrap();
        (observation, cursor)
    }

    fn usage_fact(input_tokens: u64) -> CanonicalObservationFactV1 {
        CanonicalObservationFactV1::ProviderUsage {
            model: ProviderUsageModelV1::Known {
                model: "gpt-5.6-codex".to_owned(),
            },
            native_scope: ProviderUsageScopeV1::Turn,
            counter_semantics: ProviderUsageCounterSemanticsV1::Delta,
            counters: ProviderUsageCountersV1::Known {
                input_tokens: Some(input_tokens),
                output_tokens: Some(2),
                cache_read_tokens: Some(1),
                cache_write_tokens: None,
                reasoning_tokens: Some(1),
                total_tokens: Some(input_tokens + 2),
            },
            request_id: Some(ObservationId::new(format!("request.{input_tokens}")).unwrap()),
            native_kind: "token_count".to_owned(),
            native_field: "payload.info.last_token_usage".to_owned(),
        }
    }

    async fn seed_observation(
        conn: &impl Executor,
        observation: &DurableObservationV1,
        cursor: &ObservationSourceCursorV1,
        queue: bool,
    ) -> u64 {
        let receipt = observation.receipt();
        conn.execute(
            "INSERT INTO sanitization_receipts
             (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                receipt.receipt().receipt_id().as_str(),
                receipt.receipt().sanitizer_version().as_str(),
                observation.payload_reference().digest().as_str(),
                serde_json::to_string(receipt).unwrap(),
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO observations
             (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                observation.observation_id().as_str(),
                observation.payload_reference().digest().as_str(),
                receipt.receipt().receipt_id().as_str(),
                serde_json::to_string(observation).unwrap(),
                serde_json::to_string(cursor).unwrap(),
            ],
        )
        .await
        .unwrap();
        let mut rows = conn
            .query(
                "SELECT sequence FROM observations WHERE observation_id = ?1",
                (observation.observation_id().as_str(),),
            )
            .await
            .unwrap();
        let sequence = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        if queue {
            conn.execute(
                "INSERT INTO projection_queue (observation_id, observation_sequence)
                 VALUES (?1, ?2)",
                params![observation.observation_id().as_str(), sequence],
            )
            .await
            .unwrap();
        }
        u64::try_from(sequence).unwrap()
    }

    async fn count(conn: &impl QueryExecutor, table: &str) -> i64 {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    #[tokio::test]
    async fn live_usage_replay_is_exactly_once_and_uncorrelated_evidence_is_excluded() {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&conn).await.unwrap();
        let (usage, usage_cursor) = fixture(1, usage_fact(10));
        let usage_sequence = seed_observation(&conn, &usage, &usage_cursor, false).await;

        // Fresh insert. The write path no longer reads every inserted row back,
        // so the durable tuple is asserted here instead.
        apply_provider_usage_effects(&conn, usage_sequence, &usage)
            .await
            .unwrap();
        assert_eq!(count(&conn, "observation_provider_usage").await, 1);
        let mut inserted_rows = conn
            .query(
                "SELECT usage_ordinal, observation_sequence, counters_json
                 FROM observation_provider_usage",
                (),
            )
            .await
            .unwrap();
        let inserted = inserted_rows.next().await.unwrap().unwrap();
        assert_eq!(inserted.get::<i64>(0).unwrap(), 0);
        assert_eq!(
            u64::try_from(inserted.get::<i64>(1).unwrap()).unwrap(),
            usage_sequence
        );
        let counters: serde_json::Value =
            serde_json::from_str(&inserted.get::<String>(2).unwrap()).unwrap();
        assert_eq!(counters["state"], serde_json::json!("known"));
        assert_eq!(counters["input_tokens"], serde_json::json!(10));
        drop(inserted_rows);

        // Identical replay conflicts on the primary key and must converge.
        apply_provider_usage_effects(&conn, usage_sequence, &usage)
            .await
            .unwrap();
        assert_eq!(count(&conn, "observation_provider_usage").await, 1);

        let (uncorrelated, uncorrelated_cursor) = fixture(
            2,
            CanonicalObservationFactV1::UncorrelatedUsage {
                input_tokens: Some(99),
                output_tokens: Some(4),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                native_kind: "fixture_usage".to_owned(),
                native_field: "fixture.usage".to_owned(),
                missing_dimensions: std::collections::BTreeSet::from([
                    tracedecay_domain::ProviderUsageContractDimensionV1::Model,
                ]),
            },
        );
        let sequence = seed_observation(&conn, &uncorrelated, &uncorrelated_cursor, false).await;
        apply_provider_usage_effects(&conn, sequence, &uncorrelated)
            .await
            .unwrap();
        assert_eq!(count(&conn, "observation_provider_usage").await, 1);
    }

    /// A durable row that satisfies the receipt-binding trigger (same
    /// observation, receipt, sequence, and scope) but disagrees on the
    /// projected counters must still be rejected. This is the only branch that
    /// can hide such a row, so it is also the only branch that reads back.
    #[tokio::test]
    async fn live_usage_replay_rejects_a_divergent_durable_row() {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&conn).await.unwrap();
        let (usage, usage_cursor) = fixture(1, usage_fact(10));
        let usage_sequence = seed_observation(&conn, &usage, &usage_cursor, false).await;
        conn.execute(
            "INSERT INTO observation_provider_usage (
                projector_version, observation_id, usage_ordinal, receipt_id,
                observation_sequence, scope_kind, project_id, provider, model_json,
                native_scope, counter_semantics, counters_json, session_id, turn_id,
                message_id, request_id, native_kind, native_field, ordering_domain,
                source_start, source_end, native_timestamp
             ) VALUES (
                ?1, ?2, 0, ?3, ?4, 'profile', NULL, 'codex',
                '{\"state\":\"known\",\"model\":\"gpt-5.6-codex\"}', 'turn', 'delta',
                '{\"state\":\"known\",\"input_tokens\":999}', 'session.provider-usage', NULL,
                NULL, NULL, 'token_count', 'payload.info.last_token_usage',
                'file_bytes', 10, 15, NULL
             )",
            params![
                PROVIDER_USAGE_PROJECTOR_VERSION,
                usage.observation_id().as_str(),
                usage.receipt().receipt().receipt_id().as_str(),
                i64::try_from(usage_sequence).unwrap(),
            ],
        )
        .await
        .unwrap();

        let error = apply_provider_usage_effects(&conn, usage_sequence, &usage)
            .await
            .expect_err("a divergent durable usage row must not be accepted as a replay");

        assert!(
            matches!(error, ProjectionStoreError::ProvenanceCollision),
            "{error:?}"
        );
        assert_eq!(count(&conn, "observation_provider_usage").await, 1);
    }

    #[tokio::test]
    async fn rebuild_stages_and_activates_historical_usage() {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&conn).await.unwrap();
        let project_id = ProjectId::new("project.rebuild-provider-usage").unwrap();
        let (observation, cursor) = fixture_in_scope(
            1,
            usage_fact(10),
            ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
        );
        let frontier = seed_observation(&conn, &observation, &cursor, true).await;

        rebuild_projection_with_engine(&conn, frontier)
            .await
            .unwrap();

        assert_eq!(count(&conn, "observation_provider_usage").await, 1);
        let mut scope_rows = conn
            .query(
                "SELECT scope_kind, project_id FROM observation_provider_usage",
                (),
            )
            .await
            .unwrap();
        let scope_row = scope_rows.next().await.unwrap().unwrap();
        assert_eq!(scope_row.get::<String>(0).unwrap(), "project");
        assert_eq!(
            scope_row.get::<Option<String>>(1).unwrap().as_deref(),
            Some(project_id.as_str())
        );
        assert_eq!(
            count(&conn, "observation_projection_rebuild_provider_usage").await,
            0
        );
    }

    #[tokio::test]
    async fn provider_usage_trigger_rejects_receipt_identity_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&conn).await.unwrap();
        let (first, first_cursor) = fixture(1, usage_fact(10));
        let first_sequence = seed_observation(&conn, &first, &first_cursor, false).await;
        apply_provider_usage_effects(&conn, first_sequence, &first)
            .await
            .unwrap();
        let (second, second_cursor) = fixture(2, usage_fact(20));
        seed_observation(&conn, &second, &second_cursor, false).await;

        let error = conn
            .execute(
                "INSERT INTO observation_provider_usage (
                    projector_version, observation_id, usage_ordinal, receipt_id,
                    observation_sequence, scope_kind, project_id, provider, model_json,
                    native_scope, counter_semantics, counters_json, session_id, turn_id,
                    message_id, request_id, native_kind, native_field, ordering_domain,
                    source_start, source_end, native_timestamp
                 )
                 SELECT projector_version, observation_id, 99, ?1,
                        observation_sequence, scope_kind, project_id, provider, model_json,
                        native_scope, counter_semantics, counters_json, session_id, turn_id,
                        message_id, request_id, native_kind, native_field, ordering_domain,
                        source_start, source_end, native_timestamp
                 FROM observation_provider_usage
                 WHERE observation_id = ?2",
                params![
                    second.receipt().receipt().receipt_id().as_str(),
                    first.observation_id().as_str(),
                ],
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("provider usage provenance does not match observation")
        );
    }

    #[tokio::test]
    async fn missing_projection_checkpoint_is_typed_unknown() {
        let harness = RegisteredGlobalDbHarness::open("provider-usage-missing-checkpoint").await;
        let scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.missing-checkpoint").unwrap(),
        };

        let read = harness
            .registered
            .provider_usage_observations(&scope, None, None, 10)
            .await
            .unwrap();

        assert_eq!(
            read,
            ProviderUsageReadV1::Unknown {
                reason: CanonicalUnknownStateV1::Absent,
                upper_observation_sequence: 0,
            }
        );
    }

    #[tokio::test]
    async fn published_checkpoint_with_no_exact_project_rows_is_known_empty() {
        let harness = RegisteredGlobalDbHarness::open("provider-usage-known-empty").await;
        let db = harness.registered.as_ref();
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO observation_projection_checkpoints
                 (projector_version, last_sequence) VALUES (?1, 7)",
                (SESSION_MESSAGE_PROJECTOR_VERSION,),
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.known-empty").unwrap(),
        };

        let read = db
            .provider_usage_observations(&scope, None, None, 10)
            .await
            .unwrap();

        assert_eq!(
            read,
            ProviderUsageReadV1::Known {
                observations: Vec::new(),
                upper_observation_sequence: 7,
                next_cursor: None,
            }
        );
    }

    #[tokio::test]
    async fn exact_scope_reads_isolate_profile_and_same_session_projects() {
        let harness = RegisteredGlobalDbHarness::open("provider-usage-scope-isolation").await;
        let db = harness.registered.as_ref();
        let scopes = [
            ObservationScopeV1::Profile,
            ObservationScopeV1::Project {
                project_id: ProjectId::new("project.scope-a").unwrap(),
            },
            ObservationScopeV1::Project {
                project_id: ProjectId::new("project.scope-b").unwrap(),
            },
        ];

        for (offset, scope) in scopes.iter().enumerate() {
            let index = u64::try_from(offset).unwrap() + 1;
            let (observation, cursor) =
                fixture_in_scope(index, usage_fact(index * 10), scope.clone());
            let transaction = db.begin_write_transaction().await.unwrap();
            let sequence = seed_observation(&transaction, &observation, &cursor, false).await;
            apply_provider_usage_effects(&transaction, sequence, &observation)
                .await
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO observation_projection_checkpoints
                     (projector_version, last_sequence) VALUES (?1, ?2)
                    ON CONFLICT(projector_version)
                     DO UPDATE SET last_sequence = excluded.last_sequence",
                    params![
                        SESSION_MESSAGE_PROJECTOR_VERSION,
                        i64::try_from(sequence).unwrap()
                    ],
                )
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }

        for scope in &scopes {
            let read = db
                .provider_usage_observations(scope, None, Some("session.provider-usage"), 10)
                .await
                .unwrap();
            match read {
                ProviderUsageReadV1::Known { observations, .. } => {
                    assert_eq!(observations.len(), 1);
                    assert_eq!(&observations[0].scope, scope);
                    assert_eq!(
                        observations[0].session_id.as_str(),
                        "session.provider-usage"
                    );
                }
                other => panic!("unexpected scoped provider usage read: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn pagination_uses_lookahead_and_pins_concurrent_appends_outside_the_scan() {
        let harness = RegisteredGlobalDbHarness::open("provider-usage-pagination").await;
        let db = harness.registered.as_ref();
        for index in 1..=2 {
            let (observation, cursor) = fixture(index, usage_fact(index * 10));
            let transaction = db.begin_write_transaction().await.unwrap();
            let sequence = seed_observation(&transaction, &observation, &cursor, false).await;
            apply_provider_usage_effects(&transaction, sequence, &observation)
                .await
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO observation_projection_checkpoints
                     (projector_version, last_sequence) VALUES (?1, ?2)
                    ON CONFLICT(projector_version)
                     DO UPDATE SET last_sequence = excluded.last_sequence",
                    params![
                        SESSION_MESSAGE_PROJECTOR_VERSION,
                        i64::try_from(sequence).unwrap()
                    ],
                )
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }
        let profile = ObservationScopeV1::Profile;
        let first = db
            .provider_usage_observations_after(&profile, None, None, None, 1)
            .await
            .unwrap();
        let (first_rows, upper, next) = match first {
            ProviderUsageReadV1::Known {
                observations,
                upper_observation_sequence,
                next_cursor: Some(next),
            } => (observations, upper_observation_sequence, next),
            other => panic!("unexpected first provider usage page: {other:?}"),
        };
        assert_eq!(first_rows.len(), 1);
        assert_eq!(upper, 2);
        assert_eq!(next.observation_sequence, 1);
        assert_eq!(next.scope, profile);

        let wrong_scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.cursor-mismatch").unwrap(),
        };
        let error = db
            .provider_usage_observations_after(&wrong_scope, None, None, Some(&next), 1)
            .await
            .unwrap_err();
        assert!(error.contains("cursor scope does not match"));

        let (third, third_cursor) = fixture(3, usage_fact(30));
        let transaction = db.begin_write_transaction().await.unwrap();
        let third_sequence = seed_observation(&transaction, &third, &third_cursor, false).await;
        apply_provider_usage_effects(&transaction, third_sequence, &third)
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE observation_projection_checkpoints
                 SET last_sequence = ?2 WHERE projector_version = ?1",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    i64::try_from(third_sequence).unwrap()
                ],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let second = db
            .provider_usage_observations_after(&profile, None, None, Some(&next), 1)
            .await
            .unwrap();
        match second {
            ProviderUsageReadV1::Known {
                observations,
                upper_observation_sequence,
                next_cursor,
            } => {
                assert_eq!(observations.len(), 1);
                assert_eq!(observations[0].observation_sequence, 2);
                assert_eq!(upper_observation_sequence, 2);
                assert!(next_cursor.is_none());
            }
            other => panic!("unexpected second provider usage page: {other:?}"),
        }
        let fresh = db
            .provider_usage_observations_after(&profile, None, None, None, 10)
            .await
            .unwrap();
        assert!(matches!(
            fresh,
            ProviderUsageReadV1::Known {
                observations,
                upper_observation_sequence: 3,
                next_cursor: None,
            } if observations.len() == 3
        ));
    }
}
