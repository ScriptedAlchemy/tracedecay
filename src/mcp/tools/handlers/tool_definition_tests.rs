use serde_json::json;

use super::super::get_tool_definitions;
use super::*;

#[test]
fn test_tool_definitions_complete() {
    let tools = get_tool_definitions();
    let compatibility_tools = tools
        .iter()
        .filter(|tool| ApplicationSurfaceOperation::from_tool_name(&tool.name).is_none())
        .collect::<Vec<_>>();
    for tool in compatibility_tools {
        assert!(
            LegacyToolCompatibilityOwner::admits(&tool.name),
            "{} must have an explicit compatibility owner",
            tool.name
        );
    }

    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(tool_names.contains(&"tracedecay_search"));
    assert!(tool_names.contains(&"tracedecay_move_symbol"));
    assert!(tool_names.contains(&"tracedecay_api_migration_plan"));
    assert!(tool_names.contains(&"tracedecay_api_migration_apply"));
    assert!(tool_names.contains(&"tracedecay_analytics"));
    assert!(tool_names.contains(&"tracedecay_retrieve"));
    assert!(tool_names.contains(&"tracedecay_context"));
    assert!(tool_names.contains(&"tracedecay_callers"));
    assert!(tool_names.contains(&"tracedecay_callees"));
    assert!(tool_names.contains(&"tracedecay_callers_for"));
    assert!(tool_names.contains(&"tracedecay_by_qualified_name"));
    assert!(tool_names.contains(&"tracedecay_signature"));
    assert!(tool_names.contains(&"tracedecay_impls"));
    assert!(tool_names.contains(&"tracedecay_diagnose"));
    assert!(tool_names.contains(&"tracedecay_run_affected_tests"));
    assert!(tool_names.contains(&"tracedecay_derives"));
    assert!(tool_names.contains(&"tracedecay_fact_store"));
    assert!(tool_names.contains(&"tracedecay_fact_feedback"));
    assert!(tool_names.contains(&"tracedecay_memory_status"));
    assert!(tool_names.contains(&"tracedecay_session_refresh"));
    assert!(tool_names.contains(&"tracedecay_message_search"));
    assert!(tool_names.contains(&"tracedecay_impact"));
    assert!(tool_names.contains(&"tracedecay_node"));
    assert!(tool_names.contains(&"tracedecay_status"));
    assert!(tool_names.contains(&"tracedecay_active_project"));
    assert!(tool_names.contains(&"tracedecay_storage_status"));
    assert!(tool_names.contains(&"tracedecay_project_list"));
    assert!(tool_names.contains(&"tracedecay_project_search"));
    assert!(tool_names.contains(&"tracedecay_project_context"));
    assert!(tool_names.contains(&"tracedecay_files"));
    assert!(tool_names.contains(&"tracedecay_affected"));
    assert!(tool_names.contains(&"tracedecay_dead_code"));
    assert!(tool_names.contains(&"tracedecay_diff_context"));
    assert!(tool_names.contains(&"tracedecay_module_api"));
    assert!(tool_names.contains(&"tracedecay_circular"));
    assert!(tool_names.contains(&"tracedecay_hotspots"));
    assert!(tool_names.contains(&"tracedecay_similar"));
    assert!(tool_names.contains(&"tracedecay_rename_preview"));
    assert!(tool_names.contains(&"tracedecay_unused_imports"));
    assert!(tool_names.contains(&"tracedecay_changelog"));
    assert!(tool_names.contains(&"tracedecay_rank"));
    assert!(tool_names.contains(&"tracedecay_largest"));
    assert!(tool_names.contains(&"tracedecay_coupling"));
    assert!(tool_names.contains(&"tracedecay_inheritance_depth"));
    assert!(tool_names.contains(&"tracedecay_distribution"));
    assert!(tool_names.contains(&"tracedecay_recursion"));
    assert!(tool_names.contains(&"tracedecay_complexity"));
    assert!(tool_names.contains(&"tracedecay_doc_coverage"));
    assert!(tool_names.contains(&"tracedecay_god_class"));
    assert!(tool_names.contains(&"tracedecay_port_status"));
    assert!(tool_names.contains(&"tracedecay_port_order"));
    assert!(tool_names.contains(&"tracedecay_commit_context"));
    assert!(tool_names.contains(&"tracedecay_pr_context"));
    assert!(tool_names.contains(&"tracedecay_simplify_scan"));
    assert!(tool_names.contains(&"tracedecay_test_map"));
    assert!(tool_names.contains(&"tracedecay_type_hierarchy"));
    assert!(tool_names.contains(&"tracedecay_branch_search"));
    assert!(tool_names.contains(&"tracedecay_branch_diff"));
    assert!(tool_names.contains(&"tracedecay_branch_list"));
    assert!(tool_names.contains(&"tracedecay_str_replace"));
    assert!(tool_names.contains(&"tracedecay_multi_str_replace"));
    assert!(tool_names.contains(&"tracedecay_insert_at"));
    // Structural search runs in-process (bundled grammars), so it is always
    // advertised — unlike the CLI-backed rewrite tool gated just below.
    assert!(tool_names.contains(&"tracedecay_ast_grep_search"));
    if super::super::definitions::ast_grep_available() {
        assert!(tool_names.contains(&"tracedecay_ast_grep_rewrite"));
    } else {
        assert!(!tool_names.contains(&"tracedecay_ast_grep_rewrite"));
    }
    assert!(tool_names.contains(&"tracedecay_gini"));
    assert!(tool_names.contains(&"tracedecay_dependency_depth"));
    assert!(tool_names.contains(&"tracedecay_health"));
    assert!(tool_names.contains(&"tracedecay_redundancy"));
    assert!(tool_names.contains(&"tracedecay_runtime"));
    assert!(tool_names.contains(&"tracedecay_dsm"));
    assert!(tool_names.contains(&"tracedecay_test_risk"));
    assert!(tool_names.contains(&"tracedecay_session_start"));
    assert!(tool_names.contains(&"tracedecay_session_end"));
    assert!(tool_names.contains(&"tracedecay_body"));
    assert!(tool_names.contains(&"tracedecay_todos"));
    assert!(tool_names.contains(&"tracedecay_fact_store"));
    assert!(tool_names.contains(&"tracedecay_fact_feedback"));
    assert!(tool_names.contains(&"tracedecay_memory_status"));
    assert!(tool_names.contains(&"tracedecay_dashboard"));
    assert!(tool_names.contains(&"tracedecay_message_search"));
    assert!(tool_names.contains(&"tracedecay_sessions_for"));
    assert!(tool_names.contains(&"tracedecay_workflows"));
    assert!(tool_names.contains(&"tracedecay_lcm_status"));
    assert!(tool_names.contains(&"tracedecay_lcm_doctor"));
    assert!(tool_names.contains(&"tracedecay_lcm_load_session"));
    assert!(tool_names.contains(&"tracedecay_lcm_grep"));
    assert!(tool_names.contains(&"tracedecay_lcm_describe"));
    assert!(tool_names.contains(&"tracedecay_lcm_expand"));
    assert!(tool_names.contains(&"tracedecay_lcm_expand_query"));
    assert!(tool_names.contains(&"tracedecay_lcm_preflight"));
    assert!(tool_names.contains(&"tracedecay_lcm_compress"));
    assert!(tool_names.contains(&"tracedecay_lcm_session_boundary"));
    assert!(tool_names.contains(&"tracedecay_read"));
    assert!(tool_names.contains(&"tracedecay_outline"));
    assert!(tool_names.contains(&"tracedecay_implementations"));
    assert!(tool_names.contains(&"tracedecay_unsafe_patterns"));
    assert!(tool_names.contains(&"tracedecay_diagnostics"));
    assert!(tool_names.contains(&"tracedecay_config"));
    assert!(tool_names.contains(&"tracedecay_signature_search"));
    assert!(tool_names.contains(&"tracedecay_constructors"));
    assert!(tool_names.contains(&"tracedecay_field_sites"));
    assert!(tool_names.contains(&"tracedecay_call_chain"));
    assert!(tool_names.contains(&"tracedecay_file_dependents"));
    assert!(tool_names.contains(&"tracedecay_replace_symbol"));
    assert!(tool_names.contains(&"tracedecay_insert_at_symbol"));
    assert!(tool_names.contains(&"tracedecay_move_symbol"));
    assert!(tool_names.contains(&"tracedecay_api_migration_plan"));
    assert!(tool_names.contains(&"tracedecay_api_migration_apply"));
    assert!(tool_names.contains(&"tracedecay_source_edit_reconcile"));
    assert!(tool_names.contains(&"tracedecay_find_exact_symbol"));
}

