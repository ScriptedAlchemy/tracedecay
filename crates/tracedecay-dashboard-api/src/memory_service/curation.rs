//! Deterministic curation plan payloads.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::{
    SIMILARITY_DEFAULT_THRESHOLD, propose_dedup_actions, propose_hygiene_candidates,
};
use super::facts::{dashboard_overview, fetch_facts};
use super::similarity::similarity_computation;

pub async fn build_delete_plan(
    state: &DashboardState,
) -> Result<(Vec<Value>, Value, Map<String, Value>, i64), String> {
    let total = i64::try_from(dashboard_overview(state, 1, 1).await?.fact_count)
        .map_err(|error| error.to_string())?;
    let computation = similarity_computation(state).await?;

    let actions = if computation.facts.len() < 2 || computation.dim == 0 {
        Vec::new()
    } else {
        let planner_len = computation
            .pairs
            .iter()
            .take_while(|pair| pair.similarity >= SIMILARITY_DEFAULT_THRESHOLD)
            .count();
        propose_dedup_actions(&computation.facts, &computation.pairs[..planner_len])
    };

    let dedup_loser_ids: HashSet<i64> = actions
        .iter()
        .filter_map(|action| action.get("fact_id").and_then(Value::as_i64))
        .collect();
    let hygiene_facts = fetch_facts(state, "", total).await?;
    let hygiene_candidates = propose_hygiene_candidates(
        &hygiene_facts,
        &computation.facts,
        &computation.supersession_pairs,
        &dedup_loser_ids,
    );

    let mut counts = Map::new();
    if !actions.is_empty() {
        counts.insert("delete".to_string(), json!(actions.len()));
    }
    Ok((actions, hygiene_candidates, counts, total))
}
