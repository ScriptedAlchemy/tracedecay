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
fn retired_unused_import_scan_is_absent_while_diagnostic_reads_remain() {
    let definitions = get_maximal_tool_definitions().expect("tool definitions");
    eprintln!("maximal source catalog count: {}", definitions.len());

    assert!(
        definitions
            .iter()
            .all(|definition| definition.name != "tracedecay_unused_imports")
    );
    for name in ["tracedecay_diagnose", "tracedecay_diagnostics"] {
        assert!(
            definitions.iter().any(|definition| definition.name == name),
            "{name} must remain available for compiler and published diagnostics"
        );
    }
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
        "#/$defs/NativeIntegrationSelectionDeclarationV1"
    );
    let selection = &definition.input_schema["$defs"]["NativeIntegrationSelectionDeclarationV1"];

    assert_eq!(selection["oneOf"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        selection["oneOf"][0]["properties"]["kind"]["const"],
        "declared_stack_edge"
    );
    let required = selection["oneOf"][0]["properties"]["binding"]["required"]
        .as_array()
        .expect("declared stack fields");
    assert!(required.contains(&json!("nodes")));
    assert!(required.contains(&json!("edges")));
    assert!(!required.contains(&json!("canonical_order")));
    assert!(!required.contains(&json!("digest")));
    assert_eq!(
        selection["oneOf"][1]["properties"]["kind"]["const"],
        "independent_branch"
    );
}

/// `health_read` takes no parameters, so `{}` is the whole request.
///
/// The advertised MCP schema and the reviewed request contract are the same
/// authority, and CLI and MCP reach it through one adapter. Grading the
/// advertised schema and that adapter together is what keeps an argument a
/// host may legitimately send from being accepted on one surface and refused
/// on another.
#[test]
fn health_read_accepts_the_empty_argument_object_on_every_surface() {
    let definition = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .find(|definition| definition.name == "tracedecay_health_read")
        .expect("health read is advertised");
    assert_eq!(
        definition.input_schema["required"]
            .as_array()
            .map_or(0, Vec::len),
        0,
        "the advertised health read schema must require no argument: {}",
        definition.input_schema
    );

    let adapted = tracedecay_daemon_protocol::adapt_application_tool_request(
        "tracedecay_health_read",
        json!({}),
    )
    .expect("the shared CLI/MCP adapter accepts the empty object");
    assert_eq!(adapted.request, json!({}));
    let request = tracedecay_daemon_protocol::parse_application_surface_request(
        ApplicationSurfaceOperation::HealthRead,
        adapted.request,
    )
    .expect("the reviewed health read contract accepts the empty object");
    assert!(request.matches(ApplicationSurfaceOperation::HealthRead));
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

/// The MCP tool catalog is static per build, so `tools/list` must not
/// re-assemble every JSON schema on each request.
///
/// Falsifiable on a count, never a duration: the assembly counter may advance
/// at most once for the whole process, however many callers ask for it.
#[test]
fn maximal_tool_definitions_are_assembled_once_per_process() {
    use std::sync::atomic::Ordering;

    let first = get_maximal_tool_definitions().expect("tool definitions");
    // Read the baseline *after* the first call so the one legitimate build is
    // already counted; a cached registry can never advance it again.
    let baseline = MAXIMAL_DEFINITION_BUILDS.load(Ordering::SeqCst);

    for _ in 0..8 {
        let again = get_maximal_tool_definitions().expect("tool definitions");
        assert_eq!(
            again.len(),
            first.len(),
            "the cached registry must serve the same tool set"
        );
    }

    assert_eq!(
        MAXIMAL_DEFINITION_BUILDS.load(Ordering::SeqCst),
        baseline,
        "the maximal tool registry was re-assembled after it had already been \
         built; tools/list rebuilds the whole catalog per request"
    );
}

/// Caching the registry must not freeze anything session-scoped into it.
///
/// The per-session passes mutate the vector they are handed, so every caller
/// has to receive an independent clone. If the cache handed out shared state,
/// one session's context budget would be visible to the next.
#[test]
fn per_session_budget_does_not_leak_through_the_cached_registry() {
    fn context_description(definitions: &[ToolDefinition]) -> String {
        definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_context")
            .map(|definition| definition.description.clone())
            .expect("tracedecay_context is advertised")
    }

    let small = get_tool_definitions_with_budget(11, 2).expect("tool definitions");
    let small_description = context_description(&small);
    assert!(
        small_description.contains("2 calls maximum"),
        "budget must reach the context description: {small_description}"
    );

    let large = get_tool_definitions_with_budget(999_999, 9).expect("tool definitions");
    let large_description = context_description(&large);
    assert!(
        large_description.contains("9 calls maximum"),
        "budget must reach the context description: {large_description}"
    );

    assert_ne!(
        small_description, large_description,
        "two sessions with different budgets must not share one description"
    );
    assert_eq!(
        context_description(&small),
        small_description,
        "the earlier session's definitions must not be rewritten by a later one"
    );

    // A third, unbudgeted read must still see the neutral registry.
    let neutral = get_tool_definitions().expect("tool definitions");
    let neutral_description = context_description(&neutral);
    assert!(
        !neutral_description.contains("9 calls maximum"),
        "an unbudgeted caller inherited another session's budget: {neutral_description}"
    );
}
