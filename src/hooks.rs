//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! These functions are invoked by each agent's hook system to intercept tool
//! calls, redirect exploration work to tokensave MCP tools, keep the index
//! fresh after edits / git state changes, and track per-session token savings.
//! Each agent sends its own event schema on stdin and expects its own output
//! shape, so the handlers are kept agent-specific rather than shared blindly.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod tool_hints;

use tool_hints::{decide_hint, HintAgent, ToolHintDedupe, ToolHintInput};

const TOKENSAVE_RESEARCH_BLOCK_REASON: &str = "STOP: Use tokensave MCP tools \
(tokensave_context, tokensave_search, tokensave_callees, tokensave_callers, \
tokensave_impact, tokensave_files, tokensave_affected) instead of agents for \
code research. Tokensave is faster and more precise for symbol relationships, \
call paths, and code structure. Only use agents for code exploration if you \
have already tried tokensave and it cannot answer the question.";

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
///
/// Reads the `TOOL_INPUT` environment variable (JSON), inspects the
/// `subagent_type` and `prompt` fields, and prints a JSON decision to
/// stdout. Blocks Explore agents and exploration-style prompts, directing
/// Claude to use tokensave MCP tools instead.
pub fn hook_pre_tool_use() {
    let tool_input = std::env::var("TOOL_INPUT").unwrap_or_default();
    let decision = evaluate_hook_decision(&tool_input);
    if !decision.is_empty() {
        println!("{decision}");
    }
}

/// Pure decision logic for the `PreToolUse` hook.
///
/// Takes the raw `TOOL_INPUT` JSON string and returns the JSON decision
/// string to print to stdout.
pub fn evaluate_hook_decision(tool_input: &str) -> String {
    let block_msg = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": TOKENSAVE_RESEARCH_BLOCK_REASON
        }
    });

    let parsed: serde_json::Value =
        serde_json::from_str(tool_input).unwrap_or_else(|_| serde_json::json!({}));

    // Block Explore agents outright
    if parsed.get("subagent_type").and_then(|v| v.as_str()) == Some("Explore") {
        return block_msg.to_string();
    }

    // Check if the prompt is exploration/research work that tokensave can handle
    if let Some(prompt) = parsed.get("prompt").and_then(|v| v.as_str()) {
        if is_code_research_prompt(prompt) {
            return block_msg.to_string();
        }
    }

    // Empty string = no output -> Claude Code implicitly allows the tool call
    String::new()
}

fn is_code_research_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    let exploration_patterns = [
        "explore",
        "codebase structure",
        "codebase architecture",
        "codebase overview",
        "source files contents",
        "read every",
        "full contents",
        "entire codebase",
        "architecture and structure",
        "call graph",
        "call path",
        "call chain",
        "symbol relat",
        "symbol lookup",
        "who calls",
        "callers of",
        "callees of",
    ];
    exploration_patterns.iter().any(|pat| lower.contains(pat))
}

/// Kiro `preToolUse` hook handler.
///
/// Kiro sends the hook event JSON on stdin. Returning exit code 2 blocks the
/// tool call and sends stderr back to the model. This is intentionally separate
/// from Claude's hook handler because Claude expects a JSON decision on stdout.
pub fn hook_kiro_pre_tool_use() -> i32 {
    let event = read_stdin_to_string();
    if let Some(reason) = evaluate_kiro_pre_tool_use(&event) {
        eprintln!("{reason}");
        2
    } else {
        0
    }
}

/// Cursor `subagentStart` hook handler.
///
/// Cursor sends hook event JSON on stdin and expects Cursor-shaped JSON on
/// stdout. This intentionally does not reuse the Claude hook output schema.
pub fn hook_cursor_subagent_start() -> i32 {
    let event = read_stdin_to_string();
    if let Some(decision) = evaluate_cursor_subagent_start(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `preToolUse` hook handler.
///
/// Parses Cursor's tool event JSON and emits nonblocking `additional_context`
/// soft hints for high-confidence code research tools. It never denies,
/// rewrites, or blocks the tool call.
pub fn hook_cursor_pre_tool_use() -> i32 {
    let event = read_stdin_to_string();
    if let Some(output) = evaluate_cursor_pre_tool_use(&event) {
        println!("{output}");
    }
    0
}

/// Cursor `beforeSubmitPrompt` hook handler.
///
/// Resets the project-local counter for a new prompt turn. The output uses
/// Cursor's documented `beforeSubmitPrompt` shape and never blocks submission.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_stdin_to_string();
    reset_counter_for_cursor_event(&event).await;
    println!("{}", serde_json::json!({ "continue": true }));
    0
}

/// Cursor `afterFileEdit` hook handler.
///
/// Keeps the graph fresh after Cursor Agent writes files. This uses a
/// **targeted** single-file sync (`sync_if_stale_silent`) scoped to the edited
/// path(s) rather than a full-tree `sync()`. The agent can edit many files per
/// turn, and a full-tree scan per edit scales with repo size, not edit size —
/// prohibitively expensive on large codebases. The targeted path skips the
/// scan, no-ops when not stale, and waits/gives up on the sync lock, so no
/// time-based debounce is needed. Fail-open and silent.
pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_stdin_to_string();
    targeted_sync_for_cursor_after_file_edit(&event).await;
    0
}

