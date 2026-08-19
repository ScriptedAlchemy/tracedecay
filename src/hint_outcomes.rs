//! Daemon composition for the application hint-outcome correlation port.
//!
//! The production caller is the daemon-side transcript ingest
//! (`tracedecay_hook_runtime` action `ingest_transcript`): freshly ingested
//! session activity is exactly what resolves previously emitted hints, so
//! every project-scope ingest ends with a best-effort
//! [`settle_project_hint_outcomes`] pass. Settlement imports the hook
//! JSONL tail (where hooks record `hint_emitted`) into the durable
//! `analytics_events` authority and then correlates outcomes into the same
//! table — the one the `tracedecay_analytics` hints section and the
//! dashboard analytics API already read.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_application::{
    HintEmission, HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError,
    HintOutcomePortFuture, HintOutcomePortOperation, HintOutcomeResolution, HintToolActivity,
};
use tracedecay_domain::{AdoptionOutcomeLinkedV1, CoverageStateV1};
use tracedecay_usecases::observability::record_adoption_outcome;

use crate::analytics_bridge::HookImportSource;
use crate::global_db::{
    AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb, SessionActivityRow,
};
use crate::hooks::hint_outcomes::HintOutcomeStats;

pub(crate) struct RegisteredHintOutcomeCorrelationPort<'a> {
    analytics: &'a RegisteredGlobalDb,
    sessions: &'a RegisteredGlobalDb,
}

impl<'a> RegisteredHintOutcomeCorrelationPort<'a> {
    pub(crate) const fn new(
        analytics: &'a RegisteredGlobalDb,
        sessions: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            analytics,
            sessions,
        }
    }
}

impl HintOutcomeCorrelationPort for RegisteredHintOutcomeCorrelationPort<'_> {
    fn resolved_hint_ids<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<String>> {
        Box::pin(async move {
            self.analytics
                .query_analytics_events(&AnalyticsEventQuery {
                    project_id: Some(project_id.to_owned()),
                    event_kind: Some("hint_outcome".to_owned()),
                    limit: limit as usize,
                    ..Default::default()
                })
                .await
                .map(|events| {
                    events
                        .into_iter()
                        .filter_map(|event| event.hint_id)
                        .filter(|hint_id| !hint_id.is_empty())
                        .collect()
                })
                .map_err(|error| {
                    HintOutcomePortError::new(HintOutcomePortOperation::QueryResolvedHints, error)
                })
        })
    }

    fn emitted_hints<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintEmission>> {
        Box::pin(async move {
            let events = self
                .analytics
                .query_analytics_events(&AnalyticsEventQuery {
                    project_id: Some(project_id.to_owned()),
                    event_kind: Some("hint_emitted".to_owned()),
                    limit: limit as usize,
                    ..Default::default()
                })
                .await
                .map_err(|error| {
                    HintOutcomePortError::new(HintOutcomePortOperation::QueryEmittedHints, error)
                })?;
            events
                .into_iter()
                .map(|event| {
                    let session_id = required_event_field(event.session_id, "session_id")?;
                    let category = required_event_field(event.hint_category, "hint_category")?;
                    let hint_id = required_event_field(event.hint_id, "hint_id")?;
                    Ok(HintEmission {
                        provider: event.provider,
                        project_id: event.project_id,
                        session_id,
                        timestamp: event.timestamp,
                        category,
                        hint_id,
                    })
                })
                .collect()
        })
    }

    fn session_tool_activity<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
        after_timestamp: i64,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintToolActivity>> {
        Box::pin(async move {
            let rows = self
                .sessions
                .session_messages_after(provider, session_id, after_timestamp, limit as usize)
                .await
                .map_err(|error| {
                    HintOutcomePortError::new(HintOutcomePortOperation::QuerySessionActivity, error)
                })?;
            let mut activity = Vec::new();
            for row in rows {
                if let Some(step) = activity_from_row(row)? {
                    activity.push(step);
                }
            }
            Ok(activity)
        })
    }

    fn append_outcomes<'a>(
        &'a self,
        outcomes: &'a [HintOutcomeObservation],
    ) -> HintOutcomePortFuture<'a, ()> {
        Box::pin(async move {
            let events = outcomes.iter().map(outcome_event).collect::<Vec<_>>();
            self.analytics
                .append_analytics_events(&events)
                .await
                .map(|_| ())
                .map_err(|error| {
                    HintOutcomePortError::new(HintOutcomePortOperation::AppendOutcomes, error)
                })
        })
    }
}

