use super::*;
use tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1;
use tracedecay_daemon_protocol::ApplicationSurfaceRequest;

#[test]
fn work_and_workflow_advertise_every_executable_request_schema() {
    let definitions = get_maximal_tool_definitions().expect("tool definitions");
    for (family, registry) in [
        (
            "work",
            tracedecay_contracts::work_executable_binding_registry().expect("Work registry"),
        ),
        (
            "workflow",
            tracedecay_contracts::workflow_executable_binding_registry()
                .expect("Workflow registry"),
        ),
    ] {
        assert!(registry.iter().count() > 0);
        for availability in registry.iter() {
            let binding = availability.binding().expect("executable binding");
            let operation = binding
                .operation_id()
                .as_str()
                .strip_prefix(&format!("operation.{family}."))
                .expect("operation family");
            let name = format!("tracedecay_{family}_{operation}");
            let advertised = definitions
                .iter()
                .filter(|definition| definition.name == name)
                .collect::<Vec<_>>();
            assert_eq!(advertised.len(), 1, "{name}");
            assert_eq!(
                advertised[0].input_schema,
                mcp_input_schema(binding.request_schema().body()),
                "{name}"
            );
        }
    }
}

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

/// An argument object built from the published LCM schema must decode, through
/// the shared CLI/MCP adapter, to the same `as_of` cutoff on the typed request.
#[test]
fn lcm_history_reads_accept_an_as_of_cutoff() {
    let cutoff = json!({ "kind": "as_of", "cutoff": 1_700_000_000_000_000_i64 });
    for (definition, operation, arguments) in [
        (
            def_lcm_load_session(),
            ApplicationSurfaceOperation::LcmLoadSession,
            json!({ "session_id": "session-a", "temporal_mode": cutoff }),
        ),
        (
            def_lcm_grep(),
            ApplicationSurfaceOperation::LcmGrep,
            json!({ "query": "retention", "temporal_mode": cutoff }),
        ),
    ] {
        let name = definition.name.as_str();
        let schema = &definition.input_schema;
        let properties = schema["properties"].as_object().expect("properties");
        let supplied = arguments.as_object().expect("argument object");
        assert!(
            supplied.keys().all(|key| properties.contains_key(key)),
            "{name} advertises every supplied argument"
        );
        assert!(
            schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|key| key.as_str().is_some_and(|key| supplied.contains_key(key))),
            "{name} requires only supplied arguments"
        );
        let as_of = properties["temporal_mode"]["oneOf"]
            .as_array()
            .expect("temporal mode variants")
            .iter()
            .find(|variant| variant["properties"]["kind"]["const"] == "as_of")
            .expect("advertised as_of variant");
        let mut advertised_fields = as_of["required"]
            .as_array()
            .expect("as_of required fields")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        advertised_fields.sort_unstable();
        let mut supplied_fields = cutoff
            .as_object()
            .expect("cutoff object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        supplied_fields.sort_unstable();
        assert_eq!(advertised_fields, supplied_fields, "{name}");

        let adapted = tracedecay_daemon_protocol::adapt_application_tool_request(name, arguments)
            .expect("the shared CLI/MCP adapter accepts the arguments");
        let request = tracedecay_daemon_protocol::parse_application_surface_request(
            operation,
            adapted.request,
        )
        .expect("the typed LCM request accepts an as_of cutoff");
        let temporal_mode = match request {
            ApplicationSurfaceRequest::Retained(RetainedSurfaceRequestV1::LcmLoadSession(
                request,
            )) => request.temporal_mode,
            ApplicationSurfaceRequest::Retained(RetainedSurfaceRequestV1::LcmGrep(request)) => {
                request.temporal_mode
            }
            other => panic!("{name} decoded to {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(temporal_mode).expect("temporal mode serializes"),
            cutoff,
            "{name}"
        );
    }

    let legacy = tracedecay_daemon_protocol::parse_application_surface_request(
        ApplicationSurfaceOperation::LcmLoadSession,
        json!({ "session_id": "session-a", "as_of_micros": 1_700_000_000_000_000_i64 }),
    )
    .expect_err("the retired microsecond cutoff argument is refused");
    assert!(
        legacy.to_string().contains("unknown field `as_of_micros`"),
        "{legacy}"
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
        small_description.contains("2 broad context calls"),
        "budget must reach the context description: {small_description}"
    );

    let large = get_tool_definitions_with_budget(999_999, 9).expect("tool definitions");
    let large_description = context_description(&large);
    assert!(
        large_description.contains("9 broad context calls"),
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
        !neutral_description.contains("9 broad context calls"),
        "an unbudgeted caller inherited another session's budget: {neutral_description}"
    );
}

#[test]
fn status_and_skill_view_default_to_summaries() {
    let definitions = get_tool_definitions().expect("tool definitions");
    let status = definitions
        .iter()
        .find(|definition| definition.name == "tracedecay_status")
        .expect("status");
    for key in [
        "include_branch_diagnostics",
        "include_storage_health",
        "include_session_ingest",
        "include_staleness",
    ] {
        assert_eq!(
            status.input_schema["properties"][key]["default"],
            serde_json::json!(false),
            "{key}"
        );
    }
    let view = definitions
        .iter()
        .find(|definition| definition.name == "tracedecay_skill_view")
        .expect("skill view");
    assert_eq!(
        view.input_schema["properties"]["include_support_files"]["default"],
        serde_json::json!(false)
    );
}
