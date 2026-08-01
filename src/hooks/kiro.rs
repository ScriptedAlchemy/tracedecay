//! Kiro hook handlers and helpers.
//!
//! Kiro sends hook event JSON on stdin. Successful hook stdout is added to
//! context, so handlers stay silent unless they intend to block (exit code 2
//! with stderr sent back to the model).

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_hooks::DaemonHookEvent;

use super::claude::is_code_research_prompt;
use super::memory_inject;
use super::tool_hints::{HintAgent, ToolHintInput, decide_hint};
use super::{
    event_cwd, event_cwd_from_parsed, event_project_root, event_project_root_from_json,
    event_project_root_or_process_cwd, event_session_id, hook_route_metadata_from_event,
    read_hook_event, record_hook_invoked, rel_under_root, research_block_reason,
};

/// Largest transcript tail the Kiro `userPromptSubmit` hook will read per call.
const KIRO_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Wall-clock budget for the Kiro prompt-submit catch-up ingest.
const KIRO_HOT_INGEST_BUDGET: std::time::Duration = std::time::Duration::from_millis(1_500);

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
/// Resets the per-turn counter, catches up transcripts, and injects bounded
/// user/project memory relevant to the submitted prompt.
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_from_json(&event);
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Kiro, "userPromptSubmit", &event);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::Kiro,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!("{guidance}");
        }
        return 0;
    }
    reset_counter_for_kiro_event(&event, Some(&hook_telemetry)).await;
    let ingest = ingest_kiro_transcript_for_event(
        &event,
        Some(KIRO_HOT_INGEST_MAX_BYTES),
        KIRO_HOT_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    if ingest.user_scope && ingest.messages_upserted > 0 {
        // User-scope catch-up can ingest several changed Kiro sessions in one
        // bounded sweep, so let the reflector select all recent Kiro evidence
        // instead of falsely attributing the batch to the prompt's session id.
        super::schedule_user_session_review("kiro", None);
    }
    if let Some(recall) = Box::pin(kiro_prompt_memory_recall(&event)).await {
        println!("{recall}");
    }
    0
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// Notifies the daemon after Kiro writes. Missing daemon/index state is
/// fail-open.
pub async fn hook_kiro_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_from_json(&event);
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Kiro, "postToolUse", &event);
    notify_kiro_post_tool_use(&event, &hook_telemetry).await;
    0
}

async fn reset_counter_for_kiro_event(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    super::reset_counter_for_project(&project_root, telemetry).await;
}

/// Incrementally ingests Kiro IDE transcripts for the workspace referenced by
/// `event_json`. Always fails open.
#[derive(Default)]
struct KiroIngestOutcome {
    user_scope: bool,
    messages_upserted: u64,
}

async fn ingest_kiro_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: std::time::Duration,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> KiroIngestOutcome {
    let project_root = kiro_project_root(event_json);
    let mut args = serde_json::json!({
        "action": "ingest_transcript",
        "provider": "kiro",
        "user_scope": project_root.is_none(),
        "event_json": event_json,
    });
    if let Some(max_new_bytes) = max_new_bytes {
        args["max_new_bytes"] = serde_json::json!(max_new_bytes);
    }
    args["timeout_budget_ms"] = serde_json::json!(budget.as_millis() as u64);
    if let Some(telemetry) = telemetry {
        telemetry.note_timeout_budget(budget);
    }
    match tokio::time::timeout(
        budget,
        super::daemon_hook_action(project_root.as_deref(), args, telemetry),
    )
    .await
    {
        Ok(Ok(result)) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            KiroIngestOutcome {
                user_scope: result
                    .get("user_scope")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                messages_upserted: result
                    .get("messages_upserted")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }
        }
        Ok(Err(error)) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            eprintln!("[tracedecay] Kiro transcript ingest daemon call failed: {error}");
            KiroIngestOutcome::default()
        }
        Err(_) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(true);
            }
            eprintln!("[tracedecay] Kiro transcript ingest daemon call timed out");
            KiroIngestOutcome::default()
        }
    }
}

async fn kiro_prompt_memory_recall(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    // Kiro resolves the root from the event `cwd` alone, without the registry
    // lookup Codex and Cursor make.
    memory_inject::prompt_memory_recall(&parsed, || std::future::ready(event_project_root(&parsed)))
        .await
}

async fn notify_kiro_post_tool_use(event_json: &str, telemetry: &super::analytics::HookTimingSpan) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::is_initialized(&project_root) {
        return;
    }
    let rel_paths = kiro_post_tool_use_rel_paths(event_json, &project_root);
    super::notify_hook_event_with_telemetry(
        &project_root,
        DaemonHookEvent::kiro_post_tool_use(rel_paths, event_cwd(event_json))
            .with_route(hook_route_metadata_from_event(event_json, &project_root)),
        telemetry,
    )
    .await;
}

pub fn kiro_post_tool_use_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };
    let cwd = event_cwd_from_parsed(&parsed).unwrap_or_else(|| project_root.to_path_buf());
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);

    let mut paths = Vec::new();
    collect_event_path_fields(&parsed, &mut paths);
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

/// Kiro write events may omit `cwd`, so the hook falls back to its own working
/// directory before resolving. Shares the host-neutral resolver with every other
/// `cwd`-carrying host.
fn kiro_project_root(event_json: &str) -> Option<PathBuf> {
    // An unreadable payload names no directory, so it takes the same
    // process-cwd fallback a payload without `cwd` does.
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    event_project_root_or_process_cwd(&parsed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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

        let outcome = ingest_kiro_transcript_for_event(
            &event,
            Some(8_192),
            std::time::Duration::from_millis(375),
            None,
        )
        .await;

        assert!(outcome.user_scope);
        assert_eq!(outcome.messages_upserted, 3);
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
