//! Static reachability contract for the PR12 daemon-owned production path.

const DAEMON_SOURCE: &str = include_str!("../src/daemon.rs");
const PROJECT_OPEN_OWNERS_SOURCE: &str = include_str!("../src/daemon/project_open_owners.rs");
const INVOCATION_SOURCE: &str = include_str!("../src/daemon/service/invocation.rs");
const APPLICATION_SURFACE_SOURCE: &str = include_str!("../src/application_surface.rs");
const CLI_SURFACE_SOURCE: &str = include_str!("../src/cli/dispatch.rs");
const MCP_SURFACE_SOURCE: &str = include_str!("../src/mcp/tools/dispatch.rs");
const MCP_HANDLER_SOURCE: &str = include_str!("../src/mcp/tools/handlers/application_surface.rs");
const HTTP_SURFACE_SOURCE: &str = include_str!("../crates/tracedecay-api/src/http.rs");
const HOOK_RUNTIME_SOURCE: &str = include_str!("../src/mcp/tools/handlers/hook_runtime.rs");
const HOOK_V2_SOURCE: &str = include_str!("../src/hooks/v2.rs");
const SCOUT_OWNER_SOURCE: &str = include_str!("../src/agents/context_scout_owner.rs");

fn assert_contains_all(source: &str, names: &[&str]) {
    for name in names {
        assert!(
            source.contains(name),
            "missing PR12 production path: {name}"
        );
    }
}

fn call_offsets(source: &str, function_name: &str, callee: &str) -> Vec<usize> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(
            &tracedecay::extraction::ts_provider::language("rust")
                .expect("Rust grammar is available"),
        )
        .expect("Rust grammar loads");
    let tree = parser.parse(source, None).expect("Rust source parses");
    let mut offsets = Vec::new();
    collect_call_offsets(
        tree.root_node(),
        source.as_bytes(),
        function_name,
        callee,
        false,
        &mut offsets,
    );
    offsets
}

fn collect_call_offsets(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    function_name: &str,
    callee: &str,
    inside_target: bool,
    offsets: &mut Vec<usize>,
) {
    let inside_target = if node.kind() == "function_item" {
        node.child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
            == Some(function_name)
    } else {
        inside_target
    };
    if inside_target
        && node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .and_then(|function| function.utf8_text(source).ok())
            .is_some_and(|function| {
                function
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>()
                    .ends_with(callee)
            })
    {
        offsets.push(node.start_byte());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_offsets(child, source, function_name, callee, inside_target, offsets);
    }
}

#[test]
fn project_open_scout_bootstrap_precedes_each_cache_publication() {
    for function in ["open_project_server", "portable_project_server"] {
        let bootstrap = call_offsets(
            DAEMON_SOURCE,
            function,
            "ensure_context_scout_owner_before_advertising",
        );
        let publication = call_offsets(DAEMON_SOURCE, function, "bind_or_insert_route_bounded");
        assert_eq!(
            bootstrap.len(),
            1,
            "{function} must have one Scout bootstrap call"
        );
        assert_eq!(
            publication.len(),
            1,
            "{function} must have one cache publication"
        );
        assert!(
            bootstrap[0] < publication[0],
            "{function} must retain Scout before cache publication"
        );
    }
}

#[test]
fn project_open_registers_production_owners_after_cache_publication() {
    assert!(
        DAEMON_SOURCE.contains("mod project_open_owners"),
        "daemon must own the project-open production owner module"
    );
    for function in ["open_project_server", "portable_project_server"] {
        let publication = call_offsets(DAEMON_SOURCE, function, "bind_or_insert_route_bounded");
        let owners = call_offsets(
            DAEMON_SOURCE,
            function,
            "register_project_open_production_owners",
        );
        assert_eq!(
            publication.len(),
            1,
            "{function} must have one cache publication"
        );
        assert_eq!(
            owners.len(),
            1,
            "{function} must register production owners once"
        );
        assert!(
            publication[0] < owners[0],
            "{function} must register production owners after cache publication"
        );
    }
}

