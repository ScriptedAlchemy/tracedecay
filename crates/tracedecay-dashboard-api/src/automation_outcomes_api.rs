//! Read-only dashboard endpoint for post-approval automation outcomes:
//! adoption of approved managed skills and recall trajectory of applied fact
//! proposals.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::http_detail;
use tracedecay_agent_hosts::automation::managed_skills::list_managed_skills;
use tracedecay_agent_hosts::automation::outcomes::{
    AutomationOutcomesSnapshot, compute_fact_outcomes, compute_skill_outcomes,
    load_outcomes_snapshot,
};
use tracedecay_agent_hosts::automation::skill_usage::summarize_skill_usage;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::tracedecay::current_timestamp;

pub async fn outcomes(State(state): State<DashboardState>) -> (StatusCode, Json<Value>) {
    match outcomes_payload(&state).await {
        Ok(payload) => (StatusCode::OK, Json(payload)),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(http_detail(&format!(
                "Failed to compute automation outcomes: {err}"
            ))),
        ),
    }
}

async fn outcomes_payload(state: &DashboardState) -> Result<Value> {
    let now = current_timestamp();
    let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
    let skills = list_managed_skills(&profile_root).await?;
    let summaries = summarize_skill_usage(&profile_root, &skills).await?;
    let skill_outcomes = compute_skill_outcomes(&summaries, now);

    let memory = crate::tracedecay::facts::memory_application_for_db(
        state.memory_owner.clone(),
        state.mem_db.as_ref(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not initialize dashboard memory authority: {error}"),
    })?;
    let fact_outcomes = compute_fact_outcomes(&memory, now).await?;

    let (snapshot, error) = snapshot_fields(load_outcomes_snapshot(&state.dashboard_root).await);
    Ok(json!({
        "generated_at": now,
        "skills": skill_outcomes,
        "facts": fact_outcomes,
        "snapshot": snapshot,
        "error": error,
    }))
}

/// Renders the persisted snapshot's refresh watermarks, or the reason they
/// could not be read.
///
/// A snapshot that failed to load is not a snapshot that has never been
/// refreshed: reporting the defaulted `None` watermarks with an empty `error`
/// asserted that the read succeeded and found nothing.
fn snapshot_fields(loaded: Result<AutomationOutcomesSnapshot>) -> (Value, String) {
    match loaded {
        Ok(snapshot) => (
            json!({
                "available": true,
                "skills_refreshed_at": snapshot.skills_refreshed_at,
                "facts_refreshed_at": snapshot.facts_refreshed_at,
            }),
            String::new(),
        ),
        Err(error) => (
            json!({
                "available": false,
                "skills_refreshed_at": Value::Null,
                "facts_refreshed_at": Value::Null,
            }),
            error.to_string(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_snapshot_load_reports_the_failure_instead_of_never_refreshed() {
        let (snapshot, error) = snapshot_fields(Err(TraceDecayError::Config {
            message: "failed to parse automation outcomes snapshot '/x/outcomes.json'".to_owned(),
        }));

        assert_eq!(snapshot["available"], json!(false));
        assert_eq!(snapshot["skills_refreshed_at"], Value::Null);
        assert_eq!(snapshot["facts_refreshed_at"], Value::Null);
        assert!(
            error.contains("failed to parse automation outcomes snapshot"),
            "the failed read must be reported, not an empty error: {error}"
        );
    }

    #[test]
    fn a_never_refreshed_snapshot_stays_distinct_from_a_failed_read() {
        let (snapshot, error) = snapshot_fields(Ok(AutomationOutcomesSnapshot::default()));

        assert_eq!(snapshot["available"], json!(true));
        assert_eq!(snapshot["skills_refreshed_at"], Value::Null);
        assert!(error.is_empty(), "a successful read reports no error");
    }
}
