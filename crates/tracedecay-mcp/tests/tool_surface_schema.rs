//! The tool surface advertised to MCP hosts: description invariants, JSON
//! request schemas, and read-only annotations.
//!
//! Every tool advertised by the MCP registry is rendered by hosts through its
//! description, so the description invariants the host cares about are
//! asserted here: non-empty and within a sane length cap. The schema tests keep
//! advertised `required` arrays and closed shapes in step with the parsers and
//! the canonical operation bindings.

use serde_json::{Value, json};
use tracedecay_mcp::get_tool_definitions;

#[test]
fn every_tool_description_is_non_empty_and_within_cap() {
    // Host cap on tool description length. Generous. It exists to catch
    // runaway description growth, not to force terseness.
    const MAX_DESCRIPTION_CHARS: usize = 8192;
    let tools = get_tool_definitions().expect("tool definitions");
    for tool in &tools {
        let desc = tool.description.trim();
        assert!(
            !desc.is_empty(),
            "tool '{}' has an empty description; hosts render this as the \
             tool's only routing signal",
            tool.name
        );
        assert!(
            tool.description.chars().count() <= MAX_DESCRIPTION_CHARS,
            "tool '{}' description is {} chars, over the {MAX_DESCRIPTION_CHARS}-char host cap",
            tool.name,
            tool.description.chars().count()
        );
    }
}

#[test]
fn lcm_tool_schemas_are_read_only_and_closed() {
    let tools = get_tool_definitions().expect("tool definitions");

    for read_only in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
        "tracedecay_lcm_doctor",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == read_only)
            .unwrap_or_else(|| panic!("{read_only} definition"));
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], true);
    }

    for name in ["tracedecay_lcm_describe", "tracedecay_lcm_expand"] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        assert_eq!(tool.input_schema["additionalProperties"], false);
        for branch in tool.input_schema["properties"]["target"]["oneOf"]
            .as_array()
            .unwrap()
        {
            assert_eq!(branch["additionalProperties"], false);
        }
    }

    for scoped in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == scoped)
            .unwrap_or_else(|| panic!("{scoped} definition"));
        assert_eq!(
            tool.input_schema["properties"]["storage_scope"]["enum"],
            json!(["project", "user"]),
            "{scoped} must expose only the project and user session stores"
        );
        assert!(
            tool.input_schema["properties"].get("hermes_home").is_none(),
            "{scoped} must not expose a Hermes-owned storage path"
        );
    }

    let load = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_load_session")
        .expect("tracedecay_lcm_load_session definition");
    assert_eq!(load.input_schema["required"], json!(["session_id"]));
    assert!(
        load.input_schema["properties"]
            .get("content_limit")
            .is_some()
    );
    assert_eq!(
        load.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );
    assert_eq!(
        load.input_schema["properties"]["content_limit"]["maximum"],
        json!(20000)
    );

    let grep = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_grep")
        .expect("tracedecay_lcm_grep definition");
    assert!(
        !grep
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| required.iter().any(|field| field == "provider")),
        "tracedecay_lcm_grep provider must stay optional"
    );
    assert_eq!(
        grep.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );

    let expand = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_expand")
        .expect("tracedecay_lcm_expand definition");
    assert_eq!(
        expand.input_schema["required"],
        json!(["provider", "session_id", "target"])
    );
    let target_variants = expand.input_schema["properties"]["target"]["oneOf"]
        .as_array()
        .expect("expand target must be a discriminated union");
    let raw_message_target = target_variants
        .iter()
        .find(|target| target["properties"]["kind"]["const"] == "raw_message")
        .expect("expand target must include raw messages");
    assert_eq!(
        raw_message_target["properties"]["store_id"]["type"],
        json!("integer")
    );
    assert!(
        expand.input_schema["properties"]
            .get("source_offset")
            .is_none(),
        "numeric continuation must not remain in the public schema"
    );
    assert_eq!(
        expand.input_schema["properties"]["source_limit"]["type"],
        json!("integer")
    );
    assert_eq!(
        expand.input_schema["properties"]["cursor"]["type"],
        json!("string")
    );

    let doctor = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_doctor")
        .expect("tracedecay_lcm_doctor definition");
    // Daemon-owned doctor reads a redacted temporal-store health report with
    // no provider filter: hosts cannot smuggle retired repair/clean modes or
    // provider selectors. Shared transport properties (format, storage_scope)
    // are still injected for every LCM surface.
    assert!(doctor.input_schema.get("required").is_none());
    assert!(doctor.input_schema["properties"].get("provider").is_none());
    assert!(
        doctor.input_schema["properties"]
            .get("session_id")
            .is_none()
    );
    assert!(doctor.input_schema["properties"].get("format").is_some());
    assert_eq!(
        doctor.input_schema["properties"]["storage_scope"]["enum"],
        json!(["project", "user"])
    );
    assert_eq!(doctor.input_schema["additionalProperties"], false);
}
#[test]
fn retrieve_tool_schema_requires_handle_and_canonical_project_selector() {
    let tools = get_tool_definitions().expect("tool definitions");
    let retrieve = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_retrieve")
        .expect("tracedecay_retrieve definition");
    let properties = tool_properties(&tools, "tracedecay_retrieve");

    assert!(properties.contains_key("handle"));
    assert!(properties.contains_key("offset"));
    assert!(properties.contains_key("max_chars"));
    assert!(properties.contains_key("project_selector"));
    for alias in ["project_id", "project_path", "project_root", "root"] {
        assert!(!properties.contains_key(alias));
    }
    assert!(!properties.contains_key("retrieve_handle"));
    assert_eq!(retrieve.input_schema["required"], json!(["handle"]));
    assert_eq!(properties["offset"]["default"], json!(0));
    // Pages above the response-frame budget are clamped, not refused.
    assert_eq!(properties["max_chars"]["minimum"], json!(1));
    assert_eq!(
        properties["project_selector"]["required"],
        json!(["project_id"])
    );
    assert_eq!(
        properties["project_selector"]["additionalProperties"],
        false
    );
    assert_eq!(
        properties["project_selector"]["properties"]
            .as_object()
            .expect("closed selector properties")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["project_id"]
    );
}