/// Cursor `sessionStart` hook handler (fire-and-forget).
///
/// Emits Cursor's `sessionStart` output shape (`additional_context` + `env`)
/// steering the agent toward tokensave MCP tools and reporting index freshness
/// for the resolved workspace. Never blocks session creation.
pub async fn hook_cursor_session_start() -> i32 {
    let event = read_stdin_to_string();
    let root = cursor_project_root_from_event(&event);
    let context = session_steering_context_for_root(root.as_deref()).await;
    println!("{}", cursor_session_start_json(root.as_deref(), &context));
    0
}

/// Builds the tokensave steering `additional_context` for a resolved project
/// root: reports index freshness when initialized, otherwise suggests
/// `tokensave init`. Shared by the Cursor and Codex session/prompt hooks.
async fn session_steering_context_for_root(root: Option<&Path>) -> String {
    let (initialized, staleness) = match root {
        Some(r) if crate::tokensave::TokenSave::is_initialized(r) => {
            (true, cursor_staleness_for_root(r).await)
        }
        _ => (false, None),
    };
    build_cursor_session_context(initialized, staleness.as_deref())
}

/// Cursor `afterShellExecution` hook handler.
///
/// When the executed command is a git state-changing command (checkout,
/// switch, pull, merge, rebase, reset, cherry-pick, stash apply/pop), a
/// broader change set is expected, so a full incremental `sync()` is
/// acceptable. Back-to-back git commands are coalesced via a short marker-based
/// guard (and the sync lock no-ops concurrent runs). Fail-open and silent.
pub async fn hook_cursor_after_shell() -> i32 {
    let event = read_stdin_to_string();
    sync_after_cursor_shell_event(&event).await;
    0
}

/// Cursor `workspaceOpen` hook handler.
///
/// Runs a one-shot catch-up incremental `sync()` when the workspace has a
/// tokensave index, picking up changes made while no agent was attached. We
/// don't load plugins, so the output is an empty object. Fail-open.
pub async fn hook_cursor_workspace_open() -> i32 {
    let event = read_stdin_to_string();
    workspace_open_for_cursor_event(&event).await;
    println!("{}", serde_json::json!({}));
    0
}

/// Pure decision logic for Cursor `subagentStart` hook events.
///
/// Returns Cursor-shaped nonblocking `additional_context` only when a
/// research-oriented subagent should be softly steered toward tokensave MCP
/// tools.
pub fn evaluate_cursor_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let input = cursor_subagent_hint_input(&parsed);
    let mut dedupe = ToolHintDedupe::default();
    cursor_tool_hint_output(&input, &mut dedupe)
}

/// Pure decision logic for Cursor `preToolUse` hook events.
///
/// Returns Cursor-shaped nonblocking `additional_context` for high-confidence
/// search, broad-read, call-graph, and impact tool attempts. Invalid or
/// unknown events fail open with no output.
pub fn evaluate_cursor_pre_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let input = cursor_pre_tool_hint_input(&parsed);
    let mut dedupe = ToolHintDedupe::default();
    cursor_tool_hint_output(&input, &mut dedupe)
}

pub fn cursor_tool_hint_output(
    input: &ToolHintInput,
    dedupe: &mut ToolHintDedupe,
) -> Option<String> {
    let hint = decide_hint(input)?;
    if !dedupe.should_emit(hint_session_id(input), hint.category) {
        return None;
    }
    Some(
        serde_json::json!({
            "continue": true,
            "additional_context": tool_hint_context(&hint),
        })
        .to_string(),
    )
}

fn cursor_subagent_hint_input(event: &Value) -> ToolHintInput {
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(event),
        tool_name: Some("SubagentStart".to_string()),
        prompt: event_text_field(event, &["task", "prompt", "description"]),
        subagent_type: event_text_field(event, &["subagent_type", "subagentType", "agent_type"]),
        hints_enabled: true,
        ..ToolHintInput::default()
    }
}

fn cursor_pre_tool_hint_input(event: &Value) -> ToolHintInput {
    tool_hint_input_from_pre_tool_event(HintAgent::Cursor, event)
}

fn tool_hint_input_from_pre_tool_event(agent: HintAgent, event: &Value) -> ToolHintInput {
    let tool_name = event_tool_name(event);
    let tool_input = event.get("tool_input").unwrap_or(&Value::Null);
    let file_path = tool_input_path(tool_input);
    let prompt = tool_prompt_for_hint(tool_name.as_deref(), tool_input, file_path.as_deref());
    let command = tool_command_for_hint(tool_name.as_deref(), tool_input);

    ToolHintInput {
        agent,
        session_id: event_session_id(event),
        tool_name,
        command,
        prompt,
        subagent_type: tool_input_text_field(tool_input, &["subagent_type", "agent_type"]),
        file_path,
        hints_enabled: true,
    }
}

fn tool_command_for_hint(tool_name: Option<&str>, tool_input: &Value) -> Option<String> {
    let command = tool_input_text_field(tool_input, &["command", "cmd", "query", "pattern"]);
    if tool_name.is_some_and(is_search_tool_name) {
        return Some("rg tokensave-search-hint".to_string());
    }
    let command = command?;
    if tool_name.is_some_and(is_shell_tool_name) && is_high_confidence_search_command(&command) {
        Some("rg tokensave-search-hint".to_string())
    } else {
        Some(command)
    }
}

