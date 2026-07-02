//! Read-only dashboard endpoint for post-approval automation outcomes:
//! adoption of approved managed skills and recall trajectory of applied fact
//! proposals.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{json, Value};

use super::util::http_detail;
use super::DashboardState;
use crate::automation::fact_proposals::load_fact_proposal_store;
use crate::automation::managed_skills::list_managed_skills;
use crate::automation::outcomes::{
    compute_fact_outcomes, compute_skill_outcomes, load_outcomes_snapshot,
};
use crate::automation::skill_usage::summarize_skill_usage;
use crate::errors::Result;
use crate::tracedecay::current_timestamp;

pub(crate) async fn outcomes(State(state): State<DashboardState>) -> (StatusCode, Json<Value>) {
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
    let profile_root = crate::storage::default_profile_root()?;
    let skills = list_managed_skills(&profile_root).await?;
    let summaries = summarize_skill_usage(&profile_root, &skills).await?;
    let skill_outcomes = compute_skill_outcomes(&summaries, now);

    let proposals = load_fact_proposal_store(&state.dashboard_root)
        .await?
        .proposals;
    let fact_outcomes = compute_fact_outcomes(&proposals, &state.mem_conn, now).await?;

    let snapshot = load_outcomes_snapshot(&state.dashboard_root)
        .await
        .unwrap_or_default();
    Ok(json!({
        "generated_at": now,
        "skills": skill_outcomes,
        "facts": fact_outcomes,
        "snapshot": {
            "skills_refreshed_at": snapshot.skills_refreshed_at,
            "facts_refreshed_at": snapshot.facts_refreshed_at,
        },
        "error": "",
    }))
}
