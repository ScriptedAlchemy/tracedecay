//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! These functions are invoked by each agent's hook system to intercept tool
//! calls, redirect exploration work to tokensave MCP tools, keep the index
//! fresh after edits / git state changes, and track per-session token savings.
//! Each agent sends its own event schema on stdin and expects its own output
//! shape, so the handlers are kept agent-specific rather than shared blindly.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

pub mod tool_hints;

use tool_hints::{decide_hint, HintAgent, ToolHint, ToolHintInput};

const TOKENSAVE_RESEARCH_BLOCK_REASON: &str = "STOP: Use tokensave MCP tools \
(tokensave_context, tokensave_search, tokensave_callees, tokensave_callers, \
tokensave_impact, tokensave_files, tokensave_affected) instead of agents for \
code research. Tokensave is faster and more precise for symbol relationships, \
call paths, and code structure. Only use agents for code exploration if you \
have already tried tokensave and it cannot answer the question.";

fn research_block_reason(hint: Option<ToolHint>) -> String {
    hint.map_or_else(
        || TOKENSAVE_RESEARCH_BLOCK_REASON.to_string(),
        |hint| {
            format!(
                "{}\n\n{}",
                TOKENSAVE_RESEARCH_BLOCK_REASON,
                format_tool_hint(&hint)
            )
        },
    )
}

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
    let parsed: serde_json::Value =
        serde_json::from_str(tool_input).unwrap_or_else(|_| serde_json::json!({}));
    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Claude,
        session_id: event_session_id(&parsed),
        tool_name: Some("Agent".to_string()),
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: parsed
            .get("subagent_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        file_path: None,
        hints_enabled: true,
    });
    let block_reason = research_block_reason(hint);
    let block_msg = || {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": block_reason
            }
        })
    };

    // Block Explore agents outright
    if parsed.get("subagent_type").and_then(|v| v.as_str()) == Some("Explore") {
        return block_msg().to_string();
    }

    // Check if the prompt is exploration/research work that tokensave can handle
    if let Some(prompt) = parsed.get("prompt").and_then(|v| v.as_str()) {
        if is_code_research_prompt(prompt) {
            return block_msg().to_string();
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
/// Emits nonblocking Cursor-shaped hook-specific context for broad search
/// tools such as Grep and shell `rg` before the model spends a tool call on them.
pub fn hook_cursor_pre_tool_use() -> i32 {
    let event = read_stdin_to_string();
    if let Some(decision) = evaluate_cursor_pre_tool_use(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `beforeSubmitPrompt` hook handler.
///
/// Resets the project-local counter for a new prompt turn and does at most a
/// small, time-boxed *tail* ingest of newly-appended transcript lines (the bulk
/// catch-up lives on the lower-frequency `sessionStart` / `stop` hooks). The
/// output uses Cursor's documented `beforeSubmitPrompt` shape and never blocks
/// submission, even if the tail ingest times out.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_stdin_to_string();
    reset_counter_for_cursor_event(&event).await;
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_HOT_INGEST_MAX_BYTES),
        CURSOR_HOT_INGEST_BUDGET,
    )
    .await;
    let mut output = serde_json::json!({ "continue": true });
    if let Some(hint) = cursor_prompt_hint(&event) {
        output["additional_context"] = Value::String(format_tool_hint(&hint));
    }
    println!("{output}");
    0
}

/// Cursor `stop` hook handler (fire-and-forget).
///
/// Fires at the end of an agent turn and performs the primary transcript
/// ingest: a time-boxed incremental catch-up that picks up bounded transcript
/// tails appended during the turn. The `stop` output is informational only, so
/// we emit an empty object and never ask the agent to continue. Fail-open.
pub async fn hook_cursor_stop() -> i32 {
    let event = read_stdin_to_string();
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
    )
    .await;
    println!("{}", serde_json::json!({}));
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
    // Catch-up ingest for resumed sessions whose transcript grew while no agent
    // was attached. No-op (no transcript_path) for brand-new sessions. Fail-open.
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_SESSION_INGEST_BUDGET,
    )
    .await;
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
/// Returns a Cursor hook response only when a research-oriented subagent should
/// be denied in favor of tokensave MCP tools.
pub fn evaluate_cursor_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let subagent_type = parsed
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let task = parsed
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(&parsed),
        tool_name: Some("subagentStart".to_string()),
        command: None,
        prompt: (!task.is_empty()).then(|| task.to_string()),
        subagent_type: (!subagent_type.is_empty()).then(|| subagent_type.to_string()),
        file_path: None,
        hints_enabled: true,
    });
    let is_explore = subagent_type.eq_ignore_ascii_case("explore");
    if is_explore || is_code_research_prompt(task) {
        return Some(
            serde_json::json!({
                "permission": "deny",
                "user_message": research_block_reason(hint)
            })
            .to_string(),
        );
    }

    None
}

