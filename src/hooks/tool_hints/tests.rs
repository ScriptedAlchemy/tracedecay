use super::*;

fn input_for_tool(tool_name: &str) -> ToolHintInput {
    ToolHintInput {
        tool_name: Some(tool_name.to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    }
}

#[test]
fn semantic_search_tools_get_a_context_hint() {
    for name in ["SemanticSearch", "codebase_search", "Codebase Search"] {
        let hint = decide_hint(&input_for_tool(name)).unwrap();
        assert_eq!(hint.category, HintCategory::SemanticSearch, "{name}");
        assert!(hint.context.contains("tracedecay_context"), "{name}");
        assert!(
            hint.context.contains("tracedecay_grep"),
            "semantic-search hint must route literal text to tracedecay_grep: {name}"
        );
        assert!(hint.nonblocking, "semantic-search hints must stay soft");
    }
}

#[test]
fn grep_tool_search_routes_literal_matches_to_grep() {
    for name in ["Grep", "search"] {
        let hint = decide_hint(&input_for_tool(name)).unwrap();
        assert_eq!(hint.category, HintCategory::Search, "{name}");
        assert!(
            hint.message.contains("tracedecay_grep"),
            "search hint must lead with grep routing: {name}"
        );
        assert!(hint.context.contains("tracedecay_grep"), "{name}");
        assert!(hint.context.contains("tracedecay_search"), "{name}");
    }
}

#[test]
fn parent_directory_find_gets_project_registry_hint() {
    let hint = decide_hint(&ToolHintInput {
        tool_name: Some("shell".to_string()),
        command: Some("find .. -maxdepth 3 -type f -iname '*runner*'".to_string()),
        prompt: Some("Find where the clean-ci Windows runner orchestrator is defined".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "project_context");
    assert!(hint.context.contains("tracedecay_project_list"));
    assert!(hint.context.contains("tracedecay_project_search"));
}

#[test]
fn external_repo_shell_search_prefers_project_registry_hint() {
    let hint = decide_hint(&ToolHintInput {
        tool_name: Some("shell".to_string()),
        command: Some("rg -n \"proxmox|windows|runner|clean-ci\" .".to_string()),
        prompt: Some("Find the runner orchestrator repo and update its Windows boxes".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "project_context");
    assert!(hint.message.contains("registered projects"));
}

#[test]
fn current_repo_shell_search_keeps_normal_search_hint() {
    let hint = decide_hint(&ToolHintInput {
        tool_name: Some("shell".to_string()),
        command: Some("rg -n \"runner\" .".to_string()),
        prompt: Some("Search this repo for the runner implementation".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "search");
    assert!(hint.context.contains("tracedecay_search"));
    assert!(
        hint.context.contains("tracedecay_grep"),
        "literal/regex search must route to tracedecay_grep"
    );
    assert!(
        hint.message.contains("tracedecay_grep"),
        "search hint must lead with grep routing for literal patterns"
    );
}

#[test]
fn trace_function_prompts_get_call_graph_ladder_before_generic_search() {
    let hint = decide_hint(&ToolHintInput {
        tool_name: Some("shell".to_string()),
        command: Some("rg -n \"setup_project\" tests/mcp_handler_test.rs".to_string()),
        prompt: Some(
            "Use TraceDecay to trace the function and find callers of setup_project".to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "call_graph");
    assert!(hint.context.contains("tracedecay_find_exact_symbol"));
    assert!(hint.context.contains("tracedecay_callers"));
    assert!(hint.context.contains("tracedecay_callees"));
}

#[test]
fn dependency_fixture_prompts_get_call_graph_ladder() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "Which tests still depend on setup_project instead of setup_empty_project?".to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "call_graph");
    assert!(hint.context.contains("tracedecay_callers"));
    assert!(hint.context.contains("tracedecay_impact"));
}

#[test]
fn affected_test_prompts_get_test_mapping_ladder() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "Find affected tests and blast radius for this refactor before running cargo"
                .to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "impact");
    assert!(hint.context.contains("tracedecay_diff_context"));
    assert!(hint.context.contains("tracedecay_affected"));
    assert!(hint.context.contains("tracedecay_test_map"));
}

#[test]
fn mechanical_edit_prompts_get_atomic_edit_ladder() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "Use ast-grep for a mechanical rewrite and replace this everywhere safely".to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "atomic_edit");
    assert!(hint.context.contains("tracedecay_str_replace"));
    assert!(hint.context.contains("tracedecay_multi_str_replace"));
    assert!(hint.context.contains("tracedecay_insert_at"));
    assert!(hint.context.contains("tracedecay_insert_at_symbol"));
    assert!(hint.context.contains("tracedecay_ast_grep_rewrite"));
}

#[test]
fn every_hint_route_names_a_registered_agent_tool() {
    let registered = crate::mcp::tools::get_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<std::collections::HashSet<_>>();

    for spec in CATEGORY_SPECS {
        for tool in spec.expected_tools {
            let unavailable_optional_tool =
                *tool == "tracedecay_ast_grep_rewrite" && !crate::mcp::tools::ast_grep_available();
            assert!(
                registered.contains(*tool) || unavailable_optional_tool,
                "hint category {} routes agents to unregistered tool {tool}",
                spec.key
            );
        }
    }
}

#[test]
fn type_orientation_prompts_get_ast_graph_ladder() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "Find constructor sites, field writes, trait impls, and duplicate logic".to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "type_orientation");
    assert!(hint.context.contains("tracedecay_constructors"));
    assert!(hint.context.contains("tracedecay_field_sites"));
    assert!(hint.context.contains("tracedecay_redundancy"));
}

#[test]
fn prior_conversation_prompt_gets_session_recall_hint() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "Where did we talk about clean-ci and the runner orchestrator before?".to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "session_recall");
    assert!(hint.context.contains("tracedecay_message_search"));
    assert!(hint.context.contains("tracedecay_lcm_grep"));
}

