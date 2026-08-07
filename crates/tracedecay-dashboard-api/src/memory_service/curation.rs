//! Curation activity, delete/merge op application, and explicit apply payloads.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::{
    SIMILARITY_DEFAULT_THRESHOLD, propose_dedup_actions, propose_hygiene_candidates,
};
use super::facts::{dashboard_overview, fetch_facts};
use super::similarity::similarity_computation;
use crate::tracedecay::facts::memory_application_for_db;

fn curation_apply_snapshot(index: usize, event: &Value) -> Value {
    let id = format!("curate-apply-{}", index + 1);
    json!({
        "id": id,
        "name": id,
        "path": format!("curation://{id}"),
        "ts": event.get("ts").cloned().unwrap_or(Value::Null),
        "summary": event.get("message").cloned().unwrap_or(Value::Null),
        "provider": "tracedecay",
        "mode": "similarity_dedup",
    })
}

pub async fn curation_status_payload(state: &DashboardState) -> Value {
    let activity = state.curation_activity.read().await;
    let apply_finishes: Vec<&Value> = activity
        .iter()
        .filter(|event| {
            event.get("phase").and_then(Value::as_str) == Some("finish")
                && event.get("dry_run").and_then(Value::as_bool) == Some(false)
        })
        .collect();
    let run_count = apply_finishes.len() as i64;
    let latest_run = apply_finishes.last().copied();
    let last_run_at = latest_run
        .and_then(|event| event.get("ts"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_run_summary = latest_run
        .and_then(|event| event.get("message"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_run_id = if run_count > 0 {
        json!(format!("curate-apply-{run_count}"))
    } else {
        Value::Null
    };
    let snapshots: Vec<Value> = apply_finishes
        .iter()
        .rev()
        .take(10)
        .rev()
        .enumerate()
        .map(|(index, event)| curation_apply_snapshot(index, event))
        .collect();
    json!({
        "provider": "tracedecay",
        "state": {
            "paused": false,
            "last_run_at": last_run_at,
            "run_count": run_count,
            "last_run_summary": last_run_summary,
            "last_run_id": last_run_id,
        },
        "config": {
            "enabled": true,
            "interval_hours": null,
            "min_idle_hours": null,
            "mode": "similarity_dedup",
            "dry_run_first": false,
        },
        "snapshots": snapshots,
    })
}

pub async fn push_curation_activity(
    state: &DashboardState,
    phase: &str,
    message: impl Into<String>,
    dry_run: bool,
) {
    push_curation_activity_with_level(state, phase, message, dry_run, "info").await;
}

pub async fn push_curation_activity_with_level(
    state: &DashboardState,
    phase: &str,
    message: impl Into<String>,
    dry_run: bool,
    level: &str,
) {
    let mut events = state.curation_activity.write().await;
    events.push(json!({
        "ts": tracedecay_runtime_core::timeutil::now_iso_utc(),
        "phase": phase,
        "message": message.into(),
        "level": level,
        "dry_run": dry_run,
    }));
    if events.len() > 300 {
        let overflow = events.len() - 300;
        events.drain(0..overflow);
    }
}

pub async fn curation_activity_payload(state: &DashboardState, limit: i64) -> Value {
    let events = state.curation_activity.read().await;
    let limit = limit.max(0) as usize;
    let start = events.len().saturating_sub(limit);
    let visible: Vec<Value> = events[start..].to_vec();
    let count = visible.len();
    json!({ "events": visible, "count": count, "limit": limit, "error": "" })
}

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

pub async fn delete_fact(state: &DashboardState, fact_id: i64) -> Result<bool, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let context = crate::application::memory::MemoryOperationContext::generated(
        &state.memory_owner,
        "dashboard-delete",
        None,
    )
    .map_err(|error| error.to_string())?;
    application
        .remove_fact_v1(fact_id, context)
        .await
        .map_err(|error| error.to_string())
}

/// Builds the shared curation error envelope: `{op, [id], status: "error", error}`.
fn curation_error(op: &str, id: Option<(&str, i64)>, error: impl Into<String>) -> Value {
    let mut envelope = Map::new();
    envelope.insert("op".into(), json!(op));
    if let Some((key, value)) = id {
        envelope.insert(key.into(), json!(value));
    }
    envelope.insert("status".into(), json!("error"));
    envelope.insert("error".into(), json!(error.into()));
    Value::Object(envelope)
}

/// Derives the ok bool from the envelope's embedded status instead of restating it.
fn curation_outcome(result: Value) -> (Value, bool) {
    let ok = result.get("status").and_then(Value::as_str) != Some("error");
    (result, ok)
}

pub async fn apply_delete_op(state: &DashboardState, op: &Value) -> (Value, bool) {
    let Some(fact_id) = op.get("fact_id").and_then(Value::as_i64) else {
        return curation_outcome(curation_error("delete", None, "missing or invalid fact_id"));
    };
    let reason = op.get("reason").and_then(Value::as_str).unwrap_or("");
    let result = match delete_fact(state, fact_id).await {
        Ok(true) => {
            json!({ "op": "delete", "fact_id": fact_id, "reason": reason, "status": "deleted" })
        }
        Ok(false) => curation_error(
            "delete",
            Some(("fact_id", fact_id)),
            format!("fact {fact_id} not found"),
        ),
        Err(e) => curation_error("delete", Some(("fact_id", fact_id)), e),
    };
    curation_outcome(result)
}

pub async fn apply_merge_op(state: &DashboardState, op: &Value) -> (Value, bool) {
    let Some(winner_id) = op.get("winner_id").and_then(Value::as_i64) else {
        return curation_outcome(curation_error(
            "merge",
            None,
            "missing or invalid winner_id",
        ));
    };
    let Some(loser_ids) = op.get("loser_ids").and_then(Value::as_array) else {
        return curation_outcome(curation_error(
            "merge",
            Some(("winner_id", winner_id)),
            "missing or invalid loser_ids",
        ));
    };
    let mut parsed_loser_ids = Vec::with_capacity(loser_ids.len());
    for (index, value) in loser_ids.iter().enumerate() {
        let Some(loser_id) = value.as_i64() else {
            return curation_outcome(curation_error(
                "merge",
                Some(("winner_id", winner_id)),
                format!("loser_ids[{index}] must be an integer"),
            ));
        };
        parsed_loser_ids.push(loser_id);
    }

    let merged_content = op
        .get("merged_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let application = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application,
        Err(error) => {
            return curation_outcome(curation_error(
                "merge",
                Some(("winner_id", winner_id)),
                error.to_string(),
            ));
        }
    };
    let context = match crate::application::memory::MemoryOperationContext::generated(
        &state.memory_owner,
        "dashboard-merge",
        None,
    ) {
        Ok(context) => context,
        Err(error) => {
            return curation_outcome(curation_error(
                "merge",
                Some(("winner_id", winner_id)),
                error.to_string(),
            ));
        }
    };
    let result = match application
        .dashboard_merge_fact_ids_v1(winner_id, parsed_loser_ids, merged_content, context)
        .await
    {
        Ok(outcome) => json!({
            "op": "merge",
            "winner_id": winner_id,
            "content_updated": outcome.content_updated(),
            "deleted_loser_ids": outcome.deleted_losers().iter().filter_map(tracedecay_store::ProjectMemoryFactMappingV1::legacy_fact_id).collect::<Vec<_>>(),
            "failed_losers": [],
            "status": "merged",
        }),
        Err(e) => {
            let mut envelope =
                curation_error("merge", Some(("winner_id", winner_id)), format!("{e:?}"));
            if let Some(obj) = envelope.as_object_mut() {
                obj.insert("content_updated".into(), json!(false));
                obj.insert("deleted_loser_ids".into(), json!([]));
                obj.insert("failed_losers".into(), json!([]));
            }
            envelope
        }
    };
    curation_outcome(result)
}

pub async fn curate_apply_payload(state: &DashboardState, ops: &[Value]) -> Value {
    push_curation_activity(
        state,
        "queued",
        format!("Queued explicit apply for {} curation op(s)", ops.len()),
        false,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!("Applying {} explicit curation op(s)", ops.len()),
        false,
    )
    .await;
    let mut results: Vec<Value> = Vec::with_capacity(ops.len());
    let mut deleted = 0i64;
    let mut merged = 0i64;
    let mut errors = 0i64;

    for op in ops {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        let (result, ok) = match kind {
            "delete" => apply_delete_op(state, op).await,
            "merge" => apply_merge_op(state, op).await,
            other => curation_outcome(curation_error(
                other,
                None,
                format!("unsupported op '{other}' (expected 'delete' or 'merge')"),
            )),
        };
        if ok {
            match kind {
                "delete" => deleted += 1,
                "merge" => merged += 1,
                _ => {}
            }
        } else {
            errors += 1;
        }
        results.push(result);
    }

    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated explicit apply results: {deleted} delete op(s), {merged} merge op(s), {errors} error(s)"
        ),
        false,
    )
    .await;
    if errors > 0 {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!("{errors} explicit curation op(s) were rejected or failed"),
            false,
            "warning",
        )
        .await;
    }
    push_curation_activity(
        state,
        "report",
        format!(
            "Explicit apply report ready: {deleted} delete op(s), {merged} merge op(s), {errors} error(s)"
        ),
        false,
    )
    .await;
    if errors > 0 && deleted == 0 && merged == 0 {
        push_curation_activity_with_level(
            state,
            "failure",
            format!("All {errors} explicit curation op(s) failed validation or apply"),
            false,
            "error",
        )
        .await;
    }
    push_curation_activity(
        state,
        "finish",
        format!(
            "Explicit apply completed: {deleted} delete op(s), {merged} merge op(s), {errors} op(s) errored"
        ),
        false,
    )
    .await;

    json!({
        "results": results,
        "counts": { "deleted": deleted, "merged": merged, "errors": errors },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn curation_apply_snapshot_keeps_dashboard_history_contract() {
        let event = json!({
            "ts": "2026-06-23T00:00:00Z",
            "phase": "finish",
            "message": "Apply completed: 2 fact(s) deleted, 0 action(s) skipped",
            "dry_run": false,
        });

        let snapshot = curation_apply_snapshot(0, &event);

        assert_eq!(snapshot["id"], "curate-apply-1");
        assert_eq!(snapshot["name"], "curate-apply-1");
        assert_eq!(snapshot["path"], "curation://curate-apply-1");
        assert_eq!(snapshot["ts"], "2026-06-23T00:00:00Z");
        assert_eq!(
            snapshot["summary"],
            "Apply completed: 2 fact(s) deleted, 0 action(s) skipped"
        );
        assert_eq!(snapshot["provider"], "tracedecay");
        assert_eq!(snapshot["mode"], "similarity_dedup");
    }
}
