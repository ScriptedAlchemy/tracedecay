//! Cursor `preCompact` machinery.
//!
//! Cursor's compaction event exposes pressure metadata but not Cursor's own
//! generated summary text. The hook delegates pressure admission to the daemon,
//! which ingests the current transcript tail and reports native summary content
//! as unavailable. It never substitutes `cursor-agent` output.

use std::time::Duration;

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

pub async fn cursor_pre_compact_via_daemon(event_json: &str) -> CursorPreCompactOutcome {
    cursor_pre_compact_via_daemon_with_telemetry(event_json, None).await
}

pub(super) async fn cursor_pre_compact_via_daemon_with_telemetry(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> CursorPreCompactOutcome {
    if let Some(telemetry) = telemetry {
        telemetry.note_timeout_budget(CURSOR_PRE_COMPACT_BUDGET);
    }
    if let Ok(outcome) = tokio::time::timeout(
        CURSOR_PRE_COMPACT_BUDGET,
        cursor_pre_compact_via_daemon_inner(event_json, telemetry),
    )
    .await
    {
        if let Some(telemetry) = telemetry {
            telemetry.note_timed_out(false);
        }
        outcome
    } else {
        if let Some(telemetry) = telemetry {
            telemetry.note_timed_out(true);
        }
        CursorPreCompactOutcome::error("timed out")
    }
}

async fn cursor_pre_compact_via_daemon_inner(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
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
        telemetry,
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