#[test]
fn test_tool_definitions_have_schemas() {
    let tools = get_tool_definitions();
    for tool in &tools {
        assert!(!tool.name.is_empty());
        assert!(!tool.description.is_empty());
        assert!(tool.input_schema.is_object());
        assert_eq!(tool.input_schema["type"], "object");
    }
}

#[test]
fn format_capable_tools_advertise_markdown_json_without_tables() {
    let tools = get_tool_definitions();
    for tool_name in super::super::definitions::format_capable_tool_names() {
        if *tool_name == "tracedecay_ast_grep_rewrite"
            && !super::super::definitions::ast_grep_available()
        {
            continue;
        }
        let tool = tools
            .iter()
            .find(|tool| tool.name == *tool_name)
            .unwrap_or_else(|| panic!("{tool_name} missing tool definition"));
        let format = &tool.input_schema["properties"]["format"];
        assert_eq!(
            format["enum"],
            json!(["markdown", "json"]),
            "{tool_name} should expose markdown/json format choices"
        );
        let description = format["description"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool_name} format must have a description"));
        assert!(
            description.contains("Default 'markdown'"),
            "{tool_name} should document Markdown as default: {description}"
        );
        assert!(
            description.contains("no tables"),
            "{tool_name} should advertise no-table Markdown: {description}"
        );
        assert!(
            !description.contains("prose/tables"),
            "{tool_name} should not advertise table-heavy Markdown: {description}"
        );
    }
}

