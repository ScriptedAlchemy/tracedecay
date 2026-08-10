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
