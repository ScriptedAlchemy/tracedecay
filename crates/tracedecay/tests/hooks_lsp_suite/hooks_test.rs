use std::path::Path;
use tracedecay_agent_hosts::hooks::{
    HookWorkspaceStatus, additional_context_json, build_cursor_session_context,
    codex_apply_patch_rel_paths, codex_project_root_from_event, codex_subagent_start_log_line,
    codex_user_prompt_submit_context_for_event, codex_workspace_status_from_event,
    cursor_project_root_from_event, cursor_session_start_json, cursor_staleness_hint,
    evaluate_codex_subagent_start, evaluate_cursor_subagent_start, evaluate_hook_decision,
    evaluate_kiro_pre_tool_use, kiro_post_tool_use_rel_paths, record_codex_subagent_start,
};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::storage::{pin_fixture_repository_identity, resolve_layout};

const RESEARCH_BLOCK_REASON: &str = "STOP: Use tracedecay MCP tools \
(tracedecay_context, tracedecay_grep, tracedecay_search, tracedecay_callees, \
tracedecay_callers, tracedecay_impact, tracedecay_files, tracedecay_affected) \
instead of agents for code research. Route literal/regex text to tracedecay_grep, \
symbol names to tracedecay_search, and concepts to tracedecay_context. TraceDecay \
is faster and more precise for symbol relationships, call paths, and code structure. \
Only use agents for code exploration if you have already tried tracedecay and it \
cannot answer the question.";

const EXPLORE_SUBAGENT_HINT: &str = "tracedecay hint: For code research subagents, consider adding tracedecay MCP context before broad exploration.\n\
tracedecay_context can gather focused code context, while tracedecay_search, tracedecay_callers, and tracedecay_impact can answer common research questions without a broad scan.\n\
Skill: tracedecay:discovering-tracedecay.";

const PROJECT_CONTEXT_HINT: &str = "tracedecay hint: For other repos or registered projects, consider TraceDecay project registry tools.\n\
tracedecay_project_list shows known projects; tracedecay_project_search can find a sibling repo by name/path/remote; pass project_path or project_id to tracedecay_context or tracedecay_search for cross-project code context before scanning parent directories.\n\
Skill: tracedecay:code-health.";

const CODEX_SUBAGENT_CONTEXT: &str = "TraceDecay context for this new or code-research \
subagent: when the task needs unfamiliar code context, use tracedecay_context for concepts, \
tracedecay_search for symbols, tracedecay_grep for literal or regex text, and \
tracedecay_callers/callees or tracedecay_impact for relationships. Use native reads for known \
files. Load a tracedecay skill only when its specific workflow matches the task; use \
tracedecay_message_search or tracedecay_lcm_expand_query when prior conversation context matters.";

const CLAUDE_EXPLORE: &str = r#"{"subagent_type": "Explore", "prompt": "find files"}"#;
const CLAUDE_GENERAL: &str =
    r#"{"subagent_type": "general-purpose", "prompt": "write a function"}"#;
const CLAUDE_EXPLORE_BARE: &str = r#"{"subagent_type": "Explore"}"#;
const CLAUDE_RESEARCH_PROMPT: &str =
    r#"{"prompt": "Explore the codebase and find all API endpoints"}"#;
const CLAUDE_ARCHITECTURE_PROMPT: &str = r#"{"prompt": "EXPLORE the Codebase Architecture"}"#;
const CLAUDE_IMPLEMENT_PROMPT: &str = r#"{"prompt": "write a function that adds two numbers"}"#;
const KIRO_RESEARCH: &str = r#"{
    "hook_event_name": "preToolUse",
    "tool_name": "delegate",
    "tool_input": { "task": "Explore the codebase architecture and call graph" }
}"#;
const KIRO_EXECUTE: &str = r#"{
    "hook_event_name": "preToolUse",
    "tool_name": "delegate",
    "tool_input": { "task": "Run the full test suite and report failures" }
}"#;
const KIRO_READ: &str = r#"{
    "hook_event_name": "preToolUse",
    "tool_name": "read",
    "tool_input": { "prompt": "Explore the entire codebase" }
}"#;
const CODEX_EXPLORE: &str = r#"{
    "hook_event_name": "SubagentStart",
    "agent_type": "explore",
    "cwd": "/tmp/x"
}"#;
const CODEX_EXECUTE: &str = r#"{
    "hook_event_name": "SubagentStart",
    "agent_type": "generalPurpose",
    "prompt": "Run the test suite and summarize failures"
}"#;

