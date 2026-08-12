use crate::support::*;
use serde_json::{Value, json};
use tracedecay::mcp::get_tool_definitions;
use tracedecay::tracedecay::TraceDecay;
#[test]
fn outline_schema_requires_file_without_provider_property() {
    let tools = get_tool_definitions().expect("tool definitions");
    let schema = tool_schema(&tools, "tracedecay_outline");

    assert_eq!(required_args_at(schema, &[]), vec!["file"]);
    assert!(
        schema["properties"]
            .as_object()
            .is_some_and(|properties| !properties.contains_key("provider")),
        "tracedecay_outline should not advertise a provider property: {schema}"
    );
}

#[tokio::test]
async fn schema_required_arguments_match_representative_handler_parsers() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let tools = get_tool_definitions().expect("tool definitions");

    // Direct `args.get(...).ok_or(...)` parser style.
    assert_schema_requires(&tools, "tracedecay_search", &["query"]);
    expect_missing_argument_error(
        &cg,
        "tracedecay_search",
        json!({}),
        "missing required parameter: query",
    )
    .await;

    // Shared helper parser style, including canonical node_id despite id alias support.
    assert_schema_requires(&tools, "tracedecay_callers", &["node_id"]);
    expect_missing_argument_error(
        &cg,
        "tracedecay_callers",
        json!({}),
        "missing required parameter: node_id",
    )
    .await;

    // Non-empty array parser style.
    assert_schema_requires(&tools, "tracedecay_callers_for", &["node_ids"]);
    expect_missing_argument_error(&cg, "tracedecay_callers_for", json!({}), "node_ids").await;

    // Multi-field edit parser style.
    assert_schema_requires(
        &tools,
        "tracedecay_insert_at",
        &["path", "anchor", "content"],
    );
    expect_missing_argument_error(
        &cg,
        "tracedecay_insert_at",
        json!({ "path": "src/lib.rs" }),
        "missing required parameter: anchor",
    )
    .await;

    // Exact fact routes project only their operation-specific required fields.
    for (tool_name, required_arg, expected_message) in [
        (
            "tracedecay_fact_store_add",
            "content",
            "missing required parameter: content",
        ),
        (
            "tracedecay_fact_store_search",
            "query",
            "missing required parameter: query",
        ),
        (
            "tracedecay_fact_store_probe",
            "entity",
            "missing required parameter: entity",
        ),
        (
            "tracedecay_fact_store_related",
            "entity",
            "missing required parameter: entity",
        ),
        (
            "tracedecay_fact_store_get",
            "fact_id",
            "missing required parameter: fact_id",
        ),
        (
            "tracedecay_fact_store_update",
            "fact_id",
            "missing required parameter: fact_id",
        ),
        (
            "tracedecay_fact_store_remove",
            "fact_id",
            "missing required parameter: fact_id",
        ),
    ] {
        assert_schema_requires(&tools, tool_name, &[required_arg]);
        expect_missing_argument_error(&cg, tool_name, json!({}), expected_message).await;
    }

    // Nested-object parser style.
    assert_schema_requires(
        &tools,
        "tracedecay_lcm_expand",
        &["provider", "session_id", "target"],
    );
    let expand = tool_schema(&tools, "tracedecay_lcm_expand");
    let target_branches = expand["properties"]["target"]["oneOf"]
        .as_array()
        .expect("closed target branches");
    assert_eq!(target_branches.len(), 3);
    assert_eq!(target_branches[0]["required"], json!(["kind", "store_id"]));
    assert_eq!(target_branches[1]["required"], json!(["kind", "node_id"]));
    assert_eq!(
        target_branches[2]["required"],
        json!(["kind", "payload_ref"])
    );
    expect_missing_argument_error(
        &cg,
        "tracedecay_lcm_expand",
        json!({ "provider": "cursor", "session_id": "session-1", "target": {} }),
        "target.kind must be one of raw_message, summary_node, external_payload",
    )
    .await;
}

