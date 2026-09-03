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
    event_cwd_from_parsed, event_project_root, event_project_root_or_process_cwd, event_session_id,
    read_hook_event, record_hook_invoked_parsed, rel_under_root, research_block_reason,
};

/// Largest transcript tail the Kiro `userPromptSubmit` hook will read per call.
const KIRO_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Wall-clock budget for the Kiro prompt-submit catch-up ingest.
const KIRO_HOT_INGEST_BUDGET: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Kiro `preToolUse` hook handler.
///
/// Blocks with exit code 2 and stderr, per Kiro's hook contract.
#[hotpath::measure(label = "agent_hosts.hooks.kiro.pre_tool_use")]
pub fn hook_kiro_pre_tool_use() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root(&parsed);
    let _hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Kiro,
        "preToolUse",
        &event,
        &parsed,
    );
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
/// Resets the per-turn counter, catches up transcripts, and injects bounded
/// user/project memory relevant to the submitted prompt.
#[hotpath::measure(future = true, label = "agent_hosts.hooks.kiro.prompt_submit")]
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_or_process_cwd(&parsed);
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Kiro,
        "userPromptSubmit",
        &event,
        &parsed,
    );
    let dispatch_guidance = if let Some(root) = root.as_deref() {
        super::dispatch::dispatch(
            tracedecay_hooks::HookHostV1::Kiro,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    } else {
        None
    };
    if let Some(root) = root.as_deref() {
        super::reset_counter_for_project(root, Some(&hook_telemetry)).await;
    }
    let ingest = super::ingest_transcript_for_event(
        "kiro",
        &event,
        root.as_deref(),
        Some(KIRO_HOT_INGEST_MAX_BYTES),
        KIRO_HOT_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    if ingest.should_schedule_user_review() {
        // User-scope catch-up can ingest several changed Kiro sessions in one
        // bounded sweep, so let the reflector select all recent Kiro evidence
        // instead of falsely attributing the batch to the prompt's session id.
        super::schedule_user_session_review("kiro", None).await;
    }
    let output = dispatch_guidance
        .flatten()
        .unwrap_or_else(|| serde_json::json!({}).to_string());
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::Kiro,
        &event,
        &output,
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// Notifies the daemon after Kiro writes. Missing daemon/index state is
/// fail-open.
#[hotpath::measure(future = true, label = "agent_hosts.hooks.kiro.post_tool_use")]
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
    notify_kiro_post_tool_use(&parsed, root.as_deref(), &hook_telemetry).await;
    0
}

async fn notify_kiro_post_tool_use(
    parsed: &Value,
    project_root: Option<&Path>,
    telemetry: &super::analytics::HookTimingSpan,
) {
    let Some(project_root) = project_root
        .map(Path::to_path_buf)
        .or_else(|| event_project_root_or_process_cwd(parsed))
    else {
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
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn transcript_ingest_forwards_its_budget_to_the_daemon() {
        let _lock = crate::hooks::lock_test_env();
        let cwd = tempfile::tempdir().unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "user_scope": true, "messages_upserted": 3 }),
        ]);
        let event = serde_json::json!({
            "session_id": "kiro-budget",
            "cwd": cwd.path(),
        })
        .to_string();

        let outcome = crate::hooks::ingest_transcript_for_event(
            "kiro",
            &event,
            None,
            Some(8_192),
            std::time::Duration::from_millis(375),
            None,
        )
        .await;

        assert!(outcome.user_scope);
        assert_eq!(outcome.messages_upserted, 3);
        assert!(outcome.should_schedule_user_review());
        assert!(!outcome.failed);
        assert!(!outcome.timed_out);
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].1["action"], "ingest_transcript");
        assert_eq!(calls[0].1["provider"], "kiro");
        assert_eq!(calls[0].1["max_new_bytes"], 8_192);
        assert_eq!(calls[0].1["timeout_budget_ms"], 375);
        assert_eq!(calls[0].1["format"], "json");
    }
}
