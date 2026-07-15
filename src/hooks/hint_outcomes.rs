//! Post-hoc correlation of emitted tool hints with what the model actually did.
//!
//! Hooks record a `hint_emitted` analytics event (carrying a first-class
//! `hint_id`, `hint_category`, `session_id`, and `hook_<agent>` provider) every
//! time a soft hint surfaces. Whether the model *acted* on that hint is not
//! known at emit time — it depends on which tools fire next. This module closes
//! that loop after the fact: for each emitted hint that has not yet been
//! resolved, it inspects the session's ingested [`session_messages`] activity
//! *after* the hint timestamp and appends a new `hint_outcome` analytics event:
//!
//! * `acted`   — a tracedecay tool matching the hint's category fired inside the
//!   bounded horizon after the hint.
//! * `ignored` — the horizon closed (see below) with post-hint activity but no
//!   matching tool.
//! * *(unresolved)* — the session has no ingested tool activity after the hint
//!   yet, so nothing is written and a later pass re-evaluates it.
//!
//! ## Horizon
//!
//! A hint is judged over the earlier of two bounds after its timestamp:
//! [`HORIZON_TOOL_STEPS`] tool-activity steps or [`HORIZON_SECS`] of wall time.
//! The window is treated as *closed* (making a no-match verdict `ignored`
//! rather than unresolved) when any of these hold:
//!   * a tool-activity step is observed beyond `hint_ts + HORIZON_SECS`
//!     (activity ingested past the time horizon), or
//!   * [`HORIZON_TOOL_STEPS`] steps were observed inside the window, or
//!   * wall-clock `now` is already past `hint_ts + HORIZON_SECS` (the horizon
//!     has elapsed in real time even if the session then went quiet).
//!
//! ## Idempotency
//!
//! Existing `hint_outcome` events are loaded first; any `hint_id` already
//! carrying an outcome is skipped, so re-runs never double-write. Unresolved
//! hints are intentionally left without an outcome event so a later sweep can
//! resolve them once more activity is ingested.
//!
//! The `analytics_db` (where `hint_emitted`/`hint_outcome` rows live) and the
//! `sessions_db` (where `session_messages` are ingested) may be the same handle
//! or two distinct stores; the correlator only reads/writes through the two
//! handles it is given.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord, GlobalDb};

use super::tool_hints::expected_tools_for_key;

/// Wall-clock horizon after a hint within which a matching tool counts as
/// "acted": 30 minutes.
const HORIZON_SECS: i64 = 30 * 60;

/// Tool-activity-step horizon after a hint: at most this many post-hint steps
/// are inspected before the window closes.
const HORIZON_TOOL_STEPS: usize = 25;

/// Upper bound on session-message rows fetched per hint when scanning for
/// post-hint tool activity. Comfortably exceeds [`HORIZON_TOOL_STEPS`] so the
/// horizon — not this cap — decides the window.
const SESSION_SCAN_LIMIT: usize = 256;

/// Upper bound on emitted/outcome hint events pulled per correlation pass.
const HINT_EVENT_LIMIT: usize = 5_000;

/// Aggregate result of one correlation pass, for logging and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HintOutcomeStats {
    /// Emitted hints inspected this pass (excludes already-resolved ones).
    pub scanned: usize,
    /// Hints newly resolved as `acted`.
    pub acted: usize,
    /// Hints newly resolved as `ignored`.
    pub ignored: usize,
    /// Hints left unresolved (no ingested activity after the hint yet).
    pub unresolved: usize,
}

impl HintOutcomeStats {
    /// Total `hint_outcome` events written this pass.
    pub fn written(self) -> usize {
        self.acted + self.ignored
    }
}

/// One post-hint tool-activity step: the timestamp it occurred at and the tool
/// names it fired. A single ingested message can carry several tool calls.
struct ToolStep {
    ts: i64,
    tools: Vec<String>,
}

/// Verdict for a single hint after inspecting its post-hint window.
enum Resolution {
    /// A matching tool fired; carries the matched tool name for the event.
    Acted(String),
    Ignored,
}

/// Correlates emitted hints for `project_id` with post-hint session activity and
/// appends `hint_outcome` events. Reads hint events from (and writes outcomes
/// to) `analytics_db`; reads session activity from `sessions_db`. Best-effort:
/// query errors are swallowed and simply leave hints unresolved for a later
/// pass.
pub async fn correlate_hint_outcomes(
    analytics_db: &GlobalDb,
    sessions_db: &GlobalDb,
    project_id: &str,
    now_secs: i64,
) -> HintOutcomeStats {
    let mut stats = HintOutcomeStats::default();

    // Hints that already carry an outcome: never re-resolve them.
    let mut resolved = resolved_hint_ids(analytics_db, project_id).await;

    let Ok(emitted) = analytics_db
        .query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_id.to_string()),
            event_kind: Some("hint_emitted".to_string()),
            limit: HINT_EVENT_LIMIT,
            ..Default::default()
        })
        .await
    else {
        return stats;
    };

    let mut pending: Vec<AnalyticsEventInsert> = Vec::new();
    for event in &emitted {
        let (Some(hint_id), Some(session_id), Some(category)) = (
            non_empty(event.hint_id.as_deref()),
            non_empty(event.session_id.as_deref()),
            non_empty(event.hint_category.as_deref()),
        ) else {
            continue;
        };
        // Idempotency: skip anything already resolved, and guard against the
        // same hint_id appearing twice within this batch.
        if resolved.contains(hint_id) {
            continue;
        }
        let Some(expected) = expected_tools_for_key(category) else {
            continue;
        };

        stats.scanned += 1;
        resolved.insert(hint_id.to_string());

        let provider = session_provider(&event.provider);
        let steps = match sessions_db
            .session_messages_after(provider, session_id, event.timestamp, SESSION_SCAN_LIMIT)
            .await
        {
            Ok(rows) => tool_steps(&rows),
            Err(_) => Vec::new(),
        };

        match resolve(event.timestamp, &steps, expected, now_secs) {
            Some(resolution) => {
                let (outcome, tool_name) = match resolution {
                    Resolution::Acted(tool) => {
                        stats.acted += 1;
                        ("acted", Some(tool))
                    }
                    Resolution::Ignored => {
                        stats.ignored += 1;
                        ("ignored", None)
                    }
                };
                pending.push(outcome_event(event, outcome, tool_name, now_secs));
            }
            None => stats.unresolved += 1,
        }
    }

    if !pending.is_empty() {
        let _ = analytics_db.append_analytics_events(&pending).await;
    }
    stats
}

