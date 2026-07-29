//! Reachability contract for the PR12 daemon-owned production path.
//!
//! Two kinds of check live here and they answer different questions.
//!
//! `production_*` tests boot a real daemon, open a real project, and drive a
//! PR12 primitive through the shipped MCP, HTTP, and CLI surfaces. They fail
//! when the path stops executing.
//!
//! `source_*` tests are static lints over the daemon's own source. A lint can
//! only show that a construct is written down or absent, so it is used here for
//! statement ordering and forbidden-construct questions that no single runtime
//! observation answers. A lint is never evidence that a path executes; when a
//! claim is about execution, it belongs in a `production_*` test above.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};

use serde_json::Value;
use tracedecay::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, ApplicationSurfaceRequest,
    CallableCodeSurfaceMeta, CodeSymbolSearchSurfaceRequest, PrimitiveCodeSurfaceRequest,
    resolve_http_application_surface,
};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface;
use tracedecay_application::retrieval::SymbolGraphScope;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, OpaqueCursor, OperationTermination, RequestId,
    ResultProjection, RetrievalOrder,
};

const DAEMON_SOURCE: &str = include_str!("../src/daemon.rs");
const PROJECT_OPEN_OWNERS_SOURCE: &str = include_str!("../src/daemon/project_open_owners.rs");
const INVOCATION_SOURCE: &str = include_str!("../src/daemon/service/invocation.rs");
const APPLICATION_SURFACE_SOURCE: &str = include_str!("../src/application_surface.rs");
const HOOK_V2_SOURCE: &str = include_str!("../src/hooks/v2.rs");
const HOOK_DAEMON_PORTS_SOURCE: &str = include_str!("../src/hooks/daemon_ports.rs");

/// Every surface pins its page size to ten rows, so a query with more matches
/// than this must cross a page boundary to answer at all.
const SURFACE_PAGE_SIZE: usize = 10;
const PROBE_SYMBOL_COUNT: usize = 24;
const PROBE_TOKEN: &str = "pr12_pagination_probe";

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

struct ProductionFixture {
    _daemon: common::DaemonProcess,
    client: DaemonInvocationClient,
    project: std::path::PathBuf,
    _environment: common::IsolatedEnv,
}

impl ProductionFixture {
    fn home(&self) -> &Path {
        self._environment.home()
    }
}

/// Opens the shipped daemon over an indexed project whose symbol graph is large
/// enough to cross a surface page boundary.
///
/// Nothing here is a double: the daemon is the release binary, the project is
/// opened by the daemon's own project-open path, and the client is the same
/// invocation client the hosts use.
async fn production_fixture() -> ProductionFixture {
    let (environment, project) = common::IsolatedEnv::acquire().await;
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context_eval_project"),
        &project,
    );
    write_pagination_probe(&project);
    let daemon = common::spawn_tracedecay_daemon(environment.home());
    let initialized = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert_command_success("tracedecay init", &initialized);
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake).expect("daemon client");
    ProductionFixture {
        _daemon: daemon,
        client,
        project,
        _environment: environment,
    }
}

/// Writes enough uniformly named symbols that a single query cannot be answered
/// inside one page.
fn write_pagination_probe(project: &Path) {
    let mut source = String::new();
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub fn {PROBE_TOKEN}_{index:02}(input: u32) -> u32 {{\n    input + {index}\n}}\n\n"
        ));
    }
    let destination = project.join("src");
    std::fs::create_dir_all(&destination).expect("probe destination");
    std::fs::write(destination.join("pr12_pagination_probe.rs"), source)
        .expect("write the paginated symbol probe");
}

fn copy_dir(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("fixture destination");
    for entry in std::fs::read_dir(source).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy checked-in fixture");
        }
    }
}

fn assert_command_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn symbol_search_request(query: &str, cursor: Option<&str>) -> CodeSymbolSearchSurfaceRequest {
    CodeSymbolSearchSurfaceRequest {
        query: query.to_owned(),
        scope: SymbolGraphScope { path_prefix: None },
        lazy_index_ignored_dependencies: false,
        meta: CallableCodeSurfaceMeta {
            projection: ResultProjection::Summary,
            order: RetrievalOrder::Relevance,
            cursor: cursor.map(|cursor| OpaqueCursor::new(cursor.to_owned()).expect("cursor")),
        },
    }
}

