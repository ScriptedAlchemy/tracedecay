use serde_json::json;
use tracedecay_tool_catalog::OperationId;

use super::super::get_tool_definitions;
use super::*;

#[test]
fn retired_simplify_scan_is_absent_from_the_public_catalog() {
    let retired = "tracedecay_simplify_scan";
    assert!(
        get_tool_definitions()
            .expect("tool definitions")
            .iter()
            .all(|definition| definition.name != retired)
    );
    assert!(
        crate::mcp::tools::binding::mcp_dispatch_catalog()
            .expect("MCP dispatch catalog")
            .contract(retired)
            .is_none()
    );
}

#[test]
fn terminal_application_definitions_project_canonical_request_schemas() {
    let registry = tracedecay_contracts::mcp_executable_binding_registry()
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
            projected,
            tracedecay_mcp::mcp_input_schema(canonical),
            "{tool_name} must project its canonical executable request schema before MCP transport fields",
        );
    }
}

#[test]
fn work_proposal_tools_expose_only_the_dispositions_their_routes_accept() {
    let tools = get_tool_definitions().expect("tool definitions");
    for (tool_name, disposition_schema, expected) in [
        (
            "tracedecay_work_review_proposal",
            "ReviewWorkProposalDispositionV1",
            json!(["rejected", "superseded"]),
        ),
        (
            "tracedecay_work_accept_proposal",
            "AcceptWorkProposalDispositionV1",
            json!(["accepted"]),
        ),
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} must be advertised"));
        assert_eq!(
            tool.input_schema["properties"]["disposition"]["$ref"],
            format!("#/$defs/{disposition_schema}"),
            "{tool_name} must expose its route-specific disposition schema"
        );
        assert_eq!(
            tool.input_schema["$defs"][disposition_schema]["enum"], expected,
            "{tool_name} must not advertise a disposition its handler refuses"
        );
    }
}

#[test]
fn diagnostics_public_name_preserves_one_shipped_flat_request_schema() {
    let registry = tracedecay_contracts::mcp_executable_binding_registry()
        .expect("MCP executable binding registry");
    let operation_id = OperationId::new("operation.application.diagnostics_read")
        .expect("diagnostics operation id");
    let canonical = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .expect("diagnostics executable binding")
        .request_schema()
        .body();
    let definitions = get_tool_definitions().expect("tool definitions");
    let diagnostics = definitions
        .iter()
        .filter(|definition| {
            matches!(
                definition.name.as_str(),
                "tracedecay_diagnostics" | "tracedecay_diagnostics_read"
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        diagnostics.len(),
        1,
        "diagnostics must have one public tool name"
    );
    assert_eq!(diagnostics[0].name, "tracedecay_diagnostics");
    let mut projected = diagnostics[0].input_schema.clone();
    let properties = projected["properties"]
        .as_object_mut()
        .expect("diagnostics request properties");
    assert!(properties.remove("format").is_some());
    assert_eq!(
        properties["scope"]["enum"],
        json!(["workspace", "file"]),
        "the public diagnostics tool must preserve its shipped flat scope"
    );
    assert_eq!(properties["path"]["type"], "string");
    assert_ne!(
        &projected, canonical,
        "only the MCP/CLI edge keeps the shipped flat request; the executable remains canonical"
    );
}

#[test]
fn canonical_and_retired_tools_keep_truthful_discovery() {
    let tools = get_tool_definitions().expect("tool definitions");
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for operation in ApplicationSurfaceOperation::ALL {
        let tool_name = operation.mcp_tool_name();
        assert!(
            tool_names.contains(tool_name),
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
    let registry = tracedecay_contracts::work_executable_binding_registry().unwrap();
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
            definition.input_schema,
            tracedecay_mcp::mcp_input_schema(binding.request_schema().body()),
            "{tool_name} must expose the exact executable request schema",
        );
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