fn tool_prompt_for_hint(
    tool_name: Option<&str>,
    tool_input: &Value,
    file_path: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(name) = tool_name {
        if is_call_graph_tool_name(name) {
            parts.push("who calls".to_string());
        } else if is_impact_tool_name(name) {
            parts.push("impact change-risk".to_string());
        }
    }
    collect_named_text(
        tool_input,
        &[
            "prompt",
            "task",
            "query",
            "instruction",
            "message",
            "description",
        ],
        &mut parts,
    );
    if tool_name.is_some_and(is_read_tool_name) && is_broad_read_tool_input(tool_input, file_path) {
        parts.push("read every file in this directory".to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn tool_hint_context(hint: &tool_hints::ToolHint) -> String {
    format!("{}\n\n{}", hint.message, hint.context)
}

fn hint_session_id(input: &ToolHintInput) -> String {
    input
        .session_id
        .clone()
        .unwrap_or_else(|| "default".to_string())
}

fn event_tool_name(event: &Value) -> Option<String> {
    event_text_field(event, &["tool_name", "toolName", "name"])
}

fn event_session_id(event: &Value) -> Option<String> {
    event_text_field(event, &["session_id", "sessionId"])
}

fn event_text_field(event: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| event.get(*key).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn tool_input_path(tool_input: &Value) -> Option<String> {
    tool_input_text_field(tool_input, &["path", "file_path", "filePath"])
}

fn tool_input_text_field(tool_input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| tool_input.get(*key).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn collect_named_text(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if keys
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                {
                    collect_text_values(child, out);
                } else {
                    collect_named_text(child, keys, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_named_text(item, keys, out);
            }
        }
        _ => {}
    }
}

fn collect_text_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if !s.is_empty() => out.push(s.to_string()),
        Value::Array(items) => {
            for item in items {
                collect_text_values(item, out);
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                collect_text_values(child, out);
            }
        }
        _ => {}
    }
}

fn is_broad_read_tool_input(tool_input: &Value, file_path: Option<&str>) -> bool {
    if tool_input
        .get("recursive")
        .or_else(|| tool_input.get("all"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    file_path.is_some_and(|path| {
        path.ends_with('/')
            || path.contains('*')
            || path == "."
            || path == "src"
            || path.ends_with("/src")
    })
}

fn is_high_confidence_search_command(command: &str) -> bool {
    let first = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | ';' | ','))
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "rg" | "ripgrep" | "grep" | "find" | "search"
    )
}

fn is_shell_tool_name(name: &str) -> bool {
    matches_normalized_local(name, &["bash", "shell"])
}

fn is_read_tool_name(name: &str) -> bool {
    matches_normalized_local(name, &["read", "readfile", "read_file"])
}

fn is_search_tool_name(name: &str) -> bool {
    matches_normalized_local(name, &["search", "grep", "ripgrep", "rg", "find"])
}

fn is_call_graph_tool_name(name: &str) -> bool {
    matches_normalized_local(
        name,
        &[
            "whocalls",
            "callers",
            "callersof",
            "callees",
            "calleesof",
            "callgraph",
        ],
    )
}

fn is_impact_tool_name(name: &str) -> bool {
    matches_normalized_local(name, &["impact", "changerisk", "affected", "affectedfiles"])
}

fn matches_normalized_local(value: &str, expected: &[&str]) -> bool {
    let normalized = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    expected.iter().any(|candidate| normalized == *candidate)
}

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_event_candidates(&parsed)
        .into_iter()
        .find_map(|candidate| crate::config::discover_project_root(&candidate))
}

fn cursor_event_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|s| !s.is_empty()) {
                candidates.push(PathBuf::from(path));
            }
        }
    }
    if let Some(cwd) = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        candidates.push(PathBuf::from(cwd));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let path = Path::new(file_path);
        candidates.push(path.parent().unwrap_or(path).to_path_buf());
    }
    candidates
}

/// Returns `true` when `command` is a git invocation that changes the working
/// tree / HEAD enough that a broad re-sync is warranted (checkout, switch,
/// pull, merge, rebase, reset, cherry-pick, `stash pop`/`stash apply`).
///
/// Read-only git commands (`status`, `log`, `diff`), `commit`/`add`, and
/// non-git commands return `false`. Only commands whose first shell word is
/// `git` match, so `echo git checkout` is ignored.
pub fn is_git_state_changing_command(command: &str) -> bool {
    let tokens = shell_words(command);
    let Some(sub_pos) = git_subcommand_pos(&tokens) else {
        return false;
    };
    let sub = tokens[sub_pos].to_ascii_lowercase();
    match sub.as_str() {
        "checkout" | "switch" | "pull" | "merge" | "rebase" | "reset" | "cherry-pick" => true,
        "stash" => {
            let after = tokens
                .iter()
                .skip(sub_pos + 1)
                .map(|t| t.to_ascii_lowercase())
                .find(|t| !t.starts_with('-'));
            matches!(after.as_deref(), Some("pop") | Some("apply"))
        }
        _ => false,
    }
}

/// The action a Cursor `afterShellExecution` hook should take for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShellSyncPlan {
    /// Bootstrap/maintain branch tracking for the given branch (supersedes a
    /// plain sync; the branch-add path copies the parent DB and syncs).
    BranchAdd(String),
    /// Run a full incremental sync (same-branch change set).
    IncrementalSync,
    /// Ensure the current branch is tracked, then sync it if it already was.
    CurrentBranchSync(String),
    /// Do nothing.
    Noop,
}