#[test]
fn unexpected_commit_confusion_routes_to_attribution_skill() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "There's a commit I didn't make on my branch and a test file I didn't write — \
             figure out who pushed this."
                .to_string(),
        ),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category.as_key(), "unexpected_changes");
    assert!(hint.context.contains("tracedecay_sessions_for"));
    assert!(
        hint.context
            .contains("Skill: tracedecay:investigating-unexpected-changes."),
        "unexpected-change hint must route to the attribution skill"
    );
}

#[test]
fn benign_git_narration_does_not_fire_the_unexpected_change_hint() {
    let benign = ToolHintInput {
        prompt: Some("I committed the slice and confirmed the working tree is clean.".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_ne!(
        classify_hint(&benign),
        Some(HintCategory::UnexpectedChanges),
        "ordinary commit narration must not trigger the unexpected-change hint"
    );
}

#[test]
fn single_file_read_gets_a_soft_outline_hint() {
    let mut input = input_for_tool("Read");
    input.file_path = Some("src/lib.rs".to_string());
    let hint = decide_hint(&input).unwrap();
    assert_eq!(hint.category, HintCategory::FileRead);
    assert!(hint.message.contains("tracedecay_outline"));
    assert!(
        hint.context.contains("tracedecay_grep"),
        "reading a file to find a string should route to tracedecay_grep"
    );
    assert!(hint.nonblocking, "read hints must stay soft");
}

#[test]
fn broad_read_prompts_route_literal_hunts_to_grep() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some("Read every file in the entire codebase to find the flag".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    })
    .unwrap();

    assert_eq!(hint.category, HintCategory::BroadRead);
    assert!(hint.context.contains("tracedecay_context"));
    assert!(
        hint.context.contains("tracedecay_grep"),
        "broad-read hint must route literal string hunts to tracedecay_grep"
    );
}

#[test]
fn tracedecay_tool_schema_reads_get_direct_tool_hint() {
    let mut input = input_for_tool("ReadFile");
    input.file_path = Some(
        "/home/zack/.cursor/projects/repo/mcps/plugin-tracedecay/tools/tracedecay_callers.json"
            .to_string(),
    );
    let hint = decide_hint(&input).unwrap();

    assert_eq!(hint.category, HintCategory::ToolDescriptorRead);
    assert!(hint.message.contains("tool descriptor"));
    assert!(hint.context.contains("tracedecay_callers"));
    assert!(hint.context.contains("tracedecay_callees"));
}