fn symbol_search_surface_request(query: &str, cursor: Option<&str>) -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::SymbolSearch(
        symbol_search_request(query, cursor),
    ))
}

fn evidence_payload(result: &ApplicationSurfaceInvocationResult) -> &Value {
    let envelope = result.result.as_ref().unwrap_or_else(|problem| {
        panic!(
            "{} returned {:?}: {:?}",
            result.operation.as_str(),
            problem.problem.kind(),
            problem.problem
        )
    });
    match &envelope.outcome {
        ApplicationOutcome::Evidence(evidence) => {
            assert_eq!(
                evidence.execution.termination,
                OperationTermination::Completed
            );
            evidence.payload.as_ref().expect("evidence payload")
        }
        other => panic!("expected evidence outcome, got {other:?}"),
    }
}

fn run_symbol_search_cli(home: &Path, project: &Path, query: &str, cursor: Option<&str>) -> Output {
    let project_arg = project.to_string_lossy().into_owned();
    let arguments =
        serde_json::to_string(&symbol_search_request(query, cursor)).expect("CLI arguments");
    common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            ApplicationSurfaceOperation::CodeSymbolSearch.as_str(),
            "--args",
            arguments.as_str(),
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run code_symbol_search through the CLI")
}

/// Asserts the page is the first of several and was produced by the cursor
/// authority rather than by truncation.
fn assert_first_page_of_many(surface: &str, payload: &Value) {
    let items = payload["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{surface} symbol page items: {payload:#}"));
    let total = payload["total"]
        .as_u64()
        .unwrap_or_else(|| panic!("{surface} symbol page total: {payload:#}"));
    assert!(
        total > SURFACE_PAGE_SIZE as u64,
        "{surface} probe query must exceed one page, saw {total}"
    );
    assert_eq!(
        items.len(),
        SURFACE_PAGE_SIZE,
        "{surface} must return a full first page"
    );
    assert_eq!(payload["truncated"], Value::Bool(true), "{surface} page");
    assert!(
        !payload["next_cursor"].is_null(),
        "{surface} must issue a continuation cursor for the remaining {} rows: {payload:#}",
        total - items.len() as u64
    );
}

/// Asserts a resumed page continued from the first rather than restarting it.
fn assert_second_page_continues(surface: &str, first: &Value, second: &Value) {
    let first_items = first["items"].as_array().expect("first page items");
    let second_items = second["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{surface} resumed page items: {second:#}"));
    assert_eq!(
        second["total"], first["total"],
        "{surface} resume must read the same result set"
    );
    assert!(
        !second_items.is_empty(),
        "{surface} resume returned no rows: {second:#}"
    );
    assert_ne!(
        second_items.first(),
        first_items.first(),
        "{surface} resume restarted at the first page instead of continuing"
    );
    for item in second_items {
        assert!(
            !first_items.contains(item),
            "{surface} resume repeated a first-page row: {item:#}"
        );
    }
}