#[test]
fn every_advertised_application_surface_uses_canonical_output_formats() {
    let tools = get_tool_definitions();
    for operation in APPLICATION_SURFACE_OPERATIONS {
        let tool_name = format!("tracedecay_{}", operation.as_str());
        let tool = tools
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} missing tool definition"));
        assert_eq!(
            tool.input_schema["properties"]["format"]["enum"],
            json!(["markdown", "json"]),
            "{tool_name} must expose the canonical output formats"
        );
    }
}

#[test]
fn redundancy_tool_definition_describes_ranking_contract() {
    let tools = get_tool_definitions();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_redundancy")
        .expect("tracedecay_redundancy tool definition");
    // Assert only literal output keys — free prose in the description may
    // be reworded without breaking the ranking contract.
    for required in [
        "ranking_score",
        "body_vector_cosine",
        "generic_helper_downranked",
    ] {
        assert!(
            tool.description.contains(required),
            "redundancy definition should mention {required}: {}",
            tool.description
        );
    }
}

#[test]
fn test_tool_definitions_have_annotations() {
    let tools = get_tool_definitions();
    let write_tools = [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_api_migration_apply",
        "tracedecay_source_edit_reconcile",
        "tracedecay_git_apply",
        "tracedecay_run_affected_tests",
        "tracedecay_session_start",
        "tracedecay_session_end",
        "tracedecay_fact_store",
        "tracedecay_fact_feedback",
        "tracedecay_memory_status",
        "tracedecay_session_refresh",
        "tracedecay_configuration_set",
        "tracedecay_configuration_unset",
        "tracedecay_configuration_batch",
        "tracedecay_configuration_write_credential",
        "tracedecay_configuration_protected_apply",
        "tracedecay_configuration_rollback_apply",
        "tracedecay_context_scout_pause",
        "tracedecay_context_scout_resume",
        "tracedecay_context_scout_cancel",
        "tracedecay_context_scout_claim",
        "tracedecay_context_scout_delivery",
        "tracedecay_context_scout_feedback",
        "tracedecay_lcm_doctor",
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
    ];
    for tool in &tools {
        let ann = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} missing annotations", tool.name));
        if write_tools.contains(&tool.name.as_str()) {
            assert_eq!(
                ann["readOnlyHint"], false,
                "{} should have readOnlyHint=false",
                tool.name
            );
        } else {
            assert_eq!(
                ann["readOnlyHint"], true,
                "{} missing readOnlyHint",
                tool.name
            );
        }
        assert!(
            ann["title"].is_string(),
            "{} missing title annotation",
            tool.name
        );
    }
}

#[test]
fn test_always_load_tools() {
    let tools = get_tool_definitions();
    let always_load: Vec<&str> = tools
        .iter()
        .filter(|t| {
            t.meta
                .as_ref()
                .and_then(|m| m.get("anthropic/alwaysLoad"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        always_load.contains(&"tracedecay_context"),
        "tracedecay_context must be alwaysLoad"
    );
    assert!(
        always_load.contains(&"tracedecay_search"),
        "tracedecay_search must be alwaysLoad"
    );
    assert!(
        always_load.contains(&"tracedecay_status"),
        "tracedecay_status must be alwaysLoad"
    );
    assert!(
        always_load.contains(&"tracedecay_active_project"),
        "tracedecay_active_project must be alwaysLoad"
    );
    assert!(
        always_load.contains(&"tracedecay_storage_status"),
        "tracedecay_storage_status must be alwaysLoad"
    );
    // grep and callers cover the two most common native-tool reflexes
    // (content search and "who calls this"), so they join the always-loaded
    // set to keep the model from ToolSearch-ing before reaching for Bash.
    assert!(
        always_load.contains(&"tracedecay_grep"),
        "tracedecay_grep must be alwaysLoad"
    );
    assert!(
        always_load.contains(&"tracedecay_callers"),
        "tracedecay_callers must be alwaysLoad"
    );
    assert_eq!(
        always_load.len(),
        7,
        "exactly 7 tools should be alwaysLoad (cap), got {:?}",
        always_load
    );
}

#[test]
fn test_tool_definitions_serializable() {
    let tools = get_tool_definitions();
    let json = serde_json::to_string(&tools).unwrap();
    assert!(json.contains("tracedecay_search"));
    assert!(json.contains("tracedecay_status"));
}
