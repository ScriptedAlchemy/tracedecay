//! Kiro hook handlers and helpers.
//!
//! Kiro sends hook event JSON on stdin. Successful hook stdout is added to
//! context, so handlers stay silent unless they intend to block (exit code 2
//! with stderr sent back to the model).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude::is_code_research_prompt;
use super::tool_hints::{decide_hint, HintAgent, ToolHintInput};
use super::{
    event_cwd, event_cwd_from_parsed, event_session_id, read_hook_event, record_hook_invoked,
    rel_under_root, research_block_reason,
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
    record_hook_invoked(None, HintAgent::Kiro, "preToolUse", &event);
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
/// Resets the per-turn counter and runs bounded Kiro transcript catch-up.
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_hook_event!();
    record_hook_invoked(None, HintAgent::Kiro, "userPromptSubmit", &event);
    reset_counter_for_kiro_event(&event).await;
    ingest_kiro_transcript_for_event(
        &event,
        Some(KIRO_HOT_INGEST_MAX_BYTES),
        KIRO_HOT_INGEST_BUDGET,
    )
    .await;
    0
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// Notifies the daemon after Kiro writes. Missing daemon/index state is
/// fail-open.
pub async fn hook_kiro_post_tool_use() -> i32 {
    let event = read_hook_event!();
    record_hook_invoked(None, HintAgent::Kiro, "postToolUse", &event);
    notify_kiro_post_tool_use(&event).await;
    0
}

async fn reset_counter_for_kiro_event(event_json: &str) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Incrementally ingests Kiro IDE transcripts for the workspace referenced by
/// `event_json`. Always fails open.
async fn ingest_kiro_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: std::time::Duration,
) {
    let work = async {
        let Some(project_root) = kiro_project_root(event_json) else {
            return;
        };
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return;
        };
        let _ =
            crate::sessions::kiro::ingest_kiro_for_project(&db, &project_root, max_new_bytes).await;
    };
    let _ = tokio::time::timeout(budget, work).await;
}

async fn notify_kiro_post_tool_use(event_json: &str) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&project_root).await {
        return;
    }
    let rel_paths = kiro_post_tool_use_rel_paths(event_json, &project_root);
    crate::daemon::notify_hook_event(
        &project_root,
        crate::daemon::DaemonHookEvent::kiro_post_tool_use(rel_paths, event_cwd(event_json)),
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
        if let Some(rel) = rel_under_root(project_root, &abs) {
            if !rels.contains(&rel) {
                rels.push(rel);
            }
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

fn kiro_project_root(event_json: &str) -> Option<PathBuf> {
    let cwd = event_cwd(event_json).or_else(|| std::env::current_dir().ok())?;
    crate::config::discover_project_root(&cwd)
}