#[test]
fn always_loaded_graph_tool_schemas_match_project_selector_authority() {
    let tools = get_tool_definitions().expect("tool definitions");

    // Registered-project readers dispatch to other mounted projects, so the
    // selector has to be discoverable from the schema.
    for name in [
        "tracedecay_context",
        "tracedecay_grep",
        "tracedecay_callers",
    ] {
        let properties = tool_properties(&tools, name);
        assert!(properties.contains_key("project_selector"));
        for alias in ["project_id", "project_path", "project_root", "root"] {
            assert!(!properties.contains_key(alias));
        }
        assert_eq!(
            properties["project_selector"]["required"],
            json!(["project_id"])
        );
    }

    // `tracedecay_search` is authority-bound to the active project and rejects
    // selectors at dispatch, so advertising them would make the schema lie.
    let search_properties = tool_properties(&tools, "tracedecay_search");
    for key in [
        "project_selector",
        "project_id",
        "project_path",
        "project_root",
        "root",
    ] {
        assert!(
            !search_properties.contains_key(key),
            "active-project-only tracedecay_search must not advertise {key}"
        );
    }
}

#[test]
fn exact_fact_store_definitions_project_canonical_request_schemas() {
    let registry = tracedecay_contracts::mcp_executable_binding_registry()
        .expect("MCP executable binding registry");
    let tools = get_tool_definitions().expect("tool definitions");
    for operation in [
        "fact_store_add",
        "fact_store_search",
        "fact_store_probe",
        "fact_store_related",
        "fact_store_reason",
        "fact_store_contradict",
        "fact_store_get",
        "fact_store_update",
        "fact_store_remove",
        "fact_store_supersede",
        "fact_store_list",
        "fact_feedback",
        "memory_status",
    ] {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(format!("operation.application.{operation}"))
                .expect("fact-store operation id");
        let mut canonical = tracedecay_mcp::mcp_input_schema(
            registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{operation} executable binding"))
                .request_schema()
                .body(),
        );
        let tool_name = format!("tracedecay_{operation}");
        let mut advertised = tools
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("missing tool definition for {tool_name}"))
            .input_schema
            .clone();
        for schema in [&mut canonical, &mut advertised] {
            let properties = schema["properties"]
                .as_object_mut()
                .unwrap_or_else(|| panic!("{tool_name} properties"));
            properties.remove("format");
        }
        assert_eq!(
            advertised, canonical,
            "{tool_name} must project its canonical operation-specific request schema"
        );
    }
}