/// Pure decision logic for Cursor `preToolUse` hook events.
///
/// Returns a soft native Cursor `additional_context` hint only for high-confidence
/// broad search tools. Invalid or unrelated tool events fail open with no output.
pub fn evaluate_cursor_pre_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_pre_tool_hint_input(&parsed))?;
    Some(
        serde_json::json!({
            "continue": true,
            "additional_context": format_tool_hint(&hint),
        })
        .to_string(),
    )
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
/// non-git commands return `false`. Only commands whose first token is `git`
/// match, so `echo git checkout` is ignored.
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
            matches!(after.as_deref(), Some("pop" | "apply"))
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
/// Path checkouts (`git checkout -- <file>` or obvious file pathspecs) and
/// non-switch commands return `None`. Only commands whose first token is `git`
/// are considered.
pub fn cursor_branch_switch_target(command: &str) -> Option<String> {
    let raw = shell_words(command);
    let sub_pos = git_subcommand_pos(&raw)?;
    let sub = raw[sub_pos].to_ascii_lowercase();

    match sub.as_str() {
        "checkout" | "switch" => {
            // Path checkout (`git checkout -- file`) is not a branch switch.
            let after = &raw[sub_pos + 1..];
            let mut iter = after.iter();
            while let Some(tok) = iter.next() {
                if tok == "--" {
                    return None;
                }
                if matches!(tok.as_str(), "-b" | "-B" | "-c" | "-C") {
                    return iter.find(|t| !t.starts_with('-')).cloned();
                }
                if tok.starts_with('-') {
                    continue;
                }
                if is_obvious_checkout_pathspec(tok) {
                    return None;
                }
                return Some(tok.clone());
            }
            None
        }
        "worktree" => {
            let lower: Vec<String> = raw.iter().map(|t| t.to_ascii_lowercase()).collect();
            let add_pos = lower.iter().position(|t| t == "add")?;
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
                .map(String::as_str)
                .filter(|t| !t.starts_with('-'))
                .collect();
            positionals.get(1).map(|b| (*b).to_string())
        }
        _ => None,
    }
}

fn is_obvious_checkout_pathspec(token: &str) -> bool {
    token == "."
        || token == ":/"
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with(":/")
        || token
            .rsplit_once('.')
            .is_some_and(|(_, ext)| !ext.is_empty())
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

pub fn cursor_shell_command_targets_project(
    command: &str,
    cwd: &Path,
    project_root: &Path,
) -> bool {
    let tokens = shell_words(command);
    if !tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("git"))
    {
        return true;
    }
    let Some(work_dir) = git_explicit_work_dir(&tokens, cwd) else {
        return true;
    };
    let target_root = crate::config::discover_project_root(&work_dir).unwrap_or(work_dir);
    paths_same(&target_root, project_root)
}