#[test]
fn project_open_mounts_concrete_cycle_lsp_advisory_and_hook_owners() {
    assert_contains_all(
        PROJECT_OPEN_OWNERS_SOURCE,
        &[
            "resolve_production_feedback_cycle_parts",
            "open_cycle_and_register",
            "build_and_register_pr12",
            "register_production",
            "production_advisory_hook_notice_sink",
            "open_pr12_production_primitive_runtime",
            "ProductionFeedbackRuntimeStateV1",
            "ProjectCiRetainedObservationStoreV1",
            "ProjectCiCodeAnchorStoreV1",
            "ConfiguredGitHubSourceAccessAuthorityV1",
        ],
    );
    assert!(
        !PROJECT_OPEN_OWNERS_SOURCE.contains("Unavailable stub"),
        "project-open must not document Unavailable stub installation"
    );
    assert!(
        !PROJECT_OPEN_OWNERS_SOURCE.contains("AuthorizationPortOutcome::Unavailable"),
        "project-open must not install Unavailable authorization shortcuts"
    );
    let feedback = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_project_open_production_owners",
        "feedback_runtime_registrar().open_and_register",
    );
    let cycle = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_production_feedback_cycle",
        "open_cycle_and_register",
    );
    let lsp = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_production_lsp_owner",
        "build_and_register_pr12",
    );
    let advisory = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_production_advisory_owner",
        "register_production",
    );
    let hooks = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_production_advisory_owner",
        "production_advisory_hook_notice_sink",
    );
    assert_eq!(feedback.len(), 1, "feedback runtime must register once");
    assert_eq!(cycle.len(), 1, "feedback cycle must register once");
    assert_eq!(lsp.len(), 1, "LSP owner must register once");
    assert_eq!(advisory.len(), 1, "advisory production must register once");
    assert_eq!(
        hooks.len(),
        1,
        "hook host-delivery sink must be constructed"
    );
}

#[test]
fn primitive_dispatch_uses_the_closed_daemon_protocol_on_every_surface() {
    assert_contains_all(
        INVOCATION_SOURCE,
        &[
            "DaemonInvocationOperation::PrimitiveImpact",
            "DaemonInvocationPayload::PrimitiveAffectedTests",
            "DaemonInvocationPayload::PrimitiveTestResults",
            "DaemonInvocationPayload::PrimitiveRead",
            "DaemonInvocationOutcome::Primitive",
            "dispatch_pr12_primitive",
            "DaemonPrimitiveRuntimeRegistrar",
        ],
    );
    assert!(
        !INVOCATION_SOURCE.contains("invoke_with_owners"),
        "legacy owner injection must not remain on the daemon invocation path"
    );
    assert_contains_all(
        APPLICATION_SURFACE_SOURCE,
        &[
            "ApplicationSurfaceOperation::FeedbackImpact",
            "ApplicationSurfaceOperation::AffectedTests",
            "ApplicationSurfaceOperation::TestResults",
            "ApplicationSurfaceOperation::SessionLookup",
            "ApplicationSurfaceOperation::DiagnosticsRead",
        ],
    );
    assert_contains_all(CLI_SURFACE_SOURCE, &["resolve_cli_application_surface"]);
    assert_contains_all(MCP_SURFACE_SOURCE, &["resolve_mcp_application_surface"]);
    assert_contains_all(
        MCP_HANDLER_SOURCE,
        &["handle_application_surface", "DaemonInvocationClient"],
    );
    assert_contains_all(
        HTTP_SURFACE_SOURCE,
        &[
            "HttpApplicationOperation::FeedbackImpact",
            "HttpApplicationOperation::AffectedTests",
            "HttpApplicationOperation::TestResults",
        ],
    );
}

#[test]
fn hook_v2_admission_is_daemon_owned_and_replayable() {
    assert_contains_all(
        HOOK_RUNTIME_SOURCE,
        &["\"hook_v2_admit\"", "hook_v2_admit", "context_scout_owner"],
    );
    assert_contains_all(
        SCOUT_OWNER_SOURCE,
        &[
            "ProjectContextScoutOwnerV1",
            "claim_ready_guidance",
            "record_delivery",
            "record_feedback",
        ],
    );
    assert_contains_all(
        HOOK_V2_SOURCE,
        &[
            "\"hook_v2_admit\"",
            "dispatch_decoded",
            "append_for_replay",
            "HookTransportDispositionV1::CatchupRequired",
        ],
    );
}
