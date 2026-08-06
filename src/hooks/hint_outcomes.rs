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
//! Concrete analytics and session stores stay behind the application port.
//! The daemon may compose one or two authorities without exposing either
//! handle to this policy.

use std::collections::HashSet;

use tracedecay_application::{
    HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError, HintOutcomeResolution,
};

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
const SESSION_SCAN_LIMIT: u32 = 256;

/// Upper bound on emitted/outcome hint events pulled per correlation pass.
const HINT_EVENT_LIMIT: u32 = 5_000;

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
/// appends `hint_outcome` observations through the application port. Store
/// failures remain typed so the daemon can surface the failed stage rather
/// than fabricating an empty successful pass.
pub(crate) async fn correlate_hint_outcomes(
    port: &dyn HintOutcomeCorrelationPort,
    project_id: &str,
    now_secs: i64,
) -> Result<HintOutcomeStats, HintOutcomePortError> {
    let mut stats = HintOutcomeStats::default();

    // Hints that already carry an outcome: never re-resolve them.
    let mut resolved = port
        .resolved_hint_ids(project_id, HINT_EVENT_LIMIT)
        .await
        .map(|ids| ids.into_iter().collect::<HashSet<_>>())?;
    let emitted = port.emitted_hints(project_id, HINT_EVENT_LIMIT).await?;

    let mut pending = Vec::new();
    for emission in emitted {
        // Idempotency: skip anything already resolved, and guard against the
        // same hint_id appearing twice within this batch.
        if resolved.contains(&emission.hint_id) {
            continue;
        }
        let Some(expected) = expected_tools_for_key(&emission.category) else {
            continue;
        };

        stats.scanned += 1;
        resolved.insert(emission.hint_id.clone());

        let steps = port
            .session_tool_activity(
                session_provider(&emission.provider),
                &emission.session_id,
                emission.timestamp,
                SESSION_SCAN_LIMIT,
            )
            .await
            .map(|activity| {
                activity
                    .into_iter()
                    .map(|step| ToolStep {
                        ts: step.timestamp,
                        tools: step.tool_names,
                    })
                    .collect::<Vec<_>>()
            })?;

        match resolve(emission.timestamp, &steps, expected, now_secs) {
            Some(resolution) => {
                let resolution = match resolution {
                    Resolution::Acted(tool) => {
                        stats.acted += 1;
                        HintOutcomeResolution::Acted { tool_name: tool }
                    }
                    Resolution::Ignored => {
                        stats.ignored += 1;
                        HintOutcomeResolution::Ignored
                    }
                };
                pending.push(HintOutcomeObservation {
                    emission,
                    observed_at_secs: now_secs,
                    resolution,
                });
            }
            None => stats.unresolved += 1,
        }
    }

    if !pending.is_empty() {
        port.append_outcomes(&pending).await?;
    }
    Ok(stats)
}

/// Maps a hint event's `hook_<agent>` provider to the session-store provider
/// (`claude`, `codex`, `cursor`, `kiro`) that ingested messages carry.
fn session_provider(hint_provider: &str) -> &str {
    hint_provider.strip_prefix("hook_").unwrap_or(hint_provider)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