fn claude_deny(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
    .to_string()
}

fn claude_explore_deny() -> String {
    claude_deny(&format!(
        "{RESEARCH_BLOCK_REASON}\n\n{EXPLORE_SUBAGENT_HINT}"
    ))
}

fn claude_project_context_deny() -> String {
    claude_deny(&format!(
        "{RESEARCH_BLOCK_REASON}\n\n{PROJECT_CONTEXT_HINT}"
    ))
}

fn kiro_project_context_block() -> String {
    format!("{RESEARCH_BLOCK_REASON}\n\n{PROJECT_CONTEXT_HINT}")
}

fn assert_hook_decision(input: &str, expected: &str) {
    assert_eq!(evaluate_hook_decision(input), expected);
}

fn assert_claude_explore_denied() {
    assert_hook_decision(CLAUDE_EXPLORE, &claude_explore_deny());
}

fn assert_kiro_research_blocked() {
    assert_eq!(
        evaluate_kiro_pre_tool_use(KIRO_RESEARCH),
        Some(kiro_project_context_block())
    );
}

fn assert_codex_explore_redirect(profile: &ProfileRoot) {
    assert_eq!(
        evaluate_codex_subagent_start(profile, CODEX_EXPLORE),
        Some(codex_subagent_redirect(false))
    );
}

fn codex_subagent_redirect(no_history: bool) -> String {
    let hint = format!(
        "tracedecay hint: For Codex subagents, add compact TraceDecay context before isolated work.\n{CODEX_SUBAGENT_CONTEXT}"
    );
    let mut context = String::new();
    if no_history {
        context.push_str("new/no-history subagent: recover only relevant project memory or prior-session context before assuming missing decisions.\n");
    }
    context.push_str(CODEX_SUBAGENT_CONTEXT);
    context.push_str("\n\n");
    context.push_str(&hint);
    context.push('\n');
    additional_context_json("SubagentStart", &context)
}

fn read_hook_analytics_events(root: &Path) -> Vec<serde_json::Value> {
    let path = root.join("hook_analytics.jsonl");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn analytics_contains(events: &[serde_json::Value], event: &str, category: Option<&str>) -> bool {
    events.iter().any(|item| {
        item["event"].as_str() == Some(event)
            && category.is_none_or(|category| item["category"].as_str() == Some(category))
    })
}

fn enroll_profile_project(project_root: &Path, project_id: &str) {
    pin_fixture_repository_identity(project_root, project_id).unwrap();
}

fn scratch_profile() -> (tempfile::TempDir, ProfileRoot) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::new(dir.path());
    (dir, profile)
}

struct EnrolledCodex {
    _project: tempfile::TempDir,
    _profile_dir: tempfile::TempDir,
    project_root: std::path::PathBuf,
    profile: ProfileRoot,
}

fn enrolled_codex(project_id: &str) -> EnrolledCodex {
    let project = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, project_id);
    let layout = resolve_layout(&project_root, profile.data_dir()).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();
    EnrolledCodex {
        _project: project,
        _profile_dir: profile_dir,
        project_root,
        profile,
    }
}

/// The composition root's hook runtime handle for `profile`, built explicitly
/// for each fixture. Hook and host behavior lives in `tracedecay-agent-hosts`,
/// which reaches registered project identity and the canonical store layout
/// only through this handle; no slot exists for a test binary to leave empty.
fn hook_runtime(
    profile: &ProfileRoot,
) -> tracedecay_agent_hosts::ports::hook_runtime::HookRuntimeV1 {
    tracedecay::hook_runtime(profile.clone())
}

/// A profile for marker-enrolled fixtures whose home sits beside, never at,
/// the fixture checkout.
fn fixture_profile(dir: &Path) -> ProfileRoot {
    ProfileRoot::under_home(dir.join("home"))
}

#[test]
fn test_blocks_explore_agent() {
    assert_claude_explore_denied();
}

#[test]
fn test_allows_non_explore_agent() {
    assert_hook_decision(CLAUDE_GENERAL, "");
    assert_claude_explore_denied();
}

