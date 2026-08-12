use super::*;

#[test]
fn internal_host_ingest_is_cli_resolvable_but_not_advertised() {
    assert!(
        get_tool_definitions()
            .expect("tool definitions")
            .iter()
            .all(|definition| definition.name != "tracedecay_hook_runtime")
    );
    let definition = internal_daemon_tool_definition("tracedecay_hook_runtime")
        .expect("internal host-ingest definition");
    assert_eq!(definition.name, "tracedecay_hook_runtime");
    assert_eq!(definition.input_schema, json!({ "type": "object" }));
    assert!(internal_daemon_tool_definition("tracedecay_unknown").is_none());
}

#[test]
fn multi_root_tools_are_discoverable() {
    let definitions = get_tool_definitions().expect("tool definitions");
    for name in [
        "tracedecay_multi_root_scope_set_read",
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "tracedecay_multi_root_execute",
    ] {
        assert!(
            definitions.iter().any(|definition| definition.name == name),
            "{name} must expose its daemon-owned public journey"
        );
    }
}

#[test]
fn stack_snapshot_requires_an_exact_selection_binding() {
    let definition = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .find(|definition| definition.name == "tracedecay_stack_snapshot")
        .expect("stack snapshot definition");
    assert_eq!(
        definition.input_schema["properties"]["selection"]["$ref"],
        "#/$defs/NativeIntegrationSelectionBindingV1"
    );
    let selection = &definition.input_schema["$defs"]["NativeIntegrationSelectionBindingV1"];

    assert_eq!(selection["oneOf"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        selection["oneOf"][0]["properties"]["kind"]["const"],
        "declared_stack_edge"
    );
    assert!(
        selection["oneOf"][0]["properties"]["binding"]["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("declared_revision")))
    );
    assert_eq!(
        selection["oneOf"][1]["properties"]["kind"]["const"],
        "independent_branch"
    );
}

#[test]
fn test_explore_call_budget_tiers() {
    assert_eq!(explore_call_budget(0), 3);
    assert_eq!(explore_call_budget(5000), 3);
    assert_eq!(explore_call_budget(5001), 4);
    assert_eq!(explore_call_budget(20000), 4);
    assert_eq!(explore_call_budget(20001), 5);
    assert_eq!(explore_call_budget(80000), 5);
    assert_eq!(explore_call_budget(80001), 7);
    assert_eq!(explore_call_budget(250000), 7);
    assert_eq!(explore_call_budget(250001), 10);
}

#[test]
fn test_context_description_contains_budget() {
    let desc = context_description(5000, 4);
    assert!(
        desc.contains("4 calls maximum"),
        "description should contain budget: {desc}"
    );
    assert!(
        desc.contains("5000 nodes"),
        "description should contain node count: {desc}"
    );
}

#[test]
fn context_scout_read_surfaces_are_registered_read_only() {
    let definitions = get_tool_definitions().expect("tool definitions");
    for name in [
        "tracedecay_context_scout_status",
        "tracedecay_context_scout_recent",
        "tracedecay_context_scout_explain",
        "tracedecay_context_scout_capability",
        "tracedecay_context_scout_budget",
    ] {
        let definition = definitions
            .iter()
            .find(|definition| definition.name == name)
            .expect("Context Scout read surface is registered");
        assert_eq!(
            definition
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }
}

#[test]
fn catalog_filtered_discovery_uses_the_deterministic_maximal_registry() {
    let profile_id = ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID)
        .expect("default profile");
    let definitions = get_catalog_filtered_tool_definitions_with_budget(
        0,
        explore_call_budget(0),
        &profile_id,
        &default_catalog_discovery_authority().expect("default discovery authority"),
        &project_catalog_discovery_scope(),
        ToolRegistryMode::DeterministicMaximal,
    )
    .expect("catalog-filtered definitions");

    let source_edit = definitions
        .iter()
        .find(|definition| definition.name == "tracedecay_ast_grep_rewrite")
        .expect("available source-edit handler is advertised");
    let source_edit_dispatch = &source_edit.meta.as_ref().unwrap()["tracedecay/dispatch"];
    assert_eq!(source_edit_dispatch["effect"], "source_edit");
    assert_eq!(source_edit_dispatch["availability"]["state"], "available");
    assert_eq!(source_edit_dispatch["idempotency"], "key_required");

    let fingerprints = definitions
        .iter()
        .map(|definition| {
            let dispatch = &definition.meta.as_ref().unwrap()["tracedecay/dispatch"];
            assert_eq!(dispatch["version"], 1);
            assert_eq!(
                definition.annotations.as_ref().unwrap()["readOnlyHint"],
                dispatch["read_only"]
            );
            assert!(dispatch["deadline"]["maximum_millis"].as_u64().unwrap() > 0);
            dispatch["fingerprint"].as_str().unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fingerprints.len(),
        1,
        "one catalog snapshot must fingerprint every advertised contract"
    );

    let dashboard = definitions
        .iter()
        .find(|definition| definition.name == "tracedecay_dashboard")
        .unwrap();
    let dispatch = &dashboard.meta.as_ref().unwrap()["tracedecay/dispatch"];
    assert_eq!(dispatch["effect"], "administrative");
    assert_eq!(dispatch["availability"]["state"], "available");
    assert_eq!(dispatch["idempotency"], "idempotent");
    assert_eq!(dispatch["inverse"]["mode"], "same_tool");

    let doctor = definitions
        .iter()
        .find(|definition| definition.name == "tracedecay_lcm_doctor")
        .unwrap();
    let dispatch = &doctor.meta.as_ref().unwrap()["tracedecay/dispatch"];
    assert_eq!(dispatch["effect"], "read");
    assert_eq!(dispatch["availability"]["state"], "available");
    assert!(dispatch.get("receipt").is_none());
    assert!(dispatch.get("reconciliation").is_none());

    for retired in [
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
    ] {
        assert!(
            definitions
                .iter()
                .all(|definition| definition.name != retired),
            "{retired} must remain daemon-internal"
        );
    }
}

#[test]
fn handle_gated_feedback_reads_are_advertised_with_their_request_handle() {
    let definitions = get_tool_definitions().expect("tool definitions");
    for name in [
        "tracedecay_feedback_diagnostics",
        "tracedecay_feedback_get",
        "tracedecay_feedback_expand",
        "tracedecay_feedback_list",
        "tracedecay_feedback_impact",
        "tracedecay_affected_tests",
    ] {
        let definition = definitions
            .iter()
            .find(|definition| definition.name == name)
            .unwrap_or_else(|| panic!("{name} must be advertised"));
        assert!(
            definition.input_schema["properties"]
                .get("request_handle")
                .is_some(),
            "{name} must accept the daemon-minted request handle"
        );
        assert_eq!(
            definition.input_schema["required"],
            json!(["request_handle"]),
            "{name} must require the request handle"
        );
    }
}

#[test]
fn test_context_description_scopes_budget_and_frees_narrow_tools() {
    let desc = context_description(5000, 4);
    assert!(
        desc.contains("tracedecay_context ONLY"),
        "budget must be scoped to tracedecay_context so agents don't abandon after one call: {desc}"
    );
    assert!(
        desc.contains("UNBUDGETED"),
        "description must tell agents the narrow tools are unbudgeted: {desc}"
    );
    for narrow in [
        "tracedecay_search",
        "tracedecay_grep",
        "tracedecay_callers",
        "tracedecay_body",
    ] {
        assert!(
            desc.contains(narrow),
            "description should name the narrow follow-up tool {narrow}: {desc}"
        );
    }
}

#[test]
fn test_get_tool_definitions_with_budget() {
    let defs = get_tool_definitions_with_budget(10000, 4).expect("tool definitions");
    let context_tool = defs
        .iter()
        .find(|d| d.name == "tracedecay_context")
        .unwrap();
    assert!(context_tool.description.contains("4 calls maximum"));
    assert!(context_tool.description.contains("10000 nodes"));
}

#[test]
fn catalog_filter_preserves_non_catalog_tools_and_filters_catalog_bindings() {
    let profile = ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID).unwrap();
    let definitions = get_catalog_filtered_tool_definitions_with_budget(
        10_000,
        4,
        &profile,
        &BTreeSet::new(),
        &project_catalog_discovery_scope(),
        ToolRegistryMode::HostAvailable,
    )
    .unwrap();

    assert!(
        definitions
            .iter()
            .any(|definition| definition.name == "tracedecay_context"),
        "legacy production tools remain discoverable until cataloged"
    );
    assert!(
        definitions
            .iter()
            .all(|definition| definition.name != "tracedecay_git_preview"),
        "catalog-bound tools require explicit capability authority"
    );
}

#[test]
fn lcm_compatibility_definitions_expose_only_opaque_continuation_cursors() {
    let load = def_lcm_load_session();
    let grep = def_lcm_grep();

    for definition in [&load, &grep] {
        let properties = definition.input_schema["properties"]
            .as_object()
            .expect("LCM properties");
        assert_eq!(properties["cursor"]["type"], "string");
        assert_eq!(
            properties["temporal_mode"]["enum"],
            json!(["current", "as_of", "evolution", "forensic"])
        );
        assert_eq!(properties["as_of_micros"]["minimum"], 0);
    }

    assert!(
        load.input_schema["properties"]
            .get("after_store_id")
            .is_none(),
        "legacy offset pagination must not remain public"
    );
    assert_eq!(
        grep.input_schema["properties"]["include_summaries"]["default"],
        false
    );
    assert_eq!(
        grep.input_schema["properties"]["sort"]["default"],
        "relevance"
    );
}