#[test]
fn read_without_file_path_gets_no_hint() {
    assert!(decide_hint(&input_for_tool("Read")).is_none());
}

#[test]
fn classifier_priority_handles_overlapping_signals() {
    let recall = ToolHintInput {
        prompt: Some("remember when we traced setup_project last time?".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(classify_hint(&recall), Some(HintCategory::SessionRecall));

    let file_list = ToolHintInput {
        tool_name: Some("Bash".to_string()),
        command: Some("rg --files src/hooks".to_string()),
        prompt: Some("find files, do not search contents".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(classify_hint(&file_list), Some(HintCategory::FileLookup));

    let call_vs_impact = ToolHintInput {
        prompt: Some("who calls setup_project and which tests depend on it?".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(
        classify_hint(&call_vs_impact),
        Some(HintCategory::CallGraph)
    );
}

#[test]
fn every_category_has_compact_skill_backed_rendering() {
    let categories = [
        HintCategory::Search,
        HintCategory::SemanticSearch,
        HintCategory::FileRead,
        HintCategory::ToolDescriptorRead,
        HintCategory::BroadRead,
        HintCategory::CallGraph,
        HintCategory::Impact,
        HintCategory::SymbolLookup,
        HintCategory::FileLookup,
        HintCategory::ProjectContext,
        HintCategory::SessionRecall,
        HintCategory::AtomicEdit,
        HintCategory::TypeOrientation,
        HintCategory::ExploreSubagent,
        HintCategory::SubagentStartContext,
        HintCategory::BuildDiagnostics,
        HintCategory::ReviewChanges,
        HintCategory::MemoryStore,
        HintCategory::EditRedundancy,
        HintCategory::UnexpectedChanges,
    ];

    for category in categories {
        let hint = hint_for_category(category);
        let visible = format!("{}\n{}", hint.message, hint.context);
        assert_eq!(hint.category, category);
        assert!(!hint.message.is_empty(), "{category:?}");
        assert!(!hint.context.is_empty(), "{category:?}");
        assert!(
            visible.len() <= 850,
            "{category:?} hint is too verbose: {} chars\n{}",
            visible.len(),
            visible
        );
        let skill = category_skill(category);
        assert!(
            visible.contains(&format!("Skill: tracedecay:{skill}.")),
            "{category:?} missing skill trigger"
        );
    }
}

#[test]
fn dedupe_emits_each_category_once_per_session() {
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::ToolDescriptorRead),
        HintDecision::Emit
    );
    // Fresh session gets its own budget.
    assert_eq!(
        dedupe.decide("s2", HintCategory::Search),
        HintDecision::Emit
    );
}

#[test]
fn descriptor_reads_dedupe_separately_from_source_file_reads() {
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::ToolDescriptorRead),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::SuppressedDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::ToolDescriptorRead),
        HintDecision::SuppressedDuplicate
    );
}

#[test]
fn per_session_budget_caps_total_hints() {
    let mut dedupe = ToolHintDedupe::default();
    // Three distinct categories fit the budget.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDecision::Emit
    );
    // The fourth distinct category is held back by the budget, not dedupe.
    assert_eq!(
        dedupe.decide("s1", HintCategory::CallGraph),
        HintDecision::SuppressedBudget
    );
    // A different session is unaffected by s1's exhausted budget.
    assert_eq!(
        dedupe.decide("s2", HintCategory::CallGraph),
        HintDecision::Emit
    );
}

#[test]
fn escalation_fires_exactly_once_after_repeated_triggers() {
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    // Repeat fires below the threshold stay silent.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
    // Third post-hint fire unlocks the single escalation.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Escalate
    );
    // Everything after escalation is permanently silent.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
}

#[test]
fn escalation_does_not_count_against_the_budget() {
    let mut dedupe = ToolHintDedupe::default();
    // Exhaust the budget with three categories, then escalate the first.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDecision::Emit
    );
    for _ in 0..(ESCALATION_TRIGGER_THRESHOLD - 1) {
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
    }
    // Escalation is allowed even though the budget is spent.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Escalate
    );
}