/// Classifies a shell command into the sync action a Cursor
/// `afterShellExecution` hook should take. Branch switches take precedence
/// over plain incremental syncs.
pub fn cursor_shell_sync_plan(command: &str) -> CursorShellSyncPlan {
    cursor_shell_sync_plan_with_current_branch(command, None)
}

/// Like [`cursor_shell_sync_plan`], but supplies the post-command current branch
/// for state-changing commands whose branch target is ambiguous or implicit.
pub fn cursor_shell_sync_plan_with_current_branch(
    command: &str,
    current_branch: Option<&str>,
) -> CursorShellSyncPlan {
    if let Some(branch) = cursor_branch_switch_target(command) {
        return CursorShellSyncPlan::BranchAdd(branch);
    }
    if is_git_state_changing_command(command) {
        if let Some(branch) = current_branch.filter(|branch| !branch.is_empty()) {
            return CursorShellSyncPlan::CurrentBranchSync(branch.to_string());
        }
        return CursorShellSyncPlan::IncrementalSync;
    }
    CursorShellSyncPlan::Noop
}

/// Returns the target branch for a branch-changing git command:
/// `git checkout <branch>`, `git switch <branch>`, `git checkout -b <branch>`,
/// `git switch -c <branch>`, and `git worktree add [<path>] <branch>` /
/// `git worktree add -b <branch> <path>`.
///
/// Path checkouts (`git checkout -- <file>`) and non-switch commands return
/// `None`. Only commands whose first shell word is `git` are considered.
pub fn cursor_branch_switch_target(command: &str) -> Option<String> {
    let raw = shell_words(command);
    let sub_pos = git_subcommand_pos(&raw)?;
    let sub = raw[sub_pos].to_ascii_lowercase();

    match sub.as_str() {
        "checkout" | "switch" => {
            let after = &raw[sub_pos + 1..];
            let mut i = 0;
            while i < after.len() {
                let tok = &after[i];
                if tok == "--" {
                    return None;
                }
                if matches!(tok.as_str(), "-b" | "-B" | "-c" | "-C" | "--orphan") {
                    return after.get(i + 1).cloned();
                }
                if tok.starts_with('-') {
                    i += 1;
                    continue;
                }
                return Some(tok.clone());
            }
            None
        }
        "worktree" => {
            let add_pos = raw.iter().position(|t| t.eq_ignore_ascii_case("add"))?;
            let after = &raw[add_pos + 1..];
            let mut iter = after.iter();
            while let Some(tok) = iter.next() {
                if matches!(tok.as_str(), "-b" | "-B") {
                    return iter.find(|t| !t.starts_with('-')).cloned();
                }
                if tok.starts_with('-') {
                    continue;
                }
                break;
            }
            // No `-b`: positionals are `<path> [<branch>]`; the branch is the
            // second positional, if present.
            let positionals: Vec<&str> = after
                .iter()
                .filter(|t| !t.starts_with('-'))
                .map(String::as_str)
                .collect();
            positionals.get(1).map(|b| (*b).to_string())
        }
        _ => None,
    }
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in command.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => current.push(c),
            },
            _ => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => escaped = true,
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            },
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn git_subcommand_pos(tokens: &[String]) -> Option<usize> {
    if !tokens.first()?.eq_ignore_ascii_case("git") {
        return None;
    }

    let mut i = 1;
    while i < tokens.len() {
        let token = tokens[i].to_ascii_lowercase();
        match token.as_str() {
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                i += 2;
            }
            "--" => {
                i += 1;
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--work-tree=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => {
                i += 1;
            }
            _ => return Some(i),
        }
    }
    None
}

/// Extracts the repo-relative paths edited in a Cursor `afterFileEdit` event.
///
/// Cursor sends an absolute `file_path` (plus an `edits` array). We strip the
/// resolved `project_root` prefix and normalize to forward slashes so the set
/// can be passed straight to [`TokenSave::sync_if_stale_silent`], which does a
/// targeted single-file sync instead of a full-tree scan. Paths outside the
/// project root are skipped.
pub fn cursor_after_file_edit_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };

    let mut abs_paths: Vec<String> = Vec::new();
    if let Some(p) = parsed
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        abs_paths.push(p.to_string());
    }
    // Defensive: some edit payloads may carry per-edit file paths.
    if let Some(edits) = parsed.get("edits").and_then(Value::as_array) {
        for edit in edits {
            if let Some(p) = edit
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                abs_paths.push(p.to_string());
            }
        }
    }

    let mut rels: Vec<String> = Vec::new();
    for abs in abs_paths {
        if let Some(rel) = rel_under_root(project_root, Path::new(&abs)) {
            if !rels.contains(&rel) {
                rels.push(rel);
            }
        }
    }
    rels
}

fn rel_under_root(root: &Path, abs: &Path) -> Option<String> {
    let stripped = abs.strip_prefix(root).ok()?;
    if stripped.as_os_str().is_empty() {
        return None;
    }
    Some(stripped.to_string_lossy().replace('\\', "/"))
}

