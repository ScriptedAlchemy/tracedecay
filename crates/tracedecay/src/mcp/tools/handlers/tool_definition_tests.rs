use serde_json::json;
use tracedecay_tool_catalog::OperationId;

use super::super::get_tool_definitions;
use super::*;

#[test]
fn terminal_application_definitions_project_canonical_request_schemas() {
    let registry = tracedecay_application::mcp_executable_binding_registry()
        .expect("MCP executable binding registry");
    let definitions = get_tool_definitions().expect("tool definitions");
    for (operation, tool_name, admits_project_selector) in [
        ("context", "tracedecay_context", true),
        ("callees", "tracedecay_callees", true),
        ("impact", "tracedecay_impact", true),
        ("node", "tracedecay_node", true),
        ("similar", "tracedecay_similar", false),
        ("rename_preview", "tracedecay_rename_preview", false),
        ("port_status", "tracedecay_port_status", false),
        ("port_order", "tracedecay_port_order", false),
        ("redundancy", "tracedecay_redundancy", false),
        ("todos", "tracedecay_todos", false),
    ] {
        let operation_id = OperationId::new(format!("operation.application.{operation}"))
            .expect("terminal application operation id");
        let canonical = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .unwrap_or_else(|| panic!("{operation} must have an executable MCP binding"))
            .request_schema()
            .body();
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} must be advertised"));
        let mut projected = definition.input_schema.clone();
        let properties = projected["properties"]
            .as_object_mut()
            .unwrap_or_else(|| panic!("{tool_name} request properties"));
        assert!(properties.remove("format").is_some(), "{tool_name} format");
        if admits_project_selector {
            assert!(
                properties.remove("project_selector").is_some(),
                "{tool_name} must expose project_selector.project_id",
            );
            for alias in ["project_id", "project_path", "project_root", "root"] {
                assert!(
                    !properties.contains_key(alias),
                    "{tool_name} must not expose legacy selector alias {alias}",
                );
            }
        }
        assert_eq!(
            &projected, canonical,
            "{tool_name} must project its canonical executable request schema before MCP transport fields",
        );
    }
}

#[test]
fn canonical_and_retired_tools_keep_truthful_discovery() {
    let tools = get_tool_definitions().expect("tool definitions");
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for operation in ApplicationSurfaceOperation::ALL {
        let tool_name = format!("tracedecay_{}", operation.as_str());
        assert!(
            tool_names.contains(tool_name.as_str()),
            "{tool_name} must be projected from the application registry"
        );
    }

    for retired in [
        "tracedecay_fact_store",
        "tracedecay_memory_automation_run",
        "tracedecay_session_start",
        "tracedecay_session_end",
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
    ] {
        assert!(
            !tool_names.contains(retired),
            "retired tool {retired} must not be advertised"
        );
    }

    assert!(tool_names.contains("tracedecay_ast_grep_search"));
    assert_eq!(
        tool_names.contains("tracedecay_ast_grep_rewrite"),
        tracedecay_mcp::ast_grep_available(),
        "CLI-backed rewrite discovery must match host availability"
    );
}

#[test]
fn fact_store_curate_exposes_only_caller_owned_bounds() {
    let tools = get_tool_definitions().expect("tool definitions");
    let tool = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_store_curate")
        .expect("fact_store_curate must be advertised");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("fact_store_curate request properties");
    assert_eq!(properties.len(), 3);
    for bound in ["fact_review_limit", "min_confidence_millionths", "format"] {
        assert!(properties.contains_key(bound));
    }
    assert_eq!(properties["fact_review_limit"]["minimum"], 1);
    assert_eq!(properties["fact_review_limit"]["maximum"], 1_000);
    assert_eq!(
        properties["min_confidence_millionths"]["maximum"],
        1_000_000
    );
    for forbidden in [
        "operations",
        "proposal",
        "approve",
        "apply",
        "run_id",
        "task",
    ] {
        assert!(!properties.contains_key(forbidden));
    }
}

/// Removing a canonical Work operation from MCP discovery would leave the
/// HTTP owner callable while making the same supported application journey
/// undiscoverable to MCP clients.
#[test]
fn work_definitions_cover_the_canonical_operation_registry() {
    let registry = tracedecay_application::work_executable_binding_registry().unwrap();
    let work_definitions = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .filter(|definition| definition.name.starts_with("tracedecay_work_"))
        .collect::<Vec<_>>();

    assert_eq!(
        work_definitions.len(),
        tracedecay_api::WorkOperation::ALL.len()
    );
    let canonical_reads = tracedecay_api::WorkOperation::ALL
        .iter()
        .filter(|operation| operation.is_read_only())
        .count();
    assert_eq!(
        work_definitions
            .iter()
            .filter(
                |definition| definition.annotations.as_ref().and_then(|annotations| {
                    annotations
                        .get("readOnlyHint")
                        .and_then(serde_json::Value::as_bool)
                }) == Some(true)
            )
            .count(),
        canonical_reads,
        "all and only canonical Work reads must carry readOnlyHint",
    );
    for operation in tracedecay_api::WorkOperation::ALL {
        let tool_name = format!("tracedecay_work_{}", operation.operation_key());
        let definition = work_definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is missing from MCP discovery"));
        assert_eq!(
            definition
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(serde_json::Value::as_bool),
            Some(operation.is_read_only()),
            "{tool_name} read-only annotation must match the canonical Work operation",
        );
        let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
            .expect("canonical Work operation identity");
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("canonical Work operation must be executable");
        assert_eq!(
            &definition.input_schema,
            binding.request_schema().body(),
            "{tool_name} must expose the exact executable request schema",
        );
    }
}