/// Project open must mount the PR12 primitive runtime, and that runtime must
/// answer a read whose result set crosses a page boundary in both directions:
/// minting a continuation and honouring it.
///
/// The daemon resolves the primitive runtime out of the registry populated by
/// `register_project_open_production_owners`, so a request that returns evidence
/// proves the registration ran. The oversized result set then forces the
/// symbol-graph cursor authority to mint a continuation: a broken authority
/// fails the whole read rather than returning a short page, which is how a
/// digest-shape defect in that authority stayed invisible to single-page reads.
/// Spending that continuation through the same shipped surface reaches
/// `resume_offset`, which no surface could call while the page was hard-coded to
/// the first one.
#[tokio::test(flavor = "multi_thread")]
async fn production_project_open_serves_a_paginated_symbol_graph_read() {
    let fixture = production_fixture().await;
    let result = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new("request.pr12-reachability.symbol-search.mcp").expect("request id"),
        symbol_search_surface_request(PROBE_TOKEN, None),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    let first = evidence_payload(&result).clone();
    assert_first_page_of_many("MCP", &first);

    let cursor = first["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("continuation cursor: {first:#}"))
        .to_owned();
    let resumed = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new("request.pr12-reachability.symbol-search.resume").expect("request id"),
        symbol_search_surface_request(PROBE_TOKEN, Some(cursor.as_str())),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    assert_second_page_continues("MCP", &first, evidence_payload(&resumed));

    // A query the page can hold must not advertise a continuation, so the
    // cursor above is the pagination path executing rather than a constant.
    let single = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new("request.pr12-reachability.symbol-search.single").expect("request id"),
        symbol_search_surface_request(&format!("{PROBE_TOKEN}_07"), None),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    let single = evidence_payload(&single);
    assert!(
        single["next_cursor"].is_null(),
        "a single-page read must not issue a cursor: {single:#}"
    );
    assert_eq!(single["truncated"], Value::Bool(false));
}

/// The MCP, HTTP, and CLI surfaces must reach the same daemon-owned primitive.
///
/// Each surface is entered through its own shipped entry point — the MCP and
/// HTTP application resolvers in-process, and the CLI as the installed binary —
/// so a surface that stops routing to the daemon fails here.
#[tokio::test(flavor = "multi_thread")]
async fn production_primitive_reads_agree_across_mcp_http_and_cli() {
    let fixture = production_fixture().await;

    let mcp = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new("request.pr12-reachability.parity.mcp").expect("request id"),
        symbol_search_surface_request(PROBE_TOKEN, None),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    let mcp = evidence_payload(&mcp).clone();
    assert_first_page_of_many("MCP", &mcp);

    let http = resolve_http_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new("request.pr12-reachability.parity.http").expect("request id"),
        symbol_search_surface_request(PROBE_TOKEN, None),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("HTTP application dispatch");
    let http = evidence_payload(&http).clone();
    assert_first_page_of_many("HTTP", &http);

    let cli = cli_symbol_search_payload(&fixture, PROBE_TOKEN, None);
    assert_first_page_of_many("CLI", &cli);

    assert_eq!(mcp["items"], http["items"], "MCP and HTTP page contents");
    assert_eq!(mcp["items"], cli["items"], "MCP and CLI page contents");
    assert_eq!(mcp["total"], cli["total"], "MCP and CLI page totals");

    // The continuation must be spendable from the installed binary, not only
    // from an in-process resolver: that is what a user or agent actually holds.
    let cursor = cli["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("CLI continuation cursor: {cli:#}"))
        .to_owned();
    let resumed = cli_symbol_search_payload(&fixture, PROBE_TOKEN, Some(cursor.as_str()));
    assert_second_page_continues("CLI", &cli, &resumed);
}

fn cli_symbol_search_payload(
    fixture: &ProductionFixture,
    query: &str,
    cursor: Option<&str>,
) -> Value {
    let output = run_symbol_search_cli(fixture.home(), &fixture.project, query, cursor);
    assert_command_success("CLI code_symbol_search", &output);
    let envelope: ApplicationEnvelope<Value> =
        serde_json::from_slice(&output.stdout).expect("CLI application envelope");
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        panic!("CLI code_symbol_search must return an evidence outcome");
    };
    assert_eq!(
        evidence.execution.termination,
        OperationTermination::Completed
    );
    evidence.payload.expect("CLI evidence payload")
}

/// Project open must retain Scout, publish the route cache, then mount owners,
/// in that order.
///
/// This is a source lint, not an execution proof. The invariant is an ordering
/// between three statements in one function, and the windows it guards — a
/// request admitted against a published route whose Scout owner or production
/// owners do not exist yet — are not observable from a completed request.
///
/// `production_project_server` is the single orchestrator behind the direct,
/// warm-up, and portable project-open entry points. The two names this lint
/// used to walk, `open_project_server` and `portable_project_server`, were
/// consolidated into it, and because nothing runs this suite the lint sat red
/// rather than reporting the drift.
#[test]
fn source_project_open_orders_scout_publication_and_owner_mounting() {
    const ORCHESTRATOR: &str = "production_project_server";
    let bootstrap = call_offsets(
        DAEMON_SOURCE,
        ORCHESTRATOR,
        "ensure_context_scout_owner_before_advertising",
    );
    let publication = call_offsets(DAEMON_SOURCE, ORCHESTRATOR, "bind_or_insert_route_bounded");
    let owners = call_offsets(
        DAEMON_SOURCE,
        ORCHESTRATOR,
        "register_project_open_production_owners",
    );
    assert_eq!(
        bootstrap.len(),
        1,
        "{ORCHESTRATOR} must have one Scout bootstrap call"
    );
    assert_eq!(
        publication.len(),
        1,
        "{ORCHESTRATOR} must have one cache publication"
    );
    assert_eq!(
        owners.len(),
        1,
        "{ORCHESTRATOR} must register production owners once"
    );
    assert!(
        bootstrap[0] < publication[0],
        "{ORCHESTRATOR} must retain Scout before cache publication"
    );
    assert!(
        publication[0] < owners[0],
        "{ORCHESTRATOR} must register production owners after cache publication"
    );
}

/// Project open must mount each owner exactly once and install no placeholder.
///
/// The exactly-once shape and the absence of an `Unavailable` shortcut are
/// source questions: a duplicate registration is absorbed by the registrars'
/// `AlreadyRegistered` arms, and a stub that answers `Unavailable` looks like an
/// honest degraded response from outside. Whether these owners are reached at
/// all is proven by the `production_*` tests above, not here.
#[test]
fn source_project_open_mounts_each_production_owner_once() {
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
    let primitive = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_project_open_production_owners",
        "open_pr12_production_primitive_runtime",
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
    // The advisory owner must take its host-delivery sink from the live Hook V2
    // notice queue. The legacy V1 sink beside it deliberately answers
    // `Unavailable`, so naming the queue's own sink is what distinguishes a
    // mounted delivery path from the retired one.
    let hooks = call_offsets(
        PROJECT_OPEN_OWNERS_SOURCE,
        "register_production_advisory_owner",
        "hook_notices.sink",
    );
    assert_eq!(feedback.len(), 1, "feedback runtime must register once");
    assert_eq!(primitive.len(), 1, "primitive runtime must open once");
    assert_eq!(cycle.len(), 1, "feedback cycle must register once");
    assert_eq!(lsp.len(), 1, "LSP owner must register once");
    assert_eq!(advisory.len(), 1, "advisory production must register once");
    assert_eq!(
        hooks.len(),
        1,
        "hook host-delivery sink must be constructed"
    );
}

/// The daemon invocation protocol must not reacquire the legacy owner-injection
/// escape hatch.
///
/// An absence claim has no runtime observation to make, so it stays a lint.
#[test]
fn source_daemon_invocation_retains_no_owner_injection_escape_hatch() {
    assert!(
        !INVOCATION_SOURCE.contains("invoke_with_owners"),
        "legacy owner injection must not remain on the daemon invocation path"
    );
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
}

/// The application surface must keep declaring the PR12 operation family.
///
/// Declaration is all this checks. `production_primitive_reads_agree_across_mcp_http_and_cli`
/// proves that a declared operation actually routes to the daemon; the
/// remaining operations here are declared but reach the daemon only through
/// their own suites.
#[test]
fn source_application_surface_declares_the_pr12_operation_family() {
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
}

/// Hook V2 production delivery must stay on typed daemon ports.
///
/// This is a source lint: runtime delivery through those ports is covered by
/// the hook unit suite and PR13 advisory host-delivery acceptance.
#[test]
fn source_hook_v2_feedback_delivery_uses_typed_daemon_ports() {
    assert_contains_all(
        HOOK_V2_SOURCE,
        &[
            "deliver_hook_feedback",
            "DaemonAdmissionPort",
            "DaemonFeedbackNoticeDeliveryPort",
            "HookScopedFeedbackV1",
        ],
    );
    assert!(
        !HOOK_V2_SOURCE.contains("acknowledge_advisory_feedback_notice"),
        "hook v2 must not retain a direct feedback-notice daemon helper"
    );
    let production_v2 = HOOK_V2_SOURCE
        .split("#[cfg(test)]")
        .next()
        .expect("hook v2 production source precedes tests");
    assert!(
        !production_v2.contains("\"hook_v2_feedback_notice_delivery\""),
        "hook v2 production dispatch must not embed feedback-notice delivery action JSON"
    );
    assert_contains_all(
        HOOK_DAEMON_PORTS_SOURCE,
        &[
            "AsyncHookAdmissionPortV1",
            "AsyncHookFeedbackDeliveryPortV1",
            "hook_v2_admit",
            "hook_v2_feedback_notice_delivery",
        ],
    );
}