/// Returns `true` when a sync should run given the last marker time and a
/// debounce window. Used to coalesce back-to-back `afterShellExecution` syncs.
pub fn cursor_should_run_sync(now_secs: i64, last_secs: Option<i64>, debounce_secs: i64) -> bool {
    match last_secs {
        Some(last) => now_secs - last >= debounce_secs,
        None => true,
    }
}

/// Builds the `sessionStart` `additional_context` text: steer the agent toward
/// tokensave MCP tools and report index freshness for the workspace.
pub fn build_cursor_session_context(initialized: bool, staleness_hint: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(
        "tokensave is available via MCP. Prefer tokensave MCP tools \
         (tokensave_context, tokensave_search, tokensave_callers, tokensave_callees, \
         tokensave_impact, tokensave_files, tokensave_affected) over broad file reads \
         or shell search for codebase exploration, symbol lookup, call graphs, and \
         impact analysis. Fall back to file reads only when tokensave cannot answer.\n",
    );
    if initialized {
        match staleness_hint {
            Some(hint) => s.push_str(&format!("Index status: {hint}.\n")),
            None => s.push_str("Index status: initialized.\n"),
        }
    } else {
        s.push_str(
            "Index status: no .tokensave/ index found in this workspace — \
             run `tokensave init` to enable tokensave tools.\n",
        );
    }
    s
}

/// Formats a short relative-age staleness hint from a sync age in seconds.
pub fn cursor_staleness_hint(age_secs: i64) -> String {
    let age = age_secs.max(0);
    if age < 60 {
        "last indexed just now".to_string()
    } else if age < 3_600 {
        format!("last indexed {}m ago", age / 60)
    } else if age < 86_400 {
        format!("last indexed {}h ago", age / 3_600)
    } else {
        format!("last indexed {}d ago", age / 86_400)
    }
}

/// Builds the Cursor `sessionStart` output JSON (`additional_context` + `env`).
/// When `project_root` is known, exposes it as `TOKENSAVE_PROJECT_ROOT` so
/// subsequent session hooks can reuse it.
pub fn cursor_session_start_json(project_root: Option<&Path>, additional_context: &str) -> String {
    let mut env = serde_json::Map::new();
    if let Some(root) = project_root {
        env.insert(
            "TOKENSAVE_PROJECT_ROOT".to_string(),
            Value::String(root.to_string_lossy().to_string()),
        );
    }
    serde_json::json!({
        "additional_context": additional_context,
        "env": Value::Object(env),
    })
    .to_string()
}

async fn cursor_staleness_for_root(root: &Path) -> Option<String> {
    let cg = crate::tokensave::TokenSave::open(root).await.ok()?;
    let last = cg.last_sync_timestamp().await;
    if last <= 0 {
        return None;
    }
    Some(cursor_staleness_hint(now_unix_secs() - last))
}

/// Targeted, fail-open single-file sync for Cursor `afterFileEdit`.
///
/// Resolves the edited repo-relative paths and calls `sync_if_stale_silent`,
/// which avoids the full-tree scan that `sync()` performs. No-ops when the
/// workspace is uninitialized or no in-project paths were edited.
async fn targeted_sync_for_cursor_after_file_edit(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tokensave::TokenSave::is_initialized(&root) {
        return;
    }
    let rels = cursor_after_file_edit_rel_paths(event_json, &root);
    if rels.is_empty() {
        return;
    }
    if let Ok(cg) = crate::tokensave::TokenSave::open(&root).await {
        let _ = cg.sync_if_stale_silent(&rels).await;
    }
}

/// Branch-aware, fail-open handler for git state-changing shell commands.
///
/// Branch switches (`checkout`/`switch`/`worktree add`) bootstrap/maintain
/// tokensave branch tracking via [`crate::branch::add_branch_tracking`] —
/// which is idempotent and supersedes a plain sync. Other state-changing
/// commands (pull/merge/rebase/reset/cherry-pick/stash apply|pop) run a full
/// incremental `sync()`, coalesced by a short marker-based guard so back-to-back
/// git commands don't stack. Only acts when `.tokensave/` already exists.
async fn sync_after_cursor_shell_event(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let command = parsed
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(cursor_shell_sync_plan(command), CursorShellSyncPlan::Noop) {
        return;
    }
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    // Never bootstrap indexing in an unindexed repo.
    if !crate::tokensave::TokenSave::is_initialized(&root) {
        return;
    }
    let current_branch = crate::branch::current_branch(&root);
    let plan = cursor_shell_sync_plan_with_current_branch(command, current_branch.as_deref());

    match plan {
        CursorShellSyncPlan::BranchAdd(branch) => {
            run_branch_tracking_or_sync(&root, &branch, ".cursor_shell_sync_at").await;
        }
        CursorShellSyncPlan::CurrentBranchSync(branch) => {
            run_branch_tracking_or_sync(&root, &branch, ".cursor_shell_sync_at").await;
        }
        CursorShellSyncPlan::IncrementalSync => {
            run_coalesced_incremental_sync(&root, ".cursor_shell_sync_at").await;
        }
        CursorShellSyncPlan::Noop => {}
    }
}

async fn run_branch_tracking_or_sync(root: &Path, branch: &str, marker_file: &str) {
    match crate::branch::add_branch_tracking(root, branch).await {
        Ok(
            crate::branch::BranchAddOutcome::Added | crate::branch::BranchAddOutcome::NotIndexed,
        ) => {}
        Ok(crate::branch::BranchAddOutcome::AlreadyTracked) => {
            run_coalesced_incremental_sync(root, marker_file).await;
        }
        Err(_) => {}
    }
}