#[test]
fn test_blocks_exploration_prompt_explore() {
    assert_hook_decision(CLAUDE_RESEARCH_PROMPT, &claude_deny(RESEARCH_BLOCK_REASON));
    assert_hook_decision(CLAUDE_IMPLEMENT_PROMPT, "");
}

#[test]
fn test_allows_invalid_json() {
    assert_hook_decision("not json at all", "");
    assert_hook_decision(CLAUDE_EXPLORE_BARE, &claude_explore_deny());
}

#[test]
fn test_case_insensitive_blocking() {
    assert_hook_decision(CLAUDE_ARCHITECTURE_PROMPT, &claude_project_context_deny());
    assert_hook_decision(CLAUDE_IMPLEMENT_PROMPT, "");
}

#[test]
fn test_block_response_uses_correct_hook_schema() {
    assert_hook_decision(CLAUDE_EXPLORE_BARE, &claude_explore_deny());
    assert_hook_decision(CLAUDE_GENERAL, "");
}

#[test]
fn test_kiro_blocks_delegate_code_research_task() {
    assert_kiro_research_blocked();
}

#[test]
fn test_kiro_allows_delegate_execution_task() {
    assert_eq!(evaluate_kiro_pre_tool_use(KIRO_EXECUTE), None);
    assert_kiro_research_blocked();
}

#[test]
fn test_kiro_allows_non_delegation_tool() {
    assert_eq!(evaluate_kiro_pre_tool_use(KIRO_READ), None);
    assert_kiro_research_blocked();
}

#[test]
fn test_kiro_allows_invalid_json() {
    assert_eq!(evaluate_kiro_pre_tool_use("not json"), None);
    assert_kiro_research_blocked();
}

#[test]
fn test_cursor_subagent_start_allows_tracedecay_plugin_agents() {
    // The plugin's own agents are tracedecay-first by construction and must
    // never be denied, even with a research-looking task.
    for subagent_type in [
        "code-explorer",
        "code-health-auditor",
        "session-historian",
        "tracedecay:code-explorer",
        "CodeExplorer",
    ] {
        let input = format!(
            r#"{{
                "hook_event_name": "subagentStart",
                "subagent_type": "{subagent_type}",
                "task": "Explore the codebase architecture and call graph"
            }}"#
        );
        assert_eq!(
            evaluate_cursor_subagent_start(&input),
            None,
            "{subagent_type} stays fail-open"
        );
    }
    let research = r#"{
        "hook_event_name": "subagentStart",
        "subagent_type": "explore",
        "task": "Explore the codebase architecture and call graph"
    }"#;
    assert_eq!(
        evaluate_cursor_subagent_start(research),
        None,
        "cursor subagent start stays fail-open for research agents too"
    );
}

#[test]
fn test_cursor_project_root_uses_workspace_roots() {
    let dir = tempfile::tempdir().unwrap();
    enroll_profile_project(dir.path(), "proj_cursor_workspace_roots");
    let input = format!(
        r#"{{
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": [{}]
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_cursor_project_root_uses_file_path_parent() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    enroll_profile_project(dir.path(), "proj_cursor_file_path");
    let file = src.join("lib.rs");
    let input = format!(
        r#"{{
            "hook_event_name": "afterFileEdit",
            "file_path": {}
        }}"#,
        serde_json::to_string(file.to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_cursor_project_root_prefers_cwd_in_multi_root_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root-a");
    let root_b = dir.path().join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    enroll_profile_project(&root_a, "proj_cursor_root_a");
    enroll_profile_project(&root_b, "proj_cursor_root_b");
    let cwd_b = root_b.join("src");
    std::fs::create_dir_all(&cwd_b).unwrap();

    let input = format!(
        r#"{{
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": [{}, {}],
            "cwd": {},
            "transcript_path": {}
        }}"#,
        serde_json::to_string(root_a.to_str().unwrap()).unwrap(),
        serde_json::to_string(root_b.to_str().unwrap()).unwrap(),
        serde_json::to_string(cwd_b.to_str().unwrap()).unwrap(),
        serde_json::to_string(root_b.join("agent-transcripts/s1.jsonl").to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(root_b)
    );
}

