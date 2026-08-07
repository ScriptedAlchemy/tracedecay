//! Cursor `preCompact` machinery.
//!
//! Cursor's compaction event exposes pressure metadata but not Cursor's own
//! generated summary text. The hook delegates pressure admission to the daemon,
//! which reports native summary content as unavailable without ingesting the
//! transcript. It never substitutes `cursor-agent` output.

use std::time::Duration;

use crate::sessions::lcm::LcmRelationProjectionStatus;

/// A hook only waits for the daemon's typed pressure acknowledgement; the
/// daemon owns any eventual transcript capture and compaction work. The budget
/// covers one handshake plus one hook-runtime round trip, matching the
/// hot-ingest acknowledgement scale used by the sibling Cursor hooks.
const CURSOR_PRE_COMPACT_BUDGET: Duration = Duration::from_millis(1_500);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorPreCompactOutcome {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_node_ids: Vec<String>,
    #[serde(default)]
    pub relation_projection_status: LcmRelationProjectionStatus,
}

impl CursorPreCompactOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
            relation_projection_status: LcmRelationProjectionStatus::NotApplicable,
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
            relation_projection_status: LcmRelationProjectionStatus::NotApplicable,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_compaction_routes_once_to_daemon_within_hook_budget() {
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_cursor_compaction".to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "status": "scheduled",
            "reason": "accepted",
            "summary_nodes_created": 0,
            "summary_node_ids": [],
        })]);
        let event_json = serde_json::json!({
            "hook_event_name": "preCompact",
            "conversation_id": "cursor-compact-session",
            "generation_id": "cursor-compact-generation",
            "workspace_roots": [project_root],
        })
        .to_string();

        let started = std::time::Instant::now();
        let outcome = cursor_pre_compact_via_daemon(&event_json).await;

        assert!(
            started.elapsed() < Duration::from_millis(100),
            "native compaction exceeded the hook budget"
        );
        assert_eq!(outcome.status, "scheduled");
        assert_eq!(
            daemon.calls(),
            [(
                Some(project_root),
                serde_json::json!({
                    "action": "cursor_compact",
                    "event_json": event_json,
                    "format": "json",
                }),
            )]
        );
    }
}