/// Runs a full incremental `sync()`, coalescing back-to-back invocations via a
/// short marker-based debounce so a burst of git commands doesn't stack syncs.
/// `marker_file` names the per-agent marker inside the `.tokensave/` dir. The
/// sync lock additionally no-ops genuinely concurrent runs. Fail-open.
async fn run_coalesced_incremental_sync(root: &Path, marker_file: &str) {
    let marker = crate::config::get_tokensave_dir(root).join(marker_file);
    let now = now_unix_secs();
    if !cursor_should_run_sync(now, read_marker_secs(&marker), 3) {
        return;
    }
    write_marker_secs(&marker, now);

    if let Ok(cg) = crate::tokensave::TokenSave::open(root).await {
        match cg.sync().await {
            Ok(_) | Err(crate::errors::TokenSaveError::SyncLock { .. }) => {}
            Err(e) => eprintln!("tokensave sync failed: {e}"),
        }
    }
}

/// Branch-aware workspace catch-up for Cursor `workspaceOpen`.
///
/// When the workspace has a tokensave index, ensures the current branch's DB
/// exists (branch-add if missing — which also syncs) and otherwise runs a
/// catch-up incremental `sync()`. Idempotent and fail-open.
async fn workspace_open_for_cursor_event(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tokensave::TokenSave::is_initialized(&root) {
        return;
    }

    // Ensure the current branch is tracked. When a branch is freshly added,
    // `add_branch_tracking` already runs a sync, so we can skip the catch-up.
    if let Some(branch) = crate::branch::current_branch(&root) {
        if let Ok(crate::branch::BranchAddOutcome::Added) =
            crate::branch::add_branch_tracking(&root, &branch).await
        {
            return;
        }
    }

    let _ = sync_for_cursor_event(event_json).await;
}

fn read_marker_secs(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

fn write_marker_secs(path: &Path, secs: i64) {
    let _ = std::fs::write(path, secs.to_string());
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Codex CLI hook handlers
//
// Codex sends ONE JSON object on stdin (shared fields: session_id,
// transcript_path, cwd, hook_event_name, model, plus event-specific fields)
// and reads a Codex-shaped JSON object from stdout. These handlers intentionally
// emit Codex's documented output schema (`hookSpecificOutput.additionalContext`
// for steering and soft hints) rather than reusing the Claude / Cursor / Kiro
// output shapes.
// ---------------------------------------------------------------------------

/// Codex `SessionStart` hook handler (fire-and-forget).
///
/// Emits `hookSpecificOutput.additionalContext` steering the agent toward
/// tokensave MCP tools and reporting index freshness for the session `cwd`.
pub async fn hook_codex_session_start() -> i32 {
    let event = read_stdin_to_string();
    let root = codex_project_root_from_event(&event);
    let context = session_steering_context_for_root(root.as_deref()).await;
    println!(
        "{}",
        codex_additional_context_json("SessionStart", &context)
    );
    0
}

/// Codex `UserPromptSubmit` hook handler.
///
/// Resets the per-project local counter for the new turn and injects the same
/// tokensave steering context as `SessionStart`. Never blocks the prompt.
pub async fn hook_codex_user_prompt_submit() -> i32 {
    let event = read_stdin_to_string();
    let root = codex_project_root_from_event(&event);
    reset_counter_for_codex_event(&event).await;
    let context = session_steering_context_for_root(root.as_deref()).await;
    println!(
        "{}",
        codex_additional_context_json("UserPromptSubmit", &context)
    );
    0
}

/// Codex `PreToolUse` hook handler.
///
/// Emits nonblocking `hookSpecificOutput.additionalContext` soft hints for
/// high-confidence code research tools. It never denies, rewrites, or blocks
/// the tool call, and invalid/unknown events fail open with no output.
pub fn hook_codex_pre_tool_use() -> i32 {
    let event = read_stdin_to_string();
    if let Some(output) = evaluate_codex_pre_tool_use(&event) {
        println!("{output}");
    }
    0
}

/// Codex `SubagentStart` hook handler.
///
/// Steers research/explore subagents toward tokensave MCP tools. Codex cannot
/// hard-stop a subagent at start (`continue: false` is ignored for this event),
/// so this injects `additionalContext` instead of denying.
pub fn hook_codex_subagent_start() -> i32 {
    let event = read_stdin_to_string();
    if let Some(output) = evaluate_codex_subagent_start(&event) {
        println!("{output}");
    }
    0
}

/// Codex `PostToolUse` hook handler used to keep the graph fresh after writes.
///
/// For `apply_patch` edits this runs a **targeted** single-file sync using the
/// paths parsed from the patch envelope (never a full-tree scan). For `Bash`
/// commands it reuses the shared git-command classifier: branch switches
/// bootstrap branch tracking, other state-changing commands run a coalesced
/// incremental sync. Fail-open and silent.
pub async fn hook_codex_post_tool_use() -> i32 {
    let event = read_stdin_to_string();
    codex_post_tool_use(&event).await;
    0
}

/// Builds a Codex hook stdout payload that injects model-visible context via
/// `hookSpecificOutput.additionalContext`. Used by `SessionStart`,
/// `UserPromptSubmit`, and `SubagentStart`.
pub fn codex_additional_context_json(event_name: &str, additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

/// Pure decision logic for Codex `PreToolUse` hook events.
pub fn evaluate_codex_pre_tool_use(event_json: &str) -> Option<String> {
    let mut dedupe = ToolHintDedupe::default();
    evaluate_codex_pre_tool_use_with_dedupe(event_json, &mut dedupe)
}

/// Pure Codex `PreToolUse` decision logic with caller-provided dedupe state.
pub fn evaluate_codex_pre_tool_use_with_dedupe(
    event_json: &str,
    dedupe: &mut ToolHintDedupe,
) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let input = codex_pre_tool_hint_input(&parsed);
    codex_tool_hint_output("PreToolUse", &input, dedupe)
}

fn codex_pre_tool_hint_input(event: &Value) -> ToolHintInput {
    tool_hint_input_from_pre_tool_event(HintAgent::Codex, event)
}

fn codex_tool_hint_output(
    event_name: &str,
    input: &ToolHintInput,
    dedupe: &mut ToolHintDedupe,
) -> Option<String> {
    let hint = decide_hint(input)?;
    if !dedupe.should_emit(hint_session_id(input), hint.category) {
        return None;
    }
    Some(codex_additional_context_json(
        event_name,
        &tool_hint_context(&hint),
    ))
}

/// Pure decision logic for Codex `SubagentStart` events.
///
/// Returns a Codex `additionalContext` payload steering research/explore
/// subagents toward tokensave MCP tools, or `None` for execution-style
/// subagents. Inspects `agent_type` (Codex's documented field) and any
/// prompt/task/description text.
pub fn evaluate_codex_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let input = ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: Some("SubagentStart".to_string()),
        prompt: event_text_field(&parsed, &["prompt", "task", "description"]),
        subagent_type: event_text_field(&parsed, &["agent_type", "subagent_type"]),
        hints_enabled: true,
        ..ToolHintInput::default()
    };
    let mut dedupe = ToolHintDedupe::default();
    codex_tool_hint_output("SubagentStart", &input, &mut dedupe)
}

