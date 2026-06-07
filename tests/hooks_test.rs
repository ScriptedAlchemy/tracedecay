use tokensave::hooks::{
    build_cursor_session_context, codex_additional_context_json, codex_apply_patch_rel_paths,
    codex_project_root_from_event, cursor_branch_switch_target, cursor_project_root_from_event,
    cursor_session_start_json, cursor_shell_sync_plan, cursor_should_run_sync,
    cursor_staleness_hint, evaluate_codex_subagent_start, evaluate_cursor_subagent_start,
    evaluate_hook_decision, evaluate_kiro_pre_tool_use, is_git_state_changing_command,
    CursorShellSyncPlan,
};

fn is_blocked(json: &str) -> bool {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v["hookSpecificOutput"]["permissionDecision"].as_str() == Some("deny")
}

fn get_block_reason(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn test_blocks_explore_agent() {
    let input = r#"{"subagent_type": "Explore", "prompt": "find files"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_allows_non_explore_agent() {
    let input = r#"{"subagent_type": "general-purpose", "prompt": "write a function"}"#;
    let result = evaluate_hook_decision(input);
    assert!(result.is_empty(), "allow should produce no output");
}

#[test]
fn test_blocks_exploration_prompt_explore() {
    let input = r#"{"prompt": "Explore the codebase and find all API endpoints"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_codebase_structure_prompt() {
    let input = r#"{"prompt": "Understand the codebase structure"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_call_graph_prompt() {
    let input = r#"{"prompt": "Show me the call graph for this function"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_who_calls_prompt() {
    let input = r#"{"prompt": "who calls the process_data function?"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_callers_of_prompt() {
    let input = r#"{"prompt": "find callers of handle_request"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_callees_of_prompt() {
    let input = r#"{"prompt": "what are the callees of main?"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_symbol_lookup_prompt() {
    let input = r#"{"prompt": "do a symbol lookup for TokenSave"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_read_every_prompt() {
    let input = r#"{"prompt": "read every file in src/"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_blocks_entire_codebase_prompt() {
    let input = r#"{"prompt": "scan the entire codebase for patterns"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_allows_normal_prompt() {
    let input = r#"{"prompt": "write a unit test for the parse function"}"#;
    let result = evaluate_hook_decision(input);
    assert!(result.is_empty(), "allow should produce no output");
}

#[test]
fn test_allows_empty_input() {
    let result = evaluate_hook_decision("");
    assert!(result.is_empty(), "allow should produce no output");
}

#[test]
fn test_allows_invalid_json() {
    let result = evaluate_hook_decision("not json at all");
    assert!(result.is_empty(), "allow should produce no output");
}

#[test]
fn test_allows_no_prompt_no_subagent() {
    let input = r#"{"foo": "bar"}"#;
    let result = evaluate_hook_decision(input);
    assert!(result.is_empty(), "allow should produce no output");
}

#[test]
fn test_case_insensitive_blocking() {
    let input = r#"{"prompt": "EXPLORE the Codebase Architecture"}"#;
    let result = evaluate_hook_decision(input);
    assert!(is_blocked(&result));
}

#[test]
fn test_block_response_has_reason() {
    let input = r#"{"subagent_type": "Explore"}"#;
    let result = evaluate_hook_decision(input);
    let reason = get_block_reason(&result);
    assert!(reason.contains("tokensave MCP tools"));
}

#[test]
fn test_block_response_uses_correct_hook_schema() {
    let input = r#"{"subagent_type": "Explore"}"#;
    let result = evaluate_hook_decision(input);
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"].as_str(),
        Some("PreToolUse")
    );
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"].as_str(),
        Some("deny")
    );
    assert!(v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .is_some());
}

#[test]
fn test_kiro_blocks_delegate_code_research_task() {
    let input = r#"{
        "hook_event_name": "preToolUse",
        "tool_name": "delegate",
        "tool_input": {
            "task": "Explore the codebase architecture and call graph"
        }
    }"#;
    let reason = evaluate_kiro_pre_tool_use(input).unwrap();
    assert!(reason.contains("tokensave MCP tools"));
}

#[test]
fn test_kiro_blocks_subagent_research_prompt() {
    let input = r#"{
        "hook_event_name": "preToolUse",
        "tool_name": "subagent",
        "tool_input": {
            "prompt": "who calls the process_data function?"
        }
    }"#;
    assert!(evaluate_kiro_pre_tool_use(input).is_some());
}

#[test]
fn test_kiro_allows_delegate_execution_task() {
    let input = r#"{
        "hook_event_name": "preToolUse",
        "tool_name": "delegate",
        "tool_input": {
            "task": "Run the full test suite and report failures"
        }
    }"#;
    assert!(evaluate_kiro_pre_tool_use(input).is_none());
}

#[test]
fn test_kiro_allows_non_delegation_tool() {
    let input = r#"{
        "hook_event_name": "preToolUse",
        "tool_name": "read",
        "tool_input": {
            "prompt": "Explore the entire codebase"
        }
    }"#;
    assert!(evaluate_kiro_pre_tool_use(input).is_none());
}