#[test]
fn test_kiro_post_tool_use_rel_paths_targets_written_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let input = format!(
        r#"{{
            "hook_event_name": "postToolUse",
            "tool_name": "fs_write",
            "cwd": {},
            "tool_input": {{
                "path": "src/lib.rs"
            }}
        }}"#,
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );

    assert_eq!(
        kiro_post_tool_use_rel_paths(&input, &root),
        ["src/lib.rs".to_string()]
    );
}

#[test]
fn test_kiro_post_tool_use_rel_paths_skips_paths_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let input = format!(
        r#"{{
            "hook_event_name": "postToolUse",
            "tool_name": "fs_write",
            "cwd": {},
            "tool_input": {{
                "path": "../outside.rs"
            }}
        }}"#,
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );

    let inside = format!(
        r#"{{
            "hook_event_name": "postToolUse",
            "tool_name": "fs_write",
            "cwd": {},
            "tool_input": {{
                "path": "src/lib.rs"
            }}
        }}"#,
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );

    assert_eq!(
        kiro_post_tool_use_rel_paths(&input, &root),
        Vec::<String>::new()
    );
    assert_eq!(
        kiro_post_tool_use_rel_paths(&inside, &root),
        ["src/lib.rs".to_string()]
    );
}

#[test]
fn test_build_cursor_session_context_uninitialized_suggests_init() {
    let context = build_cursor_session_context(false, None, None);
    assert!(context.contains("tracedecay init"));
    assert!(context.contains("tracedecay MCP tools"));
    assert!(
        !context.contains("Workflow skills:"),
        "uninitialized workspaces should not advertise skills: {context}"
    );
}

#[test]
fn test_build_cursor_session_context_initialized_includes_freshness() {
    let context = build_cursor_session_context(true, Some("last indexed 2m ago"), None);
    assert!(
        context.len() <= tracedecay_agent_hosts::hooks::CURSOR_SESSION_CONTEXT_BUDGET,
        "cursor initialized context should stay within its {} char budget, got {} chars: {context}",
        tracedecay_agent_hosts::hooks::CURSOR_SESSION_CONTEXT_BUDGET,
        context.len()
    );
    assert!(context.contains("last indexed 2m ago"));
    assert!(
        !context.contains("tracedecay init"),
        "initialized workspaces should not be told to run init: {context}"
    );
    assert!(context.contains("TraceDecay project hint:"));
    assert!(context.contains("tracedecay_context"));
    assert!(context.contains("tracedecay_search"));
    assert!(context.contains("tracedecay_impact"));
}

#[test]
fn test_build_codex_session_context_carries_compact_steering() {
    let context = tracedecay_agent_hosts::hooks::build_codex_session_context(
        true,
        Some("last indexed 2m ago"),
    );
    assert!(
        context.len() <= tracedecay_agent_hosts::hooks::CODEX_SESSION_CONTEXT_BUDGET,
        "codex initialized context should stay within its {} char budget, got {} chars: {context}",
        tracedecay_agent_hosts::hooks::CODEX_SESSION_CONTEXT_BUDGET,
        context.len()
    );
    assert!(context.contains("TraceDecay project hint:"));
    assert!(context.contains("tracedecay_context"));
    assert!(context.contains("tracedecay_search"));
    assert!(context.contains("tracedecay_impact"));
    assert!(context.contains("last indexed 2m ago"));
    assert!(context.contains("tracedecay_project_search"));
    assert!(context.contains("tracedecay_message_search"));
    assert!(context.contains("tracedecay_fact_store"));
    let uninit = tracedecay_agent_hosts::hooks::build_codex_session_context(false, None);
    assert!(uninit.contains("tracedecay init"));
    assert!(uninit.contains("tracedecay_project_search"));
    assert!(uninit.contains("tracedecay_message_search"));
}

#[test]
fn test_build_codex_session_context_for_generic_workspace_uses_session_guidance() {
    let context = tracedecay_agent_hosts::hooks::build_codex_session_context_for_workspace(
        HookWorkspaceStatus::Generic,
        None,
    );

    assert!(context.contains("TraceDecay session context"));
    assert!(context.contains("tracedecay_lcm_expand_query"));
    assert!(context.contains("tracedecay_message_search"));
    assert!(context.contains("tracedecay_fact_store"));
    assert!(context.contains("Do not store secrets"));
    assert!(
        !context.contains("tracedecay init"),
        "non-project chats should not be told to initialize a code graph: {context}"
    );
    assert!(
        !context.contains("tracedecay_context"),
        "non-project chats should not get code graph steering: {context}"
    );
    assert!(
        !context.contains("code-graph"),
        "non-project chats should not mention code graph setup: {context}"
    );
    assert!(
        !context.contains("repository"),
        "non-project chats should not mention repositories: {context}"
    );
}