/// Resolves the tokensave project root for a Codex event from its `cwd`.
pub fn codex_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let cwd = event_cwd(event_json)?;
    crate::config::discover_project_root(&cwd)
}

/// Extracts the project-relative paths touched by a Codex `apply_patch` command.
///
/// Codex sends the patch text as `tool_input.command`. The `apply_patch` envelope
/// names each file with `*** Add File:`, `*** Update File:`, `*** Delete File:`,
/// or `*** Move to:` lines. Patch paths are relative to the session `cwd`
/// (which may be a subdirectory of the discovered project root), so we resolve
/// each against `cwd` and then make it relative to `project_root`. Absolute
/// paths outside the root are skipped. The result feeds the targeted
/// [`TokenSave::sync_if_stale_silent`] single-file sync.
pub fn codex_apply_patch_rel_paths(command: &str, cwd: &Path, project_root: &Path) -> Vec<String> {
    const PREFIXES: [&str; 4] = [
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut rels: Vec<String> = Vec::new();
    for line in command.lines() {
        let line = line.trim();
        for prefix in PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                let raw = rest.trim();
                if raw.is_empty() {
                    continue;
                }
                let candidate = Path::new(raw);
                let abs = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    cwd.join(candidate)
                };
                if let Some(rel) = rel_under_root(project_root, &abs) {
                    if !rels.contains(&rel) {
                        rels.push(rel);
                    }
                }
            }
        }
    }
    rels
}

fn is_codex_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "apply_patch" | "edit" | "write"
    )
}

fn is_codex_bash_tool(tool_name: &str) -> bool {
    matches!(tool_name.to_ascii_lowercase().as_str(), "bash" | "shell")
}

async fn codex_post_tool_use(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let tool_name = parsed
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let command = parsed
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(cwd) = event_cwd(event_json) else {
        return;
    };
    let Some(root) = crate::config::discover_project_root(&cwd) else {
        return;
    };
    // Never bootstrap indexing in an unindexed repo.
    if !crate::tokensave::TokenSave::is_initialized(&root) {
        return;
    }

    if is_codex_edit_tool(tool_name) {
        let rels = codex_apply_patch_rel_paths(command, &cwd, &root);
        if rels.is_empty() {
            return;
        }
        if let Ok(cg) = crate::tokensave::TokenSave::open(&root).await {
            let _ = cg.sync_if_stale_silent(&rels).await;
        }
    } else if is_codex_bash_tool(tool_name) {
        let current_branch = crate::branch::current_branch(&root);
        match cursor_shell_sync_plan_with_current_branch(command, current_branch.as_deref()) {
            CursorShellSyncPlan::BranchAdd(branch) => {
                run_branch_tracking_or_sync(&root, &branch, ".codex_shell_sync_at").await;
            }
            CursorShellSyncPlan::CurrentBranchSync(branch) => {
                run_branch_tracking_or_sync(&root, &branch, ".codex_shell_sync_at").await;
            }
            CursorShellSyncPlan::IncrementalSync => {
                run_coalesced_incremental_sync(&root, ".codex_shell_sync_at").await;
            }
            CursorShellSyncPlan::Noop => {}
        }
    }
}