#[test]
fn test_kiro_allows_invalid_json() {
    assert!(evaluate_kiro_pre_tool_use("not json").is_none());
}

#[test]
fn test_cursor_subagent_start_blocks_explore_research_task() {
    let input = r#"{
        "hook_event_name": "subagentStart",
        "subagent_type": "explore",
        "task": "Explore the codebase architecture and call graph"
    }"#;

    let output = evaluate_cursor_subagent_start(input).expect("should deny research subagent");
    let v: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(v["permission"].as_str(), Some("deny"));
    assert!(v["user_message"]
        .as_str()
        .unwrap_or_default()
        .contains("tokensave MCP tools"));
    assert!(
        v.get("hookSpecificOutput").is_none(),
        "Cursor hook output must use Cursor's documented subagentStart fields"
    );
}

#[test]
fn test_cursor_subagent_start_allows_execution_task() {
    let input = r#"{
        "hook_event_name": "subagentStart",
        "subagent_type": "generalPurpose",
        "task": "Run the test suite and summarize failures"
    }"#;

    assert!(evaluate_cursor_subagent_start(input).is_none());
}

#[test]
fn test_cursor_project_root_uses_workspace_roots() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".tokensave")).unwrap();
    std::fs::write(dir.path().join(".tokensave/tokensave.db"), "").unwrap();
    let input = format!(
        r#"{{
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": [{}]
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_cursor_project_root_uses_file_path_parent() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(dir.path().join(".tokensave")).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.path().join(".tokensave/tokensave.db"), "").unwrap();
    let file = src.join("lib.rs");
    let input = format!(
        r#"{{
            "hook_event_name": "afterFileEdit",
            "file_path": {}
        }}"#,
        serde_json::to_string(file.to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_is_git_state_changing_command_detects_branch_switches() {
    for command in [
        "git checkout main",
        "git switch -c feature/x",
        "git pull --rebase",
        "git merge origin/main",
        "git rebase main",
        "git reset --hard HEAD~1",
        "git cherry-pick abc123",
        "git stash pop",
        "git stash apply stash@{0}",
        "  GIT  checkout main  ",
    ] {
        assert!(
            is_git_state_changing_command(command),
            "{command} should be treated as a git state-changing command"
        );
    }
}

#[test]
fn test_is_git_state_changing_command_ignores_read_only_and_non_git() {
    for command in [
        "git status",
        "git log --oneline",
        "git diff",
        "git commit -m wip",
        "git add .",
        "git stash list",
        "ls -la",
        "cargo test",
        "echo git checkout",
    ] {
        assert!(
            !is_git_state_changing_command(command),
            "{command} should NOT trigger a sync"
        );
    }
}

#[test]
fn test_cursor_after_file_edit_rel_paths_targets_edited_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let edited = root.join("src/lib.rs");
    let input = format!(
        r#"{{
            "hook_event_name": "afterFileEdit",
            "file_path": {},
            "edits": [{{ "old_string": "a", "new_string": "b" }}]
        }}"#,
        serde_json::to_string(edited.to_str().unwrap()).unwrap()
    );

    let rels = tokensave::hooks::cursor_after_file_edit_rel_paths(&input, &root);
    assert_eq!(rels, vec!["src/lib.rs".to_string()]);
}

#[test]
fn test_cursor_after_file_edit_rel_paths_skips_paths_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let input = r#"{
        "hook_event_name": "afterFileEdit",
        "file_path": "/etc/passwd"
    }"#;

    let rels = tokensave::hooks::cursor_after_file_edit_rel_paths(input, &root);
    assert!(
        rels.is_empty(),
        "paths outside the project root must be ignored, got {rels:?}"
    );
}

#[test]
fn test_cursor_should_run_sync_respects_debounce_window() {
    assert!(cursor_should_run_sync(1_000, None, 3));
    assert!(cursor_should_run_sync(1_000, Some(996), 3));
    assert!(!cursor_should_run_sync(1_000, Some(998), 3));
    assert!(!cursor_should_run_sync(1_000, Some(1_000), 3));
}

#[test]
fn test_build_cursor_session_context_uninitialized_suggests_init() {
    let context = build_cursor_session_context(false, None);
    assert!(context.contains("tokensave init"));
    assert!(context.contains("tokensave MCP tools"));
    assert!(context.contains("tokensave_context"));
}

#[test]
fn test_build_cursor_session_context_initialized_includes_freshness() {
    let context = build_cursor_session_context(true, Some("last indexed 2m ago"));
    assert!(context.contains("tokensave_context"));
    assert!(context.contains("last indexed 2m ago"));
    assert!(
        !context.contains("tokensave init"),
        "initialized workspaces should not be told to run init: {context}"
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
        v["env"]["TOKENSAVE_PROJECT_ROOT"].as_str(),
        Some(dir.path().to_string_lossy().as_ref())
    );
}