fn git_explicit_work_dir(tokens: &[String], cwd: &Path) -> Option<PathBuf> {
    let mut i = 1;
    while i < tokens.len() {
        let token = &tokens[i];
        match token.as_str() {
            "-C" | "--work-tree" => {
                let value = tokens.get(i + 1)?;
                return Some(resolve_shell_path(cwd, value));
            }
            "-c" | "--git-dir" | "--namespace" | "--config-env" => i += 2,
            _ if token.starts_with("--work-tree=") => {
                let value = token.trim_start_matches("--work-tree=");
                return Some(resolve_shell_path(cwd, value));
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => i += 1,
            _ => break,
        }
    }
    None
}

fn resolve_shell_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
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
    let plan = cursor_shell_sync_plan(command);
    if matches!(plan, CursorShellSyncPlan::Noop) {
        return;
    }
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    // Never bootstrap indexing in an unindexed repo.
    if !crate::tokensave::TokenSave::is_initialized(&root) {
        return;
    }

    match plan {
        CursorShellSyncPlan::BranchAdd(branch) => {
            // Idempotent + fail-open: already-tracked branches no-op.
            let _ = crate::branch::add_branch_tracking(&root, &branch).await;
        }
        CursorShellSyncPlan::IncrementalSync => {
            run_coalesced_incremental_sync(&root, ".cursor_shell_sync_at").await;
        }
        CursorShellSyncPlan::CurrentBranchSync(branch) => {
            if !matches!(
                crate::branch::add_branch_tracking(&root, &branch).await,
                Ok(crate::branch::BranchAddOutcome::Added)
            ) {
                run_coalesced_incremental_sync(&root, ".cursor_shell_sync_at").await;
            }
        }
        CursorShellSyncPlan::Noop => {}
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
// for steering, `hookSpecificOutput.permissionDecision` for PreToolUse) rather
// than reusing the Claude / Cursor / Kiro output shapes.
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
    let mut context = session_steering_context_for_root(root.as_deref()).await;
    if let Some(hint) = codex_prompt_hint(&event) {
        append_tool_hint(&mut context, &hint);
    }
    println!(
        "{}",
        codex_additional_context_json("UserPromptSubmit", &context)
    );
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

/// Pure decision logic for Codex `SubagentStart` events.
///
/// Returns a Codex `additionalContext` payload steering research/explore
/// subagents toward tokensave MCP tools, or `None` for execution-style
/// subagents. Inspects `agent_type` (Codex's documented field) and any
/// prompt/task/description text.
pub fn evaluate_codex_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let task = parsed
        .get("prompt")
        .or_else(|| parsed.get("task"))
        .or_else(|| parsed.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: Some("SubagentStart".to_string()),
        command: None,
        prompt: (!task.is_empty()).then(|| task.to_string()),
        subagent_type: (!agent_type.is_empty()).then(|| agent_type.to_string()),
        file_path: None,
        hints_enabled: true,
    });
    let is_explore = agent_type.eq_ignore_ascii_case("explore");
    if is_explore || is_code_research_prompt(task) {
        let context = research_block_reason(hint);
        return Some(codex_additional_context_json("SubagentStart", &context));
    }
    None
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
        match cursor_shell_sync_plan(command) {
            CursorShellSyncPlan::BranchAdd(branch) => {
                // Idempotent + fail-open: already-tracked branches no-op.
                let _ = crate::branch::add_branch_tracking(&root, &branch).await;
            }
            CursorShellSyncPlan::IncrementalSync => {
                run_coalesced_incremental_sync(&root, ".codex_shell_sync_at").await;
            }
            CursorShellSyncPlan::CurrentBranchSync(branch) => {
                if !matches!(
                    crate::branch::add_branch_tracking(&root, &branch).await,
                    Ok(crate::branch::BranchAddOutcome::Added)
                ) {
                    run_coalesced_incremental_sync(&root, ".codex_shell_sync_at").await;
                }
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

/// Largest tail the `beforeSubmitPrompt` hot path will read in one call. Larger
/// backlogs are left for the `sessionStart` / `stop` catch-up ingests.
const CURSOR_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Largest transcript tail a low-priority Cursor catch-up hook will read.
/// Oversized backlogs stay queued instead of blocking hook execution.
const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Hard wall-clock budget for the `beforeSubmitPrompt` tail ingest. Well under
/// Cursor's 5s hook timeout; on expiry we fail open and let heavier hooks catch up.
const CURSOR_HOT_INGEST_BUDGET: Duration = Duration::from_millis(1_500);
/// Budget for the `sessionStart` catch-up ingest (registered with a 5s timeout).
const CURSOR_SESSION_INGEST_BUDGET: Duration = Duration::from_secs(4);
/// Budget for the end-of-turn `stop` catch-up ingest (registered with a 30s timeout).
const CURSOR_STOP_INGEST_BUDGET: Duration = Duration::from_secs(25);

/// Incrementally ingests the Cursor transcript referenced by `event_json` into
/// the project-local session DB, bounded by `max_new_bytes` (the hot-path cap)
/// and an overall `budget`. Always fails open: a timeout, missing transcript, or
/// any error is swallowed so the calling hook never blocks the agent.
async fn ingest_cursor_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: Duration,
) {
    let work = async {
        let Some(project_root) = cursor_project_root_from_event(event_json) else {
            return;
        };
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return;
        };
        let _ = crate::sessions::cursor::ingest_cursor_transcript_event_capped(
            event_json,
            &db,
            max_new_bytes,
        )
        .await;
    };
    // Short-lived CLI hook processes exit immediately, so the ingest must run
    // inline (not on a detached task); the timeout keeps it inside budget.
    let _ = tokio::time::timeout(budget, work).await;
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

fn cursor_prompt_hint(event_json: &str) -> Option<ToolHint> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    decide_hint(&ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(&parsed),
        tool_name: None,
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: None,
        file_path: parsed
            .get("file_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        hints_enabled: true,
    })
}

fn cursor_pre_tool_hint_input(parsed: &Value) -> ToolHintInput {
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(parsed),
        tool_name: text_field(parsed, &["tool_name", "toolName", "name"]),
        command: text_field(tool_input, &["command", "cmd"])
            .or_else(|| text_field(parsed, &["command", "cmd"])),
        prompt: text_field(
            tool_input,
            &["prompt", "query", "pattern", "task", "description"],
        )
        .or_else(|| {
            text_field(
                parsed,
                &["prompt", "query", "pattern", "task", "description"],
            )
        }),
        subagent_type: text_field(parsed, &["subagent_type", "subagentType", "agent_type"]),
        file_path: text_field(tool_input, &["file_path", "filePath", "path"])
            .or_else(|| text_field(parsed, &["file_path", "filePath", "path"])),
        hints_enabled: true,
    }
}

fn codex_prompt_hint(event_json: &str) -> Option<ToolHint> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: None,
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: None,
        file_path: None,
        hints_enabled: true,
    })
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn prompt_like_text(parsed: &Value) -> Option<String> {
    [
        "prompt",
        "user_prompt",
        "message",
        "input",
        "task",
        "description",
    ]
    .iter()
    .find_map(|key| parsed.get(*key).and_then(Value::as_str))
    .filter(|text| !text.is_empty())
    .map(str::to_string)
}

fn event_session_id(parsed: &Value) -> Option<String> {
    ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn format_tool_hint(hint: &ToolHint) -> String {
    format!("tokensave hint: {}\n{}", hint.message, hint.context)
}

fn append_tool_hint(context: &mut String, hint: &ToolHint) {
    if !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(&format_tool_hint(hint));
    context.push('\n');
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