#[test]
fn exact_memory_tool_definitions_exclude_legacy_payload_aliases() {
    let tools = get_tool_definitions().expect("tool definitions");
    let fact_add = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_store_add")
        .expect("tracedecay_fact_store_add definition");
    let fact_search = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_store_search")
        .expect("tracedecay_fact_store_search definition");
    let status = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_memory_status")
        .expect("tracedecay_memory_status definition");

    assert_eq!(
        fact_add.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(
        fact_search.annotations.as_ref().unwrap()["readOnlyHint"],
        true
    );
    assert_eq!(status.annotations.as_ref().unwrap()["readOnlyHint"], true);

    for field in [
        "content",
        "entities",
        "category",
        "tags",
        "trust",
        "source_label",
        "metadata",
    ] {
        assert!(fact_add.input_schema["properties"].get(field).is_some());
    }
    for alias in ["entity", "source", "action"] {
        assert!(fact_add.input_schema["properties"].get(alias).is_none());
    }
    assert!(fact_add.input_schema["properties"].get("action").is_none());
    assert!(
        fact_search.input_schema["properties"]
            .get("action")
            .is_none()
    );
    assert!(
        fact_search.input_schema["properties"]
            .get("content")
            .is_none()
    );
    let trust_type = &fact_add.input_schema["properties"]["trust"]["type"];
    assert!(
        trust_type == "number" || *trust_type == serde_json::json!(["number", "null"]),
        "trust must be a number (nullable Option<f64> is also accepted): {trust_type}"
    );
}

#[test]
fn managed_skill_tool_definitions_are_read_only() {
    let tools = get_tool_definitions().expect("tool definitions");
    let artifact = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_automation_run_artifact_view")
        .expect("tracedecay_automation_run_artifact_view definition");
    let list = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_skill_list")
        .expect("tracedecay_skill_list definition");
    let view = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_skill_view")
        .expect("tracedecay_skill_view definition");
    assert_eq!(artifact.annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(artifact.input_schema["required"], json!(["run_id", "kind"]));
    assert_eq!(list.annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(view.annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(
        list.input_schema["properties"]["state"]["anyOf"],
        json!([{"$ref": "#/$defs/ManagedSkillState"}, {"type": "null"}])
    );
    assert_eq!(
        list.input_schema["$defs"]["ManagedSkillState"]["enum"],
        json!(["active", "disabled", "archived"])
    );
    assert_eq!(view.input_schema["required"], json!(["id"]));
    let hermes_skills = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_hermes_skill_bridge")
        .expect("standard Hermes skill inventory definition");
    assert!(
        hermes_skills.input_schema["properties"]
            .get("hermes_home")
            .is_none()
    );
    assert!(
        hermes_skills.input_schema["properties"]
            .get("storage_scope")
            .is_none()
    );
}

#[test]
fn message_search_provider_schema_matches_ingested_providers() {
    let tools = get_tool_definitions().expect("tool definitions");
    let message_search = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_message_search")
        .expect("tracedecay_message_search definition");

    assert_eq!(
        message_search.input_schema["properties"]["provider"]["enum"],
        serde_json::json!(tracedecay_sessions::MESSAGE_SEARCH_PROVIDER_IDS)
    );
    assert!(
        !message_search
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| required.iter().any(|field| field == "provider")),
        "tracedecay_message_search provider must stay optional"
    );
    assert_eq!(
        message_search.input_schema["properties"]["scope"]["enum"],
        serde_json::json!(["all", "parents_only", "subagents_only"])
    );
    assert_eq!(
        message_search.input_schema["properties"]["storage_scope"]["enum"],
        serde_json::json!(["project", "user"])
    );
    assert!(
        message_search.input_schema["properties"]
            .get("parent_session_id")
            .is_some()
    );
    assert!(
        message_search.input_schema["properties"]
            .get("include_subagents")
            .is_some()
    );
    assert_eq!(
        message_search.input_schema["properties"]["message_type"]["enum"],
        serde_json::json!(["all", "direct_user", "tool_result"])
    );

    let lcm_grep = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_grep")
        .expect("tracedecay_lcm_grep definition");
    assert_eq!(
        lcm_grep.input_schema["properties"]["relationship_scope"]["enum"],
        serde_json::json!(["all", "parents_only", "subagents_only"])
    );
    assert_eq!(
        lcm_grep.input_schema["properties"]["message_type"]["enum"],
        serde_json::json!(["all", "direct_user", "tool_result"])
    );
}

fn tool_properties<'a>(
    tools: &'a [tracedecay_mcp::ToolDefinition],
    name: &str,
) -> &'a serde_json::Map<String, Value> {
    tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} definition"))
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name} properties"))
}