pub(crate) async fn correlate_registered_hint_outcomes(
    analytics: &RegisteredGlobalDb,
    sessions: &RegisteredGlobalDb,
    project_id: &str,
    now_secs: i64,
) -> Result<HintOutcomeStats, HintOutcomePortError> {
    crate::hooks::hint_outcomes::correlate_hint_outcomes(
        &RegisteredHintOutcomeCorrelationPort::new(analytics, sessions),
        project_id,
        now_secs,
    )
    .await
}

/// Typed result of one best-effort post-ingest hint-outcome settlement pass.
/// Observation never blocks or fails the ingest that triggered it: callers
/// attach this state to their output instead of propagating an error.
#[derive(Debug)]
pub(crate) enum HintOutcomeSettlement {
    /// The pass ran: hook JSONL rows were imported into `analytics_events`
    /// and emitted hints were correlated against post-hint session activity.
    Settled {
        /// Hook JSONL rows imported this pass (all event kinds, not only
        /// hint events); the import advances durable byte cursors.
        imported_events: u64,
        /// Per-source import failures. Import is per-file best-effort, so a
        /// broken source is reported here while the others still land.
        import_errors: Vec<String>,
        stats: HintOutcomeStats,
    },
    /// A required store authority is not mounted; nothing ran.
    Unavailable { reason: &'static str },
    /// The correlation pass itself failed with a typed port error.
    Failed(HintOutcomePortError),
}

impl HintOutcomeSettlement {
    /// Renders the settlement into the ingest output object. Every state is
    /// visible — an unavailable authority or failed pass is reported, not
    /// silently dropped.
    pub(crate) fn as_json(&self) -> Value {
        match self {
            Self::Settled {
                imported_events,
                import_errors,
                stats,
            } => json!({
                "status": "ok",
                "imported_events": imported_events,
                "import_errors": import_errors,
                "scanned": stats.scanned,
                "acted": stats.acted,
                "ignored": stats.ignored,
                "unresolved": stats.unresolved,
                "written": stats.written(),
            }),
            Self::Unavailable { reason } => json!({
                "status": "unavailable",
                "reason": reason,
            }),
            Self::Failed(error) => json!({
                "status": "failed",
                "operation": error.operation(),
                "detail": error.detail(),
            }),
        }
    }
}

/// Settles hint outcomes for one project after new session activity landed:
/// imports the hook JSONL tail (the write path of `hint_emitted`) from
/// `sources` into the durable analytics authority, then correlates
/// unresolved hints against the project session store. Callers resolve
/// `sources` themselves (production: `analytics_bridge::hook_import_sources`;
/// fixtures: isolated temp files) so this pass never touches ambient
/// operator state on its own. Best-effort by contract — failures come back
/// as typed [`HintOutcomeSettlement`] states and are logged here so every
/// caller inherits the same observability.
pub(crate) async fn settle_project_hint_outcomes(
    analytics: Option<&RegisteredGlobalDb>,
    sessions: Option<&RegisteredGlobalDb>,
    sources: Vec<HookImportSource>,
    project_root: &Path,
    now_secs: i64,
) -> HintOutcomeSettlement {
    let Some(analytics) = analytics else {
        return HintOutcomeSettlement::Unavailable {
            reason: "accounting_authority_unavailable",
        };
    };
    let Some(sessions) = sessions else {
        return HintOutcomeSettlement::Unavailable {
            reason: "project_session_authority_unavailable",
        };
    };

    let import = crate::analytics_bridge::import_hook_analytics(analytics, sources).await;
    let import_errors: Vec<String> = import
        .sources
        .iter()
        .filter_map(|source| {
            source
                .error
                .as_ref()
                .map(|error| format!("{}: {error}", source.path.display()))
        })
        .collect();
    for error in &import_errors {
        tracing::warn!(
            project_root = %project_root.display(),
            error = %error,
            "hook analytics import failed for one source during hint-outcome settlement"
        );
    }

    let project_id = RegisteredGlobalDb::canonical_project_key(project_root);
    match correlate_registered_hint_outcomes(analytics, sessions, &project_id, now_secs).await {
        Ok(stats) => {
            if stats.scanned > 0 {
                tracing::debug!(
                    project_id = %project_id,
                    scanned = stats.scanned,
                    acted = stats.acted,
                    ignored = stats.ignored,
                    unresolved = stats.unresolved,
                    "hint-outcome settlement pass completed"
                );
            }
            record_settled_adoption_outcomes(sessions, stats).await;
            HintOutcomeSettlement::Settled {
                imported_events: import.imported(),
                import_errors,
                stats,
            }
        }
        Err(error) => {
            tracing::warn!(
                project_id = %project_id,
                operation = error.operation(),
                detail = error.detail(),
                "hint-outcome settlement pass failed"
            );
            HintOutcomeSettlement::Failed(error)
        }
    }
}

/// Records this pass's settled hints as one linked adoption-outcome funnel
/// (`adoption.outcome.linked.v1`). Strictly downstream telemetry: the record
/// result is discarded after the settlement outcome is determined.
///
/// Every stage carries only what settlement proved: the idempotent
/// `hint_outcome` write is the exactly-once terminal ledger, so
/// `invoked`/`terminal` count only hints settled this pass and cross-pass
/// sums never double-count. `independently_useful` = `acted` only — the
/// correlator behaviorally verified a category-matching tool fired in the
/// independently ingested session activity (never display/self-report).
/// `repeat_useful` stays 0 (settlement never verifies repeat use), unresolved
/// hints are re-scanned later rather than carried as per-pass censored mass,
/// and their presence weakens `census_coverage` to `Partial`.
async fn record_settled_adoption_outcomes(sessions: &RegisteredGlobalDb, stats: HintOutcomeStats) {
    let settled = stats.written() as u64;
    if settled == 0 {
        return;
    }
    let census_coverage = if stats.unresolved == 0 {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    if let Err(error) = record_adoption_outcome(
        sessions,
        census_coverage,
        AdoptionOutcomeLinkedV1 {
            invoked: settled,
            terminal: settled,
            independently_useful: stats.acted as u64,
            repeat_useful: 0,
            censored: 0,
            unknown: 0,
        },
    )
    .await
    {
        tracing::debug!(
            error = ?error,
            "settled hint adoption outcome was not recorded; settlement output is unaffected"
        );
    }
}

fn required_event_field(
    value: Option<String>,
    field: &'static str,
) -> Result<String, HintOutcomePortError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        HintOutcomePortError::new(
            HintOutcomePortOperation::QueryEmittedHints,
            format!("hint_emitted row has no {field}"),
        )
    })
}

