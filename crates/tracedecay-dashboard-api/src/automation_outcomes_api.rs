//! Read-only dashboard endpoint for automatic curation outcomes: adoption of
//! activated managed skills and recall trajectory of automatically applied
//! facts.

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::Serialize;

use super::automation_authority_error_response;
use super::exact_automation_authority;
use super::{DashboardAutomationAuthorityErrorV1, DashboardState, RequestControl};
use crate::memory_api::control::{
    fact_read_control, request_terminal_state, terminal_read_response,
};
use tracedecay_automation_runtime::automation::managed_skills::list_managed_skills;
use tracedecay_automation_runtime::automation::outcomes::{
    AutomationOutcomesSnapshot, FactOutcomeRecord, SkillOutcomeRecord, compute_fact_outcomes,
    compute_skill_outcomes, load_outcomes_snapshot,
};
use tracedecay_automation_runtime::automation::skill_usage::summarize_skill_usage;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_store::FactReadControl;

/// Refresh watermarks of the persisted outcomes snapshot. `available` is
/// false when the snapshot could not be read, which differs from a snapshot
/// that was never refreshed.
#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct AutomationOutcomesSnapshotStatusV1 {
    available: bool,
    skills_refreshed_at: Option<i64>,
    facts_refreshed_at: Option<i64>,
}

/// `GET /api/automation/outcomes`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationOutcomesPayloadV1 {
    generated_at: i64,
    skills: Vec<SkillOutcomeRecord>,
    facts: Vec<FactOutcomeRecord>,
    snapshot: AutomationOutcomesSnapshotStatusV1,
    /// Why the snapshot could not be read; empty when it was.
    error: String,
}

#[hotpath::measure(label = "dashboard_api.outcomes.read", future = true)]
pub async fn outcomes(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
) -> Response {
    let result = outcomes_payload(&state, &fact_read_control(&control)).await;
    if let Some(state) = request_terminal_state(&control) {
        return terminal_read_response(state).into_response();
    }
    match result {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => automation_authority_error_response(error).into_response(),
    }
}

async fn outcomes_payload(
    state: &DashboardState,
    read_control: &FactReadControl,
) -> std::result::Result<AutomationOutcomesPayloadV1, DashboardAutomationAuthorityErrorV1> {
    let now = current_timestamp();
    let authority = exact_automation_authority(state)?;
    let profile_root = authority.profile_root();
    let skills = list_managed_skills(profile_root)
        .await
        .map_err(automation_failure)?;
    let summaries = summarize_skill_usage(profile_root, &skills)
        .await
        .map_err(automation_failure)?;
    let skill_outcomes = compute_skill_outcomes(&summaries, now);

    let memory = crate::tracedecay::facts::memory_application_for_db(
        state.memory_owner.clone(),
        state.mem_db.as_ref(),
    )
    .map_err(|error| {
        automation_failure(format!(
            "could not initialize dashboard memory authority: {error}"
        ))
    })?;
    let fact_outcomes = compute_fact_outcomes(&memory, now, read_control)
        .await
        .map_err(automation_failure)?;

    let (snapshot, error) = snapshot_fields(load_outcomes_snapshot(&state.dashboard_root).await);
    Ok(AutomationOutcomesPayloadV1 {
        generated_at: now,
        skills: skill_outcomes,
        facts: fact_outcomes,
        snapshot,
        error,
    })
}

fn automation_failure(error: impl ToString) -> DashboardAutomationAuthorityErrorV1 {
    DashboardAutomationAuthorityErrorV1::Failed {
        detail: format!(
            "Failed to compute automation outcomes: {}",
            error.to_string()
        ),
    }
}

/// Renders the persisted snapshot's refresh watermarks, or the reason they
/// could not be read.
///
/// A snapshot that failed to load is not a snapshot that has never been
/// refreshed: reporting the defaulted `None` watermarks with an empty `error`
/// asserted that the read succeeded and found nothing.
fn snapshot_fields(
    loaded: Result<AutomationOutcomesSnapshot>,
) -> (AutomationOutcomesSnapshotStatusV1, String) {
    match loaded {
        Ok(snapshot) => (
            AutomationOutcomesSnapshotStatusV1 {
                available: true,
                skills_refreshed_at: snapshot.skills_refreshed_at,
                facts_refreshed_at: snapshot.facts_refreshed_at,
            },
            String::new(),
        ),
        Err(error) => (
            AutomationOutcomesSnapshotStatusV1 {
                available: false,
                skills_refreshed_at: None,
                facts_refreshed_at: None,
            },
            error.to_string(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_domain::errors::TraceDecayError;

    #[test]
    fn a_failed_snapshot_load_reports_the_failure_instead_of_never_refreshed() {
        let (snapshot, error) = snapshot_fields(Err(TraceDecayError::Config {
            message: "failed to parse automation outcomes snapshot '/x/outcomes.json'".to_owned(),
        }));

        assert_eq!(
            snapshot,
            AutomationOutcomesSnapshotStatusV1 {
                available: false,
                skills_refreshed_at: None,
                facts_refreshed_at: None,
            }
        );
        assert!(
            error.contains("failed to parse automation outcomes snapshot"),
            "the failed read must be reported, not an empty error: {error}"
        );
    }

    #[test]
    fn a_never_refreshed_snapshot_stays_distinct_from_a_failed_read() {
        let (snapshot, error) = snapshot_fields(Ok(AutomationOutcomesSnapshot::default()));

        assert!(snapshot.available);
        assert_eq!(snapshot.skills_refreshed_at, None);
        assert!(error.is_empty(), "a successful read reports no error");
    }
}