#[tokio::test]
async fn test_codex_user_prompt_submit_records_workspace_status_and_missing_session_hint() {
    let project = tempfile::tempdir().unwrap();
    let generic = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, "codex_prompt_analytics");
    let layout = resolve_layout(&project_root, profile.data_dir()).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();

    let generic_event = serde_json::json!({
        "cwd": generic.path(),
        "session_id": "codex-generic-analytics",
        "prompt": "Who calls build_codex_session_context?"
    })
    .to_string();
    let generic_context =
        codex_user_prompt_submit_context_for_event(&hook_runtime(&profile), &generic_event).await;
    // Turn-local steering: a generic workspace still records its workspace
    // status but emits no prompt context.
    assert!(
        generic_context.is_empty(),
        "generic workspaces should emit no prompt steering: {generic_context}"
    );

    let prompt_event = serde_json::json!({
        "cwd": project_root,
        "prompt": "Please explain the impact of changing parse_user"
    })
    .to_string();
    let prompt_context =
        codex_user_prompt_submit_context_for_event(&hook_runtime(&profile), &prompt_event).await;
    assert!(prompt_context.contains("tracedecay hint:"));

    let profile_events = read_hook_analytics_events(&profile_root);
    assert!(profile_events.iter().any(|item| {
        item["event"].as_str() == Some("workspace_status")
            && item["workspace_status"].as_str() == Some("generic")
    }));

    let project_events = read_hook_analytics_events(&layout.data_root);
    assert!(project_events.iter().any(|item| {
        item["event"].as_str() == Some("workspace_status")
            && item["workspace_status"].as_str() == Some("initialized")
    }));
    assert!(analytics_contains(
        &project_events,
        "missing_session",
        Some("impact")
    ));
}

#[test]
fn test_codex_workspace_status_distinguishes_generic_and_project_like_dirs() {
    let profile_dir = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::new(profile_dir.path());
    // Markers under the process temp root are refused so ephemeral agent
    // worktrees are not enrolled as projects. Keep the generic case in TMPDIR
    // (must stay Generic) and place project-like fixtures outside it.
    let generic = tempfile::tempdir().unwrap();
    let generic_event = serde_json::json!({ "cwd": generic.path() }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &generic_event),
        HookWorkspaceStatus::Generic
    );

    // The target directory usually sits inside this checkout, whose
    // repository may be an enrolled TraceDecay project; a fresh repository
    // bounds project discovery so the outcome never depends on the checkout.
    let outside_dir = tempfile::tempdir_in(crate::common::fixture::cargo_target_tmpdir())
        .expect("fixture root outside temp");
    let outside = outside_dir.path();
    gix::init(outside).expect("fixture repository boundary");

    let project_like = outside.join("cargo-marker");
    std::fs::create_dir_all(&project_like).unwrap();
    std::fs::write(project_like.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let project_event = serde_json::json!({ "cwd": project_like }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &project_event),
        HookWorkspaceStatus::UnindexedProject
    );

    let git_like = outside.join("git-marker");
    std::fs::create_dir_all(&git_like).unwrap();
    std::fs::create_dir(git_like.join(".git")).unwrap();
    let nested = git_like.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let git_event = serde_json::json!({ "cwd": nested }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &git_event),
        HookWorkspaceStatus::UnindexedProject
    );
}

#[test]
fn test_codex_workspace_status_detects_initialized_trace_decay_project() {
    let project = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, "codex_workspace_status_initialized");

    let nested = project_root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let event = serde_json::json!({ "cwd": nested }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &event),
        HookWorkspaceStatus::Initialized
    );
}