/// Loads the set of `hint_id`s that already carry a `hint_outcome` event for
/// this project so resolved hints are never rewritten.
async fn resolved_hint_ids(analytics_db: &GlobalDb, project_id: &str) -> HashSet<String> {
    let outcomes = analytics_db
        .query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_id.to_string()),
            event_kind: Some("hint_outcome".to_string()),
            limit: HINT_EVENT_LIMIT,
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    outcomes
        .into_iter()
        .filter_map(|event| event.hint_id)
        .filter(|id| !id.is_empty())
        .collect()
}

/// Maps a hint event's `hook_<agent>` provider to the session-store provider
/// (`claude`, `codex`, `cursor`, `kiro`) that ingested messages carry.
fn session_provider(hint_provider: &str) -> &str {
    hint_provider.strip_prefix("hook_").unwrap_or(hint_provider)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Projects ingested session rows into ordered tool-activity steps, dropping
/// rows that fired no tools.
fn tool_steps(rows: &[crate::global_db::SessionActivityRow]) -> Vec<ToolStep> {
    let mut steps = Vec::new();
    for row in rows {
        let Some(ts) = row.timestamp else {
            continue;
        };
        let tools = row_tools(row);
        if !tools.is_empty() {
            steps.push(ToolStep { ts, tools });
        }
    }
    steps
}

/// Collects tool names a single ingested message fired: the `tool_names`
/// column (Codex `tool_event` rows and Claude/Cursor message rows both populate
/// it) plus any `metadata_json.tool_events[].tool_name` entries.
fn row_tools(row: &crate::global_db::SessionActivityRow) -> Vec<String> {
    let mut tools = Vec::new();
    if let Some(names) = &row.tool_names {
        tools.extend(crate::analytics::split_tool_names(names));
    }
    if let Some(metadata) = &row.metadata_json
        && let Ok(value) = serde_json::from_str::<Value>(metadata)
        && let Some(events) = value.get("tool_events").and_then(Value::as_array)
    {
        for event in events {
            if let Some(name) = event.get("tool_name").and_then(Value::as_str)
                && !name.is_empty()
            {
                tools.push(name.to_string());
            }
        }
    }
    tools
}

/// Applies the horizon rules to a hint's post-hint tool steps. Returns `None`
/// when the window is still open (unresolved) so a later pass can retry.
fn resolve(
    hint_ts: i64,
    steps: &[ToolStep],
    expected: &[&str],
    now_secs: i64,
) -> Option<Resolution> {
    if steps.is_empty() {
        return None;
    }

    let time_horizon = hint_ts.saturating_add(HORIZON_SECS);
    let mut considered = 0usize;
    let mut activity_beyond_horizon = false;
    for step in steps {
        if step.ts > time_horizon {
            activity_beyond_horizon = true;
            break;
        }
        if considered >= HORIZON_TOOL_STEPS {
            break;
        }
        considered += 1;
        for tool in &step.tools {
            if tool_matches_expected(tool, expected) {
                return Some(Resolution::Acted(tool.clone()));
            }
        }
    }

    let step_horizon_full = considered >= HORIZON_TOOL_STEPS;
    let time_horizon_elapsed = activity_beyond_horizon || now_secs >= time_horizon;
    if step_horizon_full || time_horizon_elapsed {
        Some(Resolution::Ignored)
    } else {
        None
    }
}

/// True when a fired tool name satisfies one of a category's expected tools,
/// tolerating MCP prefixes (`mcp__tracedecay__…`, plugin-namespaced variants)
/// and hyphen/underscore/case differences. The `_`-boundary check avoids
/// matching an unrelated tool that merely ends with the same letters.
fn tool_matches_expected(fired: &str, expected: &[&str]) -> bool {
    let normalized = fired.trim().to_ascii_lowercase().replace('-', "_");
    expected
        .iter()
        .any(|tool| normalized == *tool || normalized.ends_with(&format!("_{tool}")))
}

fn outcome_event(
    emitted: &AnalyticsEventRecord,
    outcome: &str,
    tool_name: Option<String>,
    now_secs: i64,
) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: emitted.provider.clone(),
        project_id: emitted.project_id.clone(),
        session_id: emitted.session_id.clone(),
        timestamp: now_secs,
        event_kind: "hint_outcome".to_string(),
        hook_name: None,
        tool_name,
        tool_category: None,
        skill_name: None,
        hint_category: emitted.hint_category.clone(),
        hint_id: emitted.hint_id.clone(),
        outcome: Some(outcome.to_string()),
        metadata_json: Some(
            json!({
                "source": "hint_outcome_correlator",
                "hint_ts": emitted.timestamp,
            })
            .to_string(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