async fn reset_counter_for_codex_event(event_json: &str) {
    let Some(project_root) = codex_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tokensave::TokenSave::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Pure decision logic for Kiro `preToolUse` hook events.
///
/// Returns a block reason only for Kiro delegation/subagent tool calls whose
/// task text looks like codebase research that tokensave MCP tools should
/// answer first.
pub fn evaluate_kiro_pre_tool_use(event_json: &str) -> Option<&'static str> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    if !is_kiro_delegation_tool(tool_name) {
        return None;
    }

    if kiro_event_has_research_text(parsed.get("tool_input").unwrap_or(&Value::Null)) {
        Some(TOKENSAVE_RESEARCH_BLOCK_REASON)
    } else {
        None
    }
}

fn is_kiro_delegation_tool(tool_name: &str) -> bool {
    matches!(tool_name, "delegate" | "subagent" | "use_subagent")
}

fn kiro_event_has_research_text(value: &Value) -> bool {
    let mut text = Vec::new();
    collect_kiro_task_strings(value, &mut text);
    if text.is_empty() {
        collect_strings(value, &mut text);
    }
    text.iter().any(|s| is_code_research_prompt(s))
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

/// `UserPromptSubmit` hook handler: resets the per-session local counter.
///
/// Token savings are now reported inline in each MCP tool response,
/// so this hook only needs to reset the counter for the new turn.
pub async fn hook_prompt_submit() {
    let project_path = crate::config::resolve_path(None);
    if let Ok(cg) = crate::tokensave::TokenSave::open(&project_path).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Kiro `userPromptSubmit` hook handler.
///
/// Kiro adds successful hook stdout to context, so this handler stays silent.
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_stdin_to_string();
    reset_counter_for_kiro_event(&event).await;
    0
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// The installed Kiro agent maps this to `fs_write`. The hook discovers the
/// nearest initialized tokensave project from Kiro's `cwd` field and runs a
/// silent incremental sync. Missing indexes and concurrent syncs are no-ops.
pub async fn hook_kiro_post_tool_use() -> i32 {
    let event = read_stdin_to_string();
    match sync_for_kiro_event(&event).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tokensave sync failed: {e}");
            1
        }
    }
}

async fn reset_counter_for_kiro_event(event_json: &str) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tokensave::TokenSave::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

async fn reset_counter_for_cursor_event(event_json: &str) {
    let Some(project_root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tokensave::TokenSave::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

async fn sync_for_kiro_event(event_json: &str) -> crate::errors::Result<()> {
    let Some(project_root) = kiro_project_root(event_json) else {
        return Ok(());
    };
    let cg = crate::tokensave::TokenSave::open(&project_root).await?;
    match cg.sync().await {
        Ok(_) | Err(crate::errors::TokenSaveError::SyncLock { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn sync_for_cursor_event(event_json: &str) -> crate::errors::Result<()> {
    let Some(project_root) = cursor_project_root_from_event(event_json) else {
        return Ok(());
    };
    if !crate::tokensave::TokenSave::is_initialized(&project_root) {
        return Ok(());
    }
    let cg = crate::tokensave::TokenSave::open(&project_root).await?;
    match cg.sync().await {
        Ok(_) | Err(crate::errors::TokenSaveError::SyncLock { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

fn kiro_project_root(event_json: &str) -> Option<PathBuf> {
    let cwd = event_cwd(event_json).or_else(|| std::env::current_dir().ok())?;
    crate::config::discover_project_root(&cwd)
}

/// Reads the `cwd` string field from a hook event JSON payload. Shared by the
/// Kiro and Codex handlers, both of which send the session working directory.
fn event_cwd(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let cwd = parsed.get("cwd").and_then(Value::as_str)?;
    let path = Path::new(cwd);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path.to_path_buf())
    }
}

fn read_stdin_to_string() -> String {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    input
}

/// `Stop` hook handler: ingests new session data and prints a cost receipt.
///
/// Parses any new JSONL lines from Claude Code sessions, inserts them into
/// the global DB, and prints a one-line summary to stderr showing the
/// session cost, tokens saved, and efficiency ratio.
pub async fn hook_stop() {
    let Some(gdb) = crate::global_db::GlobalDb::open().await else {
        return;
    };

    let stats = crate::accounting::parser::ingest(&gdb).await;
    if stats.turns_inserted == 0 {
        return;
    }

    // Read tokens saved for efficiency calculation
    let project_path = crate::config::resolve_path(None);
    let tokens_saved = if let Ok(cg) = crate::tokensave::TokenSave::open(&project_path).await {
        cg.get_tokens_saved().await.unwrap_or(0)
    } else {
        0
    };

    let efficiency = if tokens_saved + stats.tokens_consumed > 0 {
        (tokens_saved as f64 / (tokens_saved + stats.tokens_consumed) as f64) * 100.0
    } else {
        0.0
    };

    let saved_str = crate::display::format_token_count(tokens_saved);

    // Print to stderr so it appears in the terminal but doesn't interfere
    // with stdout (which Claude Code may parse).
    if stats.cost_usd >= 0.001 {
        eprintln!(
            "\x1b[36mSession: ${:.2} spent | {saved_str} saved | {efficiency:.0}% efficiency\x1b[0m",
            stats.cost_usd
        );
    }
}