#[test]
fn test_build_cursor_session_context_lists_skills_and_tokens_saved() {
    let context = build_cursor_session_context(true, None, Some(12_345));
    assert!(context.contains("Workflow skills: tracedecay:"));
    assert!(context.contains("discovering-tracedecay"));
    assert!(context.contains("exploring-code"));
    assert!(context.contains("managing-session-context"));
    assert!(context.contains("12345"));

    let without_savings = build_cursor_session_context(true, None, Some(0));
    assert!(
        !without_savings.contains("Tokens saved"),
        "a zero counter should not be reported: {without_savings}"
    );
}

#[test]
fn test_cursor_staleness_hint_formats_relative_age() {
    assert!(cursor_staleness_hint(0).contains("just"));
    assert!(cursor_staleness_hint(120).contains('m'));
    assert!(cursor_staleness_hint(7_200).contains('h'));
}

#[test]
fn test_cursor_session_start_json_sets_context_and_env_root() {
    let dir = tempfile::tempdir().unwrap();
    let json = cursor_session_start_json(Some(dir.path()), "hello context");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["additional_context"], "hello context");
    assert_eq!(
        v["env"]["TRACEDECAY_PROJECT_ROOT"].as_str(),
        Some(dir.path().to_string_lossy().as_ref())
    );
}

#[test]
fn test_cursor_session_start_json_without_root_omits_env_path() {
    let json = cursor_session_start_json(None, "ctx");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["additional_context"], "ctx");
    assert!(v["env"].get("TRACEDECAY_PROJECT_ROOT").is_none());
}

// ---------------------------------------------------------------------------
// Codex hook handlers
// ---------------------------------------------------------------------------

#[test]
fn test_codex_session_start_context_uses_hook_specific_output() {
    assert_eq!(
        additional_context_json("SessionStart", "hello context"),
        r#"{"hookSpecificOutput":{"additionalContext":"hello context","hookEventName":"SessionStart"}}"#
    );
}

#[test]
fn test_codex_subagent_start_redirects_explore_research_agent() {
    let (_dir, profile) = scratch_profile();
    // Codex SubagentStart cannot hard-stop a subagent (`continue: false` is
    // ignored), so the handler steers it via hookSpecificOutput.additionalContext.
    assert_codex_explore_redirect(&profile);
}

#[test]
fn test_codex_subagent_start_allows_execution_agent() {
    let (_dir, profile) = scratch_profile();
    assert_eq!(evaluate_codex_subagent_start(&profile, CODEX_EXECUTE), None);
    assert_codex_explore_redirect(&profile);
}

#[test]
fn test_codex_subagent_start_allows_invalid_json() {
    let (_dir, profile) = scratch_profile();
    assert_eq!(evaluate_codex_subagent_start(&profile, "not json"), None);
    assert_codex_explore_redirect(&profile);
}

#[test]
fn test_codex_subagent_start_injects_context_for_new_no_history_agent() {
    let (_dir, profile) = scratch_profile();
    let input = r#"{
        "hook_event_name": "SubagentStart",
        "agent_type": "generalPurpose",
        "session_id": "codex-subagent-session-1",
        "is_new": true,
        "has_history": false,
        "prompt": "Implement the fix in the relevant files"
    }"#;

    assert_eq!(
        evaluate_codex_subagent_start(&profile, input),
        Some(codex_subagent_redirect(true))
    );
    assert_eq!(evaluate_codex_subagent_start(&profile, CODEX_EXECUTE), None);
}

#[test]
fn test_codex_subagent_start_dedupes_context_per_session() {
    let fixture = enrolled_codex("codex_subagent_dedupe");
    let profile = &fixture.profile;
    let project_root = &fixture.project_root;
    let input = serde_json::json!({
        "hook_event_name": "SubagentStart",
        "agent_type": "generalPurpose",
        "session_id": "codex-subagent-session-2",
        "cwd": project_root,
        "is_new": true,
        "has_history": false
    })
    .to_string();

    assert_eq!(
        evaluate_codex_subagent_start(profile, &input),
        Some(codex_subagent_redirect(true))
    );
    assert_eq!(
        evaluate_codex_subagent_start(profile, &input),
        None,
        "repeated SubagentStart context should be suppressed per session"
    );
}