#[test]
fn lcm_tool_schemas_are_registered_with_stable_names() {
    let tools = get_tool_definitions().expect("tool definitions");
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for expected in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
        "tracedecay_lcm_doctor",
    ] {
        assert!(names.contains(expected), "missing {expected}");
    }

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

    for retired in [
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
    ] {
        assert!(
            !names.contains(retired),
            "{retired} must remain daemon-internal"
        );
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
        load.input_schema["properties"]["provider"]["description"]
            .as_str()
            .unwrap()
            .contains("across all providers")
    );
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
    assert!(
        grep.input_schema["properties"]["provider"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("all providers")
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
    assert_eq!(doctor.annotations.as_ref().unwrap()["readOnlyHint"], true);
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
    assert!(properties.contains_key("project_selector"));
    for alias in ["project_id", "project_path", "project_root", "root"] {
        assert!(!properties.contains_key(alias));
    }
    assert!(!properties.contains_key("retrieve_handle"));
    assert_eq!(retrieve.input_schema["required"], json!(["handle"]));
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

    assert!(retrieve.description.contains("tracedecay_retrieve"));
    assert!(retrieve.description.contains("required argument `handle`"));
    assert!(retrieve.description.contains("pass the same selector"));
    assert!(
        retrieve
            .description
            .contains("Only call it when the missing details are needed")
    );
    assert!(
        properties["handle"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("required `handle` argument")
    );
}

#[test]
fn always_loaded_graph_tool_schemas_match_project_selector_authority() {
    let tools = get_tool_definitions().expect("tool definitions");

    // Registered-project readers dispatch to other mounted projects, so the
    // selector has to be discoverable from the schema.
    for name in ["tracedecay_context", "tracedecay_grep", "tracedecay_read"] {
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
    let registry = tracedecay_application::mcp_executable_binding_registry()
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
        "fact_store_list",
        "fact_feedback",
        "memory_status",
    ] {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(format!("operation.application.{operation}"))
                .expect("fact-store operation id");
        let mut canonical = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .unwrap_or_else(|| panic!("{operation} executable binding"))
            .request_schema()
            .body()
            .clone();
        let tool_name = format!("tracedecay_{operation}");
        let mut advertised = tool_schema(&tools, &tool_name).clone();
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
    let tool_names: std::collections::HashSet<_> =
        tools.iter().map(|tool| tool.name.as_str()).collect();
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
    assert_eq!(
        fact_add.input_schema["properties"]["trust"]["type"],
        "number"
    );
    assert_eq!(fact_add.input_schema["properties"]["trust"]["minimum"], 0);
    assert_eq!(fact_add.input_schema["properties"]["trust"]["maximum"], 1);

    assert!(
        !tool_names.contains("tracedecay_record_decision"),
        "unshipped legacy decision tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tracedecay_record_code_area"),
        "unshipped legacy code-area tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tracedecay_session_recall"),
        "unshipped legacy recall tool should not be exposed"
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
        list.input_schema["properties"]["state"]["enum"],
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

pub(crate) fn tool_schema<'a>(
    tools: &'a [tracedecay::mcp::ToolDefinition],
    name: &str,
) -> &'a Value {
    &tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("missing tool definition for {name}"))
        .input_schema
}

pub(crate) fn required_args_at<'a>(schema: &'a Value, path: &[&str]) -> Vec<&'a str> {
    let mut node = schema;
    for segment in path {
        node = &node["properties"][*segment];
    }
    node["required"]
        .as_array()
        .unwrap_or_else(|| panic!("schema path {path:?} is missing a required array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("schema path {path:?} has non-string required entry"))
        })
        .collect()
}

pub(crate) fn assert_schema_requires(
    tools: &[tracedecay::mcp::ToolDefinition],
    tool_name: &str,
    expected: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let actual = required_args_at(schema, &[]);
    assert_eq!(
        actual, expected,
        "{tool_name} schema required arguments drifted from handler parser expectations"
    );
}

pub(crate) async fn expect_missing_argument_error(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    expected_message: &str,
) {
    let message = expect_tool_error(handle_tool_call(cg, tool_name, args, None, None).await);
    assert!(
        message.contains(expected_message),
        "{tool_name} parser error should mention `{expected_message}`, got `{message}`"
    );
}