fn activity_from_row(
    row: SessionActivityRow,
) -> Result<Option<HintToolActivity>, HintOutcomePortError> {
    let timestamp = row.timestamp.ok_or_else(|| {
        HintOutcomePortError::new(
            HintOutcomePortOperation::QuerySessionActivity,
            "session activity row has no timestamp",
        )
    })?;
    let tool_names = activity_tool_names(&row)?;
    if tool_names.is_empty() {
        return Ok(None);
    }
    Ok(Some(HintToolActivity {
        timestamp,
        tool_names,
    }))
}

fn activity_tool_names(row: &SessionActivityRow) -> Result<Vec<String>, HintOutcomePortError> {
    let mut tools = Vec::new();
    if let Some(names) = &row.tool_names {
        tools.extend(crate::analytics::split_tool_names(names));
    }
    if let Some(metadata) = &row.metadata_json {
        let value = serde_json::from_str::<Value>(metadata).map_err(|error| {
            HintOutcomePortError::new(
                HintOutcomePortOperation::QuerySessionActivity,
                format!("session activity metadata is invalid JSON: {error}"),
            )
        })?;
        if let Some(events) = value.get("tool_events").and_then(Value::as_array) {
            tools.extend(events.iter().filter_map(|event| {
                event
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
            }));
        }
    }
    Ok(tools)
}