#[test]
fn test_codex_subagent_start_no_history_does_not_suppress_later_research_context() {
    let fixture = enrolled_codex("codex_subagent_research_after_no_history");
    let profile = &fixture.profile;
    let project_root = &fixture.project_root;
    let no_history_input = serde_json::json!({
        "hook_event_name": "SubagentStart",
        "agent_type": "generalPurpose",
        "session_id": "codex-subagent-session-research-after-no-history",
        "cwd": project_root,
        "is_new": true,
        "has_history": false,
        "prompt": "Implement the requested fix"
    })
    .to_string();
    let research_input = serde_json::json!({
        "hook_event_name": "SubagentStart",
        "agent_type": "explore",
        "session_id": "codex-subagent-session-research-after-no-history",
        "cwd": project_root,
        "prompt": "Explore the codebase architecture before changing files"
    })
    .to_string();

    assert_eq!(
        evaluate_codex_subagent_start(profile, &no_history_input),
        Some(codex_subagent_redirect(true))
    );
    assert_eq!(
        evaluate_codex_subagent_start(profile, &research_input),
        Some(codex_subagent_redirect(false))
    );
}

#[test]
fn test_codex_subagent_start_counts_and_formats_log_line() {
    let project = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, "codex_subagent_count");
    let input = serde_json::json!({
        "hook_event_name": "SubagentStart",
        "agent_type": "generalPurpose",
        "session_id": "codex-subagent-session-3",
        "cwd": project_root
    })
    .to_string();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| panic!("failed to build tokio runtime: {err}"));
    assert_eq!(
        runtime.block_on(record_codex_subagent_start(&hook_runtime(&profile), &input)),
        Some(1)
    );
    assert_eq!(
        runtime.block_on(record_codex_subagent_start(&hook_runtime(&profile), &input)),
        Some(2)
    );

    let line = codex_subagent_start_log_line(&input, Some(2), true);
    assert!(line.contains("Codex SubagentStart #2"));
    assert!(line.contains("agent_type=generalPurpose"));
    assert!(line.contains("additional_context=true"));

    let layout = resolve_layout(&project_root, profile.data_dir()).unwrap();
    let events = read_hook_analytics_events(&layout.data_root);
    assert!(events.iter().any(|item| {
        item["event"].as_str() == Some("codex_subagent_start")
            && item["count"].as_u64() == Some(1)
            && item["agent_type"].as_str() == Some("generalPurpose")
    }));
    assert!(events.iter().any(|item| {
        item["event"].as_str() == Some("codex_subagent_start")
            && item["count"].as_u64() == Some(2)
            && item["agent_type"].as_str() == Some("generalPurpose")
    }));
}

#[test]
fn test_codex_apply_patch_rel_paths_extracts_patched_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let command = "*** Begin Patch\n\
        *** Update File: src/lib.rs\n\
        @@\n-old\n+new\n\
        *** Add File: src/new_mod.rs\n+contents\n\
        *** Delete File: src/old_mod.rs\n\
        *** End Patch\n";

    let mut rels = codex_apply_patch_rel_paths(command, &root, &root);
    rels.sort();
    assert_eq!(
        rels,
        vec![
            "src/lib.rs".to_string(),
            "src/new_mod.rs".to_string(),
            "src/old_mod.rs".to_string(),
        ]
    );
}

#[test]
fn test_codex_apply_patch_rel_paths_resolves_relative_to_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cwd = root.join("crate_a");
    std::fs::create_dir_all(&cwd).unwrap();
    // apply_patch paths are relative to the session cwd, which may be a
    // subdirectory of the discovered project root.
    let command = "*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch\n";

    let rels = codex_apply_patch_rel_paths(command, &cwd, &root);
    assert_eq!(rels, vec!["crate_a/src/lib.rs".to_string()]);
}

#[test]
fn test_codex_apply_patch_rel_paths_skips_paths_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let command = "*** Begin Patch\n\
        *** Update File: src/lib.rs\n\
        *** Update File: /etc/passwd\n\
        *** End Patch\n";

    assert_eq!(
        codex_apply_patch_rel_paths(command, &root, &root),
        vec!["src/lib.rs".to_string()]
    );
}

#[test]
fn test_codex_project_root_uses_cwd() {
    let dir = tempfile::tempdir().unwrap();
    enroll_profile_project(dir.path(), "proj_codex_cwd");
    let input = format!(
        r#"{{
            "hook_event_name": "PostToolUse",
            "cwd": {}
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        codex_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}