#[test]
fn test_tool_definitions_have_schemas() {
    let tools = get_tool_definitions().expect("tool definitions");
    for tool in &tools {
        assert!(!tool.name.is_empty());
        assert!(!tool.description.is_empty());
        assert!(tool.input_schema.is_object());
        assert_eq!(tool.input_schema["type"], "object");
    }
}

#[test]
fn format_capable_tools_advertise_markdown_json_without_tables() {
    let tools = get_tool_definitions().expect("tool definitions");
    for tool_name in tracedecay_mcp::format_capable_tool_names() {
        if *tool_name == "tracedecay_ast_grep_rewrite" && !tracedecay_mcp::ast_grep_available() {
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
    let tools = get_tool_definitions().expect("tool definitions");
    for operation in ApplicationSurfaceOperation::ALL {
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
    let tools = get_tool_definitions().expect("tool definitions");
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
    let tools = get_tool_definitions().expect("tool definitions");
    let write_tools = [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_rename_symbol",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_source_edit_reconcile",
        "tracedecay_source_edit_rollback",
        "tracedecay_git_apply",
        "tracedecay_approve_native_integration",
        "tracedecay_apply_native_integration",
        "tracedecay_cancel_native_integration",
        "tracedecay_run_affected_tests",
        "tracedecay_fact_store_add",
        "tracedecay_fact_store_curate",
        "tracedecay_fact_store_update",
        "tracedecay_fact_store_remove",
        "tracedecay_fact_store_supersede",
        "tracedecay_fact_feedback",
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "tracedecay_worktree_cleanup_remove",
        "tracedecay_session_refresh",
        "tracedecay_session_refresh_begin",
        "tracedecay_session_refresh_cancel",
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
        "tracedecay_dashboard",
    ];
    for tool in &tools {
        let ann = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} missing annotations", tool.name));
        let name = tool.name.as_str();
        // Work and Workflow project readOnlyHint from their executable
        // registries; those families are pinned by their own discovery tests.
        let registry_projected =
            name.starts_with("tracedecay_work_") || name.starts_with("tracedecay_workflow_");
        if write_tools.contains(&name) {
            assert_eq!(
                ann["readOnlyHint"], false,
                "{} should have readOnlyHint=false",
                tool.name
            );
        } else if !registry_projected {
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
fn memory_status_discovery_matches_its_pure_read_owner() {
    let status = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .find(|tool| tool.name == "tracedecay_memory_status")
        .expect("memory status definition");
    assert_eq!(status.annotations.unwrap()["readOnlyHint"], true);
    assert!(
        status
            .description
            .contains("Inspect canonical memory state"),
        "read-only discovery must advertise a status snapshot, not a mutation"
    );
    assert!(
        !status.description.contains("repair"),
        "memory status no longer shares an owner with holographic repair"
    );
}

#[test]
fn advertised_read_only_matches_canonical_execution_effect() {
    let catalog = crate::mcp::tools::binding::mcp_dispatch_catalog().expect("MCP dispatch catalog");
    for tool in get_tool_definitions().expect("tool definitions") {
        if INTERNAL_DAEMON_TOOL_NAMES.contains(&tool.name.as_str()) {
            continue;
        }
        let advertised_read_only = tool
            .annotations
            .as_ref()
            .and_then(|annotations| annotations["readOnlyHint"].as_bool())
            .unwrap_or(false);
        let contract = catalog
            .contract(&tool.name)
            .unwrap_or_else(|| panic!("{} missing dispatch contract", tool.name));
        assert_eq!(
            advertised_read_only,
            contract.read_only(),
            "{} advertises readOnlyHint={advertised_read_only} but its canonical execution \
             contract says read_only={}",
            tool.name,
            contract.read_only()
        );
    }
}

#[test]
fn lcm_doctor_exposes_diagnostics_only() {
    let tools = get_tool_definitions().expect("tool definitions");
    let doctor = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_doctor")
        .expect("LCM Doctor definition");
    let properties = doctor.input_schema["properties"]
        .as_object()
        .expect("LCM Doctor properties");

    for removed in [
        "mode",
        "apply",
        "doctor_clean_apply_enabled",
        "lcm_gc_apply_enabled",
        "gc_config",
        "ignore_session_patterns",
        "stateless_session_patterns",
        "ignore_message_patterns",
    ] {
        assert!(
            !properties.contains_key(removed),
            "read-only Doctor must not accept `{removed}`"
        );
    }
    assert_eq!(
        doctor.annotations.as_ref().unwrap()["readOnlyHint"],
        json!(true)
    );
}

#[test]
fn test_always_load_tools() {
    let tools = get_tool_definitions().expect("tool definitions");
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
