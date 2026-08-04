//! Daemon composition for the application hint-outcome correlation port.

use serde_json::{Value, json};
use tracedecay_application::{
    HintEmission, HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError,
    HintOutcomePortFuture, HintOutcomePortOperation, HintOutcomeResolution, HintToolActivity,
};

use crate::global_db::{
    AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb, SessionActivityRow,
};

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
) -> Result<crate::hooks::hint_outcomes::HintOutcomeStats, HintOutcomePortError> {
    crate::hooks::hint_outcomes::correlate_hint_outcomes(
        &RegisteredHintOutcomeCorrelationPort::new(analytics, sessions),
        project_id,
        now_secs,
    )
    .await
}

pub(crate) async fn observe_registered_hint_outcomes(
    analytics: &RegisteredGlobalDb,
    sessions: &RegisteredGlobalDb,
    project_id: &str,
    now_secs: i64,
) {
    if let Err(error) =
        correlate_registered_hint_outcomes(analytics, sessions, project_id, now_secs).await
    {
        tracing::warn!(%error, "startup hint-outcome correlation failed");
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