fn outcome_event(outcome: &HintOutcomeObservation) -> AnalyticsEventInsert {
    let (disposition, tool_name) = match &outcome.resolution {
        HintOutcomeResolution::Acted { tool_name } => ("acted", Some(tool_name.clone())),
        HintOutcomeResolution::Ignored => ("ignored", None),
    };
    AnalyticsEventInsert {
        provider: outcome.emission.provider.clone(),
        project_id: outcome.emission.project_id.clone(),
        session_id: Some(outcome.emission.session_id.clone()),
        timestamp: outcome.observed_at_secs,
        event_kind: "hint_outcome".to_owned(),
        hook_name: None,
        tool_name,
        tool_category: None,
        skill_name: None,
        hint_category: Some(outcome.emission.category.clone()),
        hint_id: Some(outcome.emission.hint_id.clone()),
        outcome: Some(disposition.to_owned()),
        metadata_json: Some(
            json!({
                "source": "hint_outcome_correlator",
                "hint_ts": outcome.emission.timestamp,
            })
            .to_string(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_application::{
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{ObservabilityEnvelopeV1, ObservabilityPayloadV1, ProjectId};
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
    use tracedecay_usecases::host_admission::HostAdmissionScope;
    use tracedecay_usecases::observability::RegisteredObservabilityPortV1;

    use super::*;
    use crate::host_admission::HostAdmissionTestRuntimeV1;

    const HINT_TS: i64 = 1_000_000;
    /// Past `hooks::hint_outcomes::HORIZON_SECS` (30 minutes) after `HINT_TS`,
    /// so a non-matching window settles as ignored instead of staying open.
    const AFTER_HORIZON: i64 = HINT_TS + 2_000;

    struct ProjectSettlementFixture {
        _dir: TempDir,
        runtime: HostAdmissionTestRuntimeV1,
        project_root: std::path::PathBuf,
        /// Scope the observability envelopes are attributed to: the registered
        /// project session binding, not the analytics project key.
        binding_project_id: ProjectId,
    }

    impl ProjectSettlementFixture {
        async fn open(scope: &str) -> Self {
            let dir = TempDir::new().unwrap();
            let project_root = dir.path().join("project");
            std::fs::create_dir_all(&project_root).unwrap();
            let binding_project_id = ProjectId::new(scope).expect("project id");
            let runtime = HostAdmissionTestRuntimeV1::project(
                dir.path().join("profile"),
                &project_root,
                binding_project_id.clone(),
            )
            .await
            .expect("open registered project runtime");
            Self {
                _dir: dir,
                runtime,
                project_root,
                binding_project_id,
            }
        }

        fn analytics(&self) -> &RegisteredGlobalDb {
            self.runtime.profile_database_for_test()
        }

        fn sessions(&self) -> &RegisteredGlobalDb {
            self.runtime
                .registered_database(HostAdmissionScope::Project)
                .expect("registered project session database")
        }

        fn analytics_project_key(&self) -> String {
            RegisteredGlobalDb::canonical_project_key(&self.project_root)
        }

        async fn seed_hint(&self, session_id: &str, hint_id: &str) {
            self.runtime
                .append_profile_analytics_event_for_test(&AnalyticsEventInsert {
                    provider: "hook_claude".to_owned(),
                    project_id: self.analytics_project_key(),
                    session_id: Some(session_id.to_owned()),
                    timestamp: HINT_TS,
                    event_kind: "hint_emitted".to_owned(),
                    hook_name: None,
                    tool_name: None,
                    tool_category: None,
                    skill_name: None,
                    hint_category: Some("search".to_owned()),
                    hint_id: Some(hint_id.to_owned()),
                    outcome: Some("observed".to_owned()),
                    metadata_json: None,
                })
                .await
                .expect("seed hint_emitted");
        }

        async fn seed_session_activity(&self, session_id: &str, tools: Option<&str>) {
            let inserted = self
                .runtime
                .upsert_session_for_test(
                    HostAdmissionScope::Project,
                    &SessionRecord {
                        provider: "claude".to_owned(),
                        session_id: session_id.to_owned(),
                        project_key: self.analytics_project_key(),
                        project_path: self.project_root.display().to_string(),
                        title: None,
                        started_at: Some(HINT_TS),
                        ended_at: None,
                        transcript_path: None,
                        metadata_json: None,
                        parent_session_id: None,
                        is_subagent: false,
                        agent_id: None,
                        parent_tool_use_id: None,
                    },
                )
                .await
                .expect("upsert project session");
            assert!(inserted, "session should upsert");
            let Some(tools) = tools else {
                return;
            };
            let inserted = self
                .runtime
                .upsert_session_message_for_test(
                    HostAdmissionScope::Project,
                    &SessionMessageRecord {
                        provider: "claude".to_owned(),
                        message_id: format!("{session_id}:1"),
                        session_id: session_id.to_owned(),
                        role: "assistant".to_owned(),
                        timestamp: Some(HINT_TS + 60),
                        ordinal: 1,
                        text: "activity".to_owned(),
                        kind: None,
                        model: None,
                        tool_names: Some(tools.to_owned()),
                        source_path: None,
                        source_offset: Some(1),
                        metadata_json: None,
                    },
                )
                .await
                .expect("upsert project session message");
            assert!(inserted, "session message should upsert");
        }

        async fn settle(&self, now_secs: i64) -> HintOutcomeStats {
            let settlement = settle_project_hint_outcomes(
                Some(self.analytics()),
                Some(self.sessions()),
                Vec::new(),
                &self.project_root,
                now_secs,
            )
            .await;
            let HintOutcomeSettlement::Settled { stats, .. } = settlement else {
                panic!("expected a settled pass, got {settlement:?}");
            };
            stats
        }

        async fn adoption_outcome_events(&self) -> Vec<ObservabilityEnvelopeV1> {
            RegisteredObservabilityPortV1::new(self.sessions())
                .query(ObservabilityQueryV1 {
                    authorized_scope_ref: self.binding_project_id.as_str().to_owned(),
                    event_kinds: vec!["adoption.outcome.linked.v1".to_owned()],
                    horizon: ObservabilityHorizonV1 {
                        since_micros: 0,
                        until_micros: i64::MAX,
                    },
                    after_watermark: None,
                    limit: 8,
                })
                .await
                .expect("read persisted adoption outcomes")
                .events
        }
    }

    fn outcome_payload(event: &ObservabilityEnvelopeV1) -> &AdoptionOutcomeLinkedV1 {
        let ObservabilityPayloadV1::AdoptionOutcome(outcome) = &event.payload else {
            panic!("unexpected payload for {}", event.event_kind);
        };
        outcome
    }

    #[tokio::test]
    async fn settlement_records_settled_hints_as_a_linked_adoption_outcome() {
        let fixture = ProjectSettlementFixture::open("project.hint.adoption.mixed").await;
        // One hint acted on (matching tracedecay tool observed after the
        // hint), one ignored (only non-matching activity, horizon elapsed),
        // and one still open (no post-hint activity ingested yet).
        fixture.seed_hint("s-acted", "h-acted").await;
        fixture
            .seed_session_activity("s-acted", Some("tracedecay_context"))
            .await;
        fixture.seed_hint("s-ignored", "h-ignored").await;
        fixture
            .seed_session_activity("s-ignored", Some("Read"))
            .await;
        fixture.seed_hint("s-open", "h-open").await;
        fixture.seed_session_activity("s-open", None).await;

        let stats = fixture.settle(AFTER_HORIZON).await;
        assert_eq!(
            stats,
            HintOutcomeStats {
                scanned: 3,
                acted: 1,
                ignored: 1,
                unresolved: 1,
            }
        );

        let events = fixture.adoption_outcome_events().await;
        assert_eq!(events.len(), 1, "one funnel record per settlement pass");
        assert_eq!(
            outcome_payload(&events[0]),
            &AdoptionOutcomeLinkedV1 {
                invoked: 2,
                terminal: 2,
                independently_useful: 1,
                repeat_useful: 0,
                censored: 0,
                unknown: 0,
            },
            "only exactly-once settled hints may carry funnel mass"
        );
        assert_eq!(
            events[0].coverage,
            CoverageStateV1::Partial,
            "an unresolved remainder must weaken the census, never render Known"
        );

        // A later pass re-scans only the still-open hint, settles nothing,
        // and must not re-count it as new funnel mass.
        let stats = fixture.settle(AFTER_HORIZON + 240).await;
        assert_eq!(
            stats,
            HintOutcomeStats {
                scanned: 1,
                acted: 0,
                ignored: 0,
                unresolved: 1,
            }
        );
        assert_eq!(fixture.adoption_outcome_events().await.len(), 1);
    }

    #[tokio::test]
    async fn settlement_with_only_open_hints_emits_no_adoption_outcome() {
        let fixture = ProjectSettlementFixture::open("project.hint.adoption.open").await;
        fixture.seed_hint("s-open", "h-open").await;
        fixture.seed_session_activity("s-open", None).await;

        let stats = fixture.settle(AFTER_HORIZON).await;
        assert_eq!(
            stats,
            HintOutcomeStats {
                scanned: 1,
                acted: 0,
                ignored: 0,
                unresolved: 1,
            }
        );
        assert!(
            fixture.adoption_outcome_events().await.is_empty(),
            "an all-open pass settled nothing and must not fabricate funnel mass"
        );
    }

    #[tokio::test]
    async fn fully_settled_pass_records_a_known_coverage_census() {
        let fixture = ProjectSettlementFixture::open("project.hint.adoption.known").await;
        fixture.seed_hint("s-acted", "h-acted").await;
        fixture
            .seed_session_activity("s-acted", Some("tracedecay_context"))
            .await;

        let stats = fixture.settle(AFTER_HORIZON).await;
        assert_eq!(
            stats,
            HintOutcomeStats {
                scanned: 1,
                acted: 1,
                ignored: 0,
                unresolved: 0,
            }
        );
        let events = fixture.adoption_outcome_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            outcome_payload(&events[0]),
            &AdoptionOutcomeLinkedV1 {
                invoked: 1,
                terminal: 1,
                independently_useful: 1,
                repeat_useful: 0,
                censored: 0,
                unknown: 0,
            }
        );
        assert_eq!(
            events[0].coverage,
            CoverageStateV1::Known,
            "a pass that settled every scanned hint is a complete census"
        );
    }
}
