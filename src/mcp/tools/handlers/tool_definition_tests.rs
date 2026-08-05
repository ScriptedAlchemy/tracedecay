use serde_json::json;

use super::super::get_tool_definitions;
use super::*;

fn get_catalog_discovery_tool_definitions() -> Vec<super::super::ToolDefinition> {
    let profile_id =
        ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("default MCP discovery profile");
    super::super::definitions::get_catalog_filtered_tool_definitions_with_budget(
        0,
        super::super::explore_call_budget(0),
        &profile_id,
        &super::super::definitions::default_catalog_discovery_authority()
            .expect("default MCP discovery authority"),
        &super::super::definitions::project_catalog_discovery_scope(),
        super::super::definitions::ToolRegistryMode::HostAvailable,
    )
    .expect("catalog-filtered MCP discovery definitions")
}

#[test]
fn test_tool_definitions_complete() {
    let tools = get_tool_definitions();
    assert!(
        !tools.is_empty(),
        "MCP discovery must expose at least one tool"
    );
    let mut names = std::collections::BTreeSet::new();
    for tool in &tools {
        assert!(
            names.insert(&tool.name),
            "duplicate MCP tool definition: {}",
            tool.name
        );
        assert!(
            tool.name.starts_with("tracedecay_"),
            "MCP tool definition has no tracedecay namespace: {}",
            tool.name
        );
    }
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
    let tools = get_catalog_discovery_tool_definitions();
    for tool in &tools {
        let ann = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} missing annotations", tool.name));
        let dispatch = tool
            .meta
            .as_ref()
            .and_then(|metadata| metadata.get("tracedecay/dispatch"))
            .unwrap_or_else(|| panic!("{} missing canonical dispatch metadata", tool.name));
        let effect = dispatch["effect"]
            .as_str()
            .unwrap_or_else(|| panic!("{} missing canonical dispatch effect", tool.name));
        let read_only = dispatch["read_only"]
            .as_bool()
            .unwrap_or_else(|| panic!("{} missing canonical dispatch read_only", tool.name));
        assert_eq!(
            read_only,
            matches!(effect, "read" | "preview"),
            "{} dispatch effect/read_only metadata disagrees",
            tool.name
        );
        assert_eq!(
            ann["readOnlyHint"],
            json!(read_only),
            "{} readOnlyHint must follow canonical dispatch metadata",
            tool.name
        );
        assert!(
            ann["title"].is_string(),
            "{} missing title annotation",
            tool.name
        );
    }
}

#[test]
fn dashboard_start_stop_is_advertised_as_an_effect() {
    let dashboard = get_catalog_discovery_tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "tracedecay_dashboard")
        .expect("dashboard definition");
    assert_eq!(
        dashboard
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("readOnlyHint")),
        Some(&json!(false))
    );
    assert_eq!(
        dashboard.input_schema["properties"]["action"]["enum"],
        json!(["start", "stop"])
    );
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
}

#[test]
fn test_tool_definitions_serializable() {
    let tools = get_tool_definitions();
    let json = serde_json::to_string(&tools).unwrap();
    assert!(json.contains("tracedecay_search"));
    assert!(json.contains("tracedecay_status"));
}