#[test]
fn escalated_hint_prefixes_the_base_message() {
    let base = hint_for_category(HintCategory::Search);
    let escalated = base.escalated();
    assert!(
        escalated
            .message
            .starts_with("Repeated native search usage this session — ")
    );
    assert!(escalated.message.contains(&base.message));
    assert_eq!(escalated.category, base.category);
    assert_eq!(escalated.context, base.context);
    assert_eq!(escalated.nonblocking, base.nonblocking);
}

#[test]
fn dedupe_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/tool_hints_seen.json");

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    dedupe.save(&path).unwrap();

    let mut reloaded = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        reloaded.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate,
        "persisted (session, category) pairs must suppress re-emission"
    );
    assert_eq!(
        reloaded.decide("s1", HintCategory::FileRead),
        HintDecision::Emit
    );
}

#[test]
fn save_writes_versioned_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
    dedupe.save(&path).unwrap();

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 2);
    assert!(value["sessions"].is_array());
    assert!(value["categories"].is_array());
}

#[test]
fn legacy_store_migrates_to_versioned_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    // Legacy v1 file: a bare array of {session_id, category}.
    std::fs::write(
        &path,
        r#"[{"session_id":"s1","category":"search"},{"session_id":"s1","category":"file_read"}]"#,
    )
    .unwrap();

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    // v1 categories load as already-hinted: they suppress, not re-emit.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDecision::SuppressedDuplicate
    );
    // The two migrated hints already count against s1's budget, so only one
    // more distinct category can emit before the cap.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDecision::Emit
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::CallGraph),
        HintDecision::SuppressedBudget
    );

    // Persisting rewrites the file in v2 shape.
    dedupe.save(&path).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 2);

    // Reload from v2 preserves the migrated suppression state.
    let mut reloaded = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        reloaded.decide("s1", HintCategory::Search),
        HintDecision::SuppressedDuplicate
    );
}

#[test]
fn oversized_store_resets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    // A v1 array beyond the persisted bound must reset to an empty state.
    let entries: Vec<String> = (0..=MAX_PERSISTED_HINT_ENTRIES)
        .map(|i| format!(r#"{{"session_id":"s{i}","category":"search"}}"#))
        .collect();
    std::fs::write(&path, format!("[{}]", entries.join(","))).unwrap();

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    // Reset means s0's category is treated as never hinted.
    assert_eq!(
        dedupe.decide("s0", HintCategory::Search),
        HintDecision::Emit
    );
}

#[test]
fn shell_search_classification_honors_quoting() {
    assert!(is_shell_search_command("rg foo src/"));
    assert!(is_shell_search_command("grep -r foo ."));
    assert!(is_shell_search_command("grep --recursive foo ."));
    assert!(is_shell_search_command("(grep -r foo .)"));
    // Quoted multi-word pattern: still a recursive grep.
    assert!(is_shell_search_command("grep -r \"foo bar\" src/"));
    // A flag-looking string INSIDE quotes is data, not a flag — the old
    // split_whitespace parser misclassified this as recursive.
    assert!(!is_shell_search_command("grep \"needle -r\" file.txt"));
    assert!(!is_shell_search_command("grep foo file.txt"));
    assert!(!is_shell_search_command("cat file.txt"));
    assert!(!is_shell_search_command(""));
}

#[test]
fn dedupe_load_tolerates_missing_and_corrupt_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    let mut dedupe = ToolHintDedupe::load_or_default(&missing);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );

    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, "not json").unwrap();
    let mut dedupe = ToolHintDedupe::load_or_default(&corrupt);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDecision::Emit
    );
}