#[test]
fn test_cursor_session_start_json_without_root_omits_env_path() {
    let json = cursor_session_start_json(None, "ctx");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["additional_context"], "ctx");
    assert!(v["env"].get("TOKENSAVE_PROJECT_ROOT").is_none());
}

#[test]
fn test_cursor_branch_switch_target_extracts_branch() {
    assert_eq!(
        cursor_branch_switch_target("git checkout main"),
        Some("main".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git switch develop"),
        Some("develop".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git checkout -b feature/x"),
        Some("feature/x".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git switch -c feature/y"),
        Some("feature/y".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git worktree add ../wt feature/z"),
        Some("feature/z".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git worktree add -b newbranch ../wt"),
        Some("newbranch".to_string())
    );
}

#[test]
fn test_cursor_branch_switch_target_ignores_path_checkouts_and_non_switches() {
    assert_eq!(
        cursor_branch_switch_target("git checkout -- src/main.rs"),
        None
    );
    assert_eq!(cursor_branch_switch_target("git pull --rebase"), None);
    assert_eq!(cursor_branch_switch_target("git merge origin/main"), None);
    assert_eq!(cursor_branch_switch_target("git status"), None);
    assert_eq!(cursor_branch_switch_target("echo git checkout main"), None);
}

#[test]
fn test_cursor_shell_sync_plan_routes_branch_switch_to_branch_add() {
    assert_eq!(
        cursor_shell_sync_plan("git checkout main"),
        CursorShellSyncPlan::BranchAdd("main".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan("git switch -c feature/x"),
        CursorShellSyncPlan::BranchAdd("feature/x".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan("git worktree add ../wt feature/z"),
        CursorShellSyncPlan::BranchAdd("feature/z".to_string())
    );
}

#[test]
fn test_cursor_shell_sync_plan_routes_same_branch_changes_to_incremental_sync() {
    for command in [
        "git pull --rebase",
        "git merge origin/main",
        "git rebase main",
        "git reset --hard HEAD~1",
        "git cherry-pick abc123",
        "git stash pop",
    ] {
        assert_eq!(
            cursor_shell_sync_plan(command),
            CursorShellSyncPlan::IncrementalSync,
            "{command} should route to an incremental sync"
        );
    }
}

#[test]
fn test_cursor_shell_sync_plan_noop_for_read_only_and_non_git() {
    for command in ["git status", "git log", "ls -la", "cargo build"] {
        assert_eq!(
            cursor_shell_sync_plan(command),
            CursorShellSyncPlan::Noop,
            "{command} should be a no-op"
        );
    }
}

// ---------------------------------------------------------------------------
// Codex hook handlers
// ---------------------------------------------------------------------------

#[test]
fn test_codex_additional_context_json_uses_codex_schema() {
    let json = codex_additional_context_json("SessionStart", "hello context");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"].as_str(),
        Some("SessionStart")
    );
    assert_eq!(
        v["hookSpecificOutput"]["additionalContext"].as_str(),
        Some("hello context")
    );
    // Codex must not reuse the Cursor/Claude permission output shapes.
    assert!(v.get("permission").is_none());
    assert!(v["hookSpecificOutput"].get("permissionDecision").is_none());
}

#[test]
fn test_codex_subagent_start_redirects_explore_research_agent() {
    // Codex SubagentStart cannot hard-stop a subagent (`continue: false` is
    // ignored), so the handler steers it via hookSpecificOutput.additionalContext.
    let input = r#"{
        "hook_event_name": "SubagentStart",
        "agent_type": "explore",
        "cwd": "/tmp/x"
    }"#;

    let output = evaluate_codex_subagent_start(input).expect("should redirect research subagent");
    let v: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"].as_str(),
        Some("SubagentStart")
    );
    assert!(v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .contains("tokensave MCP tools"));
    // Must use the Codex output schema, not Cursor's `permission`/`user_message`.
    assert!(
        v.get("permission").is_none(),
        "Codex hook output must not use Cursor's subagentStart fields"
    );
}

#[test]
fn test_codex_subagent_start_allows_execution_agent() {
    let input = r#"{
        "hook_event_name": "SubagentStart",
        "agent_type": "generalPurpose",
        "prompt": "Run the test suite and summarize failures"
    }"#;
    assert!(evaluate_codex_subagent_start(input).is_none());
}

#[test]
fn test_codex_subagent_start_allows_invalid_json() {
    assert!(evaluate_codex_subagent_start("not json").is_none());
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
    let command = "*** Begin Patch\n*** Update File: /etc/passwd\n*** End Patch\n";

    let rels = codex_apply_patch_rel_paths(command, &root, &root);
    assert!(
        rels.is_empty(),
        "absolute paths outside the project root must be ignored, got {rels:?}"
    );
}

#[test]
fn test_codex_project_root_uses_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".tokensave")).unwrap();
    std::fs::write(dir.path().join(".tokensave/tokensave.db"), "").unwrap();
    let input = format!(
        r#"{{
            "hook_event_name": "PostToolUse",
            "cwd": {}
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        codex_project_root_from_event(&input),
        Some(dir.path().to_path_buf())
    );
}
