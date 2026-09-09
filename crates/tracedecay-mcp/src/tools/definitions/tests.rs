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