fn shell_input(command: &str) -> ToolHintInput {
    ToolHintInput {
        tool_name: Some("Bash".to_string()),
        command: Some(command.to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    }
}

#[test]
fn successful_or_running_build_commands_do_not_get_a_diagnostics_hint() {
    for command in [
        "cargo check",
        "cargo build --release",
        "cargo clippy --all-targets",
        "cargo test hooks::",
        "tsc --noEmit",
        "npx tsc -p tsconfig.json",
        "pnpm tsc",
        "pyright src/",
        "/usr/bin/tsc",
    ] {
        assert!(
            decide_hint(&shell_input(command)).is_none(),
            "{command} has no failure signal and must stay silent"
        );
    }
}

#[test]
fn build_failure_prompt_gets_a_diagnostics_hint() {
    let hint = decide_hint(&ToolHintInput {
        prompt: Some(
            "cargo check failed with error[E0308]: mismatched types\n --> src/lib.rs:42:5"
                .to_string(),
        ),
        ..ToolHintInput::default()
    })
    .expect("an explicit compiler failure must produce a diagnostics hint");

    assert_eq!(hint.category, HintCategory::BuildDiagnostics);
    assert!(hint.context.contains("tracedecay_diagnostics"));
    assert!(hint.context.contains("tracedecay_diagnose"));
}

#[test]
fn behavioral_test_failure_output_does_not_get_a_diagnostics_hint() {
    let input = ToolHintInput {
        prompt: Some(
            "thread 'tests::works' panicked at src/lib.rs:42:5\n\
             test result: FAILED. 3 passed; 1 failed\n\
             error: test failed, to rerun pass `--lib`"
                .to_string(),
        ),
        ..ToolHintInput::default()
    };

    assert!(
        decide_hint(&input).is_none(),
        "behavioral test failures must use the affected-test path, not compiler diagnostics"
    );
}

#[test]
fn tracedecay_tool_invocations_do_not_recommend_the_same_tool_family() {
    for command in [
        "tracedecay tool diagnostics",
        "tracedecay tool grep --pattern needle",
        "tracedecay tool read --file src/lib.rs",
    ] {
        assert!(
            decide_hint(&shell_input(command)).is_none(),
            "{command} already selected TraceDecay and must stay silent"
        );
    }
}

#[test]
fn non_build_shell_commands_keep_their_own_classification() {
    // A recursive grep is still a search hint, not a build-diagnostics one.
    assert_eq!(
        decide_hint(&shell_input("grep -r foo src/"))
            .unwrap()
            .category,
        HintCategory::Search
    );
    for command in ["cargo run", "cargo fmt", "npm install", ""] {
        assert!(decide_hint(&shell_input(command)).is_none(), "{command}");
    }
}

fn edit_input(tool_name: &str, file_path: &str) -> ToolHintInput {
    ToolHintInput {
        tool_name: Some(tool_name.to_string()),
        file_path: Some(file_path.to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    }
}

#[test]
fn memory_file_edits_get_a_fact_store_hint() {
    for (tool, path) in [
        ("Write", "/home/zack/.claude/projects/foo/memory/MEMORY.md"),
        ("Edit", "/home/zack/.claude/projects/foo/memory/pr-flow.md"),
        ("Write", "/repo/MEMORY.md"),
        ("Edit", "/home/zack/.claude/CLAUDE.md"),
        ("Write", "project/CLAUDE.md"),
    ] {
        let hint = decide_hint(&edit_input(tool, path))
            .unwrap_or_else(|| panic!("{tool} {path} must produce a memory-store hint"));
        assert_eq!(hint.category, HintCategory::MemoryStore, "{tool} {path}");
        assert!(
            hint.message.contains("tracedecay_fact_store"),
            "{tool} {path} hint must point at tracedecay_fact_store"
        );
    }
}

#[test]
fn non_memory_edits_get_no_memory_store_hint() {
    // A regular source edit is not a memory location — and edit tools have no
    // other hint branch, so no hint at all.
    assert!(decide_hint(&edit_input("Write", "src/lib.rs")).is_none());
    // A markdown file in a non-`.claude` `memory` dir does not match.
    assert!(!is_harness_memory_path("/repo/docs/memory/notes.md"));
    // `.claude` present but the file is not directly under a `memory` dir.
    assert!(!is_harness_memory_path(
        "/home/zack/.claude/memory/sub/notes.md"
    ));
    // A non-markdown file under `.claude/**/memory/` does not match.
    assert!(!is_harness_memory_path(
        "/home/zack/.claude/projects/foo/memory/data.json"
    ));
    // Positive controls.
    assert!(is_harness_memory_path(
        "/home/zack/.claude/projects/foo/memory/notes.md"
    ));
    assert!(is_harness_memory_path("/anywhere/MEMORY.md"));
    assert!(is_harness_memory_path("/anywhere/CLAUDE.md"));
    // Windows-style separators normalize.
    assert!(is_harness_memory_path(
        "C:\\Users\\z\\.claude\\projects\\foo\\memory\\notes.md"
    ));
}
