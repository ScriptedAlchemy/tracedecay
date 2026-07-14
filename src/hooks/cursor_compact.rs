//! Cursor `preCompact` machinery.
//!
//! Cursor's compaction event exposes pressure metadata but not Cursor's own
//! generated summary text, so at the boundary `TraceDecay` ingests the current
//! transcript tail, asks LCM for the compactable raw-message backlog,
//! generates a summary through `cursor-agent -p`, and stores that summary as
//! a normal LCM summary node.

use std::time::Duration;

/// Budget for the auxiliary `cursor-agent` summary call inside the hook. Kept
/// below the registered Cursor hook timeout so the child can be killed/reaped
/// by `TraceDecay` rather than by Cursor killing the hook process. Sized so
/// the ingest budget plus this cap stay below the overall preCompact budget,
/// leaving slack for LCM prepare/persist and process overhead.
pub(super) const CURSOR_PRE_COMPACT_SUMMARY_BUDGET: Duration = Duration::from_secs(75);
/// Overall budget for the `preCompact` hook (registered with a 120s timeout).
const CURSOR_PRE_COMPACT_BUDGET: Duration = Duration::from_secs(115);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorPreCompactOutcome {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_node_ids: Vec<String>,
}

impl CursorPreCompactOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }
}

pub async fn cursor_pre_compact_for_event_with_config(
    event_json: &str,
    config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    match tokio::time::timeout(
        CURSOR_PRE_COMPACT_BUDGET,
        cursor_pre_compact_for_event_inner(event_json, config),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => CursorPreCompactOutcome::error("timed out"),
    }
}

async fn cursor_pre_compact_for_event_inner(
    event_json: &str,
    _config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    let root = serde_json::from_str::<serde_json::Value>(event_json)
        .ok()
        .as_ref()
        .and_then(super::cursor::cursor_project_root_from_parsed_event);
    let Some(root) = root else {
        return CursorPreCompactOutcome::skipped("no project root");
    };
    let result = match super::daemon_hook_action(
        Some(&root),
        serde_json::json!({
            "action": "cursor_compact",
            "event_json": event_json,
        }),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return CursorPreCompactOutcome::error(format!(
                "daemon compaction call failed: {error}"
            ));
        }
    };
    serde_json::from_value(result).unwrap_or_else(|error| {
        CursorPreCompactOutcome::error(format!("invalid daemon compaction response: {error}"))
    })
}
