//! Kiro hook handlers and helpers.
//!
//! Kiro sends hook event JSON on stdin. Successful hook stdout is added to
//! context, so handlers stay silent unless they intend to block (exit code 2
//! with stderr sent back to the model).

use std::path::Path;

use serde_json::Value;
use tracedecay_hooks::DaemonHookEvent;

use super::claude::is_code_research_prompt;
use super::post_tool_use::{EmptyPathPolicy, notify_edited_paths};
use super::tool_hints::{HintAgent, ToolHintInput, decide_hint};
use super::{
    event_cwd_from_parsed, event_project_root, event_project_root_from_json,
    event_project_root_or_process_cwd, event_project_root_with_identity_from_json,
    event_session_id, read_hook_event, record_hook_invoked, record_hook_invoked_parsed,
    rel_under_root, research_block_reason,
};

/// Kiro `preToolUse` hook handler.
///
/// Blocks with exit code 2 and stderr, per Kiro's hook contract.
pub fn hook_kiro_pre_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_from_json(&event);
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Kiro, "preToolUse", &event);
    if let Some(reason) = evaluate_kiro_pre_tool_use(&event) {
        eprintln!("{reason}");
        2
    } else {
        0
    }
}

/// Pure decision logic for Kiro `preToolUse` hook events.
///
/// Returns a block reason only for Kiro delegation/subagent tool calls whose
/// task text looks like codebase research that tracedecay MCP tools should
/// answer first.
pub fn evaluate_kiro_pre_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    if !is_kiro_delegation_tool(tool_name) {
        return None;
    }

    let tool_input = parsed.get("tool_input").unwrap_or(&Value::Null);
    if let Some(prompt) = kiro_event_text(tool_input).filter(|text| is_code_research_prompt(text)) {
        let hint = decide_hint(&ToolHintInput {
            agent: HintAgent::Kiro,
            session_id: event_session_id(&parsed),
            tool_name: Some(tool_name.to_string()),
            command: None,
            prompt: Some(prompt),
            subagent_type: Some(tool_name.to_string()),
            file_path: None,
            captured_output: None,
            trusted_failure: false,
            edit_text: None,
            hints_enabled: true,
        });
        Some(research_block_reason(hint))
    } else {
        None
    }
}

fn is_kiro_delegation_tool(tool_name: &str) -> bool {
    matches!(tool_name, "delegate" | "subagent" | "use_subagent")
}

fn kiro_event_text(value: &Value) -> Option<String> {
    let mut text = Vec::new();
    collect_kiro_task_strings(value, &mut text);
    if text.is_empty() {
        collect_strings(value, &mut text);
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

fn collect_kiro_task_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key = key.to_ascii_lowercase();
                if key.contains("prompt")
                    || key.contains("task")
                    || key.contains("query")
                    || key.contains("instruction")
                    || key.contains("message")
                    || key.contains("description")
                {
                    collect_strings(child, out);
                } else {
                    collect_kiro_task_strings(child, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_kiro_task_strings(item, out);
            }
        }
        Value::String(s) => out.push(s),
        _ => {}
    }
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                collect_strings(child, out);
            }
        }
        _ => {}
    }
}

/// Kiro `userPromptSubmit` hook handler.
///
/// Admits Kiro's native prompt boundary through the daemon-owned V2 route.
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_hook_event!();
    if let Some(response) = kiro_prompt_submit_response(&event).await {
        println!("{response}");
    }
    0
}

async fn kiro_prompt_submit_response(event: &str) -> Option<String> {
    let root = event_project_root_with_identity_from_json(event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Kiro, "userPromptSubmit", event);
    super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::Kiro,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// Notifies the daemon after Kiro writes. Missing daemon/index state is
/// fail-open.
pub async fn hook_kiro_post_tool_use() -> i32 {
    let event = read_hook_event!();
    // One parse for the root, the analytics row, and the notification.
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root(&parsed);
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Kiro,
        "postToolUse",
        &event,
        &parsed,
    );
    notify_kiro_post_tool_use(&parsed, &hook_telemetry).await;
    0
}

async fn notify_kiro_post_tool_use(parsed: &Value, telemetry: &super::analytics::HookTimingSpan) {
    let Some(project_root) = event_project_root_or_process_cwd(parsed) else {
        return;
    };
    let cwd = event_cwd_from_parsed(parsed);
    // Kiro's event reports the session `cwd` alongside the paths, so it is sent
    // even when no edited path landed inside the project.
    notify_edited_paths(
        &project_root,
        parsed,
        || kiro_post_tool_use_rel_paths_from_parsed(parsed, &project_root),
        |rel_paths| DaemonHookEvent::kiro_post_tool_use(rel_paths, cwd),
        EmptyPathPolicy::Send,
        Some(telemetry),
    )
    .await;
}

pub fn kiro_post_tool_use_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };
    kiro_post_tool_use_rel_paths_from_parsed(&parsed, project_root)
}

fn kiro_post_tool_use_rel_paths_from_parsed(parsed: &Value, project_root: &Path) -> Vec<String> {
    let cwd = event_cwd_from_parsed(parsed).unwrap_or_else(|| project_root.to_path_buf());
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);

    let mut paths = Vec::new();
    collect_event_path_fields(parsed, &mut paths);
    collect_event_path_fields(tool_input, &mut paths);

    let mut rels = Vec::new();
    for path in paths {
        let path = Path::new(&path);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if let Some(rel) = rel_under_root(project_root, &abs)
            && !rels.contains(&rel)
        {
            rels.push(rel);
        }
    }
    rels
}

fn collect_event_path_fields(value: &Value, out: &mut Vec<String>) {
    for key in ["file_path", "filePath", "path", "target_file", "targetFile"] {
        match value.get(key) {
            Some(Value::String(path)) if !path.is_empty() => out.push(path.clone()),
            Some(Value::Array(paths)) => {
                out.extend(
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(str::to_string),
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_submit_without_provider_event_identity_remains_unavailable() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_kiro_prompt_admission".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([]);
        let event = serde_json::json!({
            "hook_event_name": "userPromptSubmit",
            "session_id": "kiro-prompt-session",
            "cwd": project_root,
            "prompt": "find the active symbol",
        })
        .to_string();

        let started = std::time::Instant::now();
        assert_eq!(kiro_prompt_submit_response(&event).await, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "unsupported prompt classification exceeded the bounded hook path"
        );
        assert!(
            daemon.calls().is_empty(),
            "a documented callback without replay-safe identity must not reach daemon admission"
        );
    }
}
