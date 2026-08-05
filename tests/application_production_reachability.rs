//! Reachability contract for the daemon-owned application production path.
//!
//! These tests boot a real daemon, open a real project, and drive the
//! primitive through the shipped MCP, HTTP, and CLI surfaces. They fail when
//! the path stops executing; source text and catalog declarations are not
//! accepted as reachability evidence.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};

use serde_json::Value;
use tracedecay::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, ApplicationSurfaceRequest,
    CallableCodeSurfaceMeta, CodeSymbolSearchSurfaceRequest, PrimitiveCodeSurfaceRequest,
    parse_application_surface_request, resolve_http_application_surface,
};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface;
use tracedecay_application::retrieval::SymbolGraphScope;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, OpaqueCursor, OperationTermination, RequestId,
    ResultProjection, RetrievalOrder,
};

/// Every surface pins its page size to ten rows, so a query with more matches
/// than this must cross a page boundary to answer at all.
const SURFACE_PAGE_SIZE: usize = 10;
const PROBE_SYMBOL_COUNT: usize = 24;
const PROBE_TOKEN: &str = "application_pagination_probe";

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
    std::fs::write(destination.join("application_pagination_probe.rs"), source)
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

fn run_application_cli(
    fixture: &ProductionFixture,
    operation: ApplicationSurfaceOperation,
    arguments: &Value,
) -> Output {
    let project = fixture.project.to_string_lossy().into_owned();
    let arguments = arguments.to_string();
    common::tracedecay_command_with_home(fixture.home())
        .current_dir(&fixture.project)
        .args([
            "tool",
            "--project",
            project.as_str(),
            operation.as_str(),
            "--args",
            arguments.as_str(),
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("run {} through the CLI: {error}", operation.as_str()))
}

fn invocation_terminal(result: &ApplicationSurfaceInvocationResult) -> Value {
    match &result.result {
        Ok(envelope) => serde_json::to_value(envelope).expect("application evidence envelope"),
        Err(envelope) => serde_json::to_value(envelope).expect("application problem envelope"),
    }
}

fn terminal_disposition(value: &Value) -> (&str, &str) {
    if let Some(outcome) = value.get("outcome") {
        let outcome = outcome["outcome"]
            .as_str()
            .unwrap_or_else(|| panic!("typed application outcome: {value:#}"));
        if outcome == "evidence" {
            assert_eq!(
                value["outcome"]["value"]["execution"]["termination"], "completed",
                "evidence must be terminal rather than a route placeholder: {value:#}"
            );
        }
        return ("outcome", outcome);
    }
    let problem = value.get("problem").unwrap_or_else(|| {
        panic!("application call returned no outcome or typed problem: {value:#}")
    });
    let kind = problem["kind"]
        .as_str()
        .unwrap_or_else(|| panic!("typed application problem: {value:#}"));
    assert!(
        problem["diagnostic"]["code"].is_string(),
        "application problem must identify its observed terminal state: {value:#}"
    );
    ("problem", kind)
}

async fn invoke_mcp_symbol_search(
    client: &DaemonInvocationClient,
    request_id: &str,
) -> ApplicationSurfaceInvocationResult {
    resolve_mcp_application_surface(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        RequestId::new(request_id).expect("request id"),
        symbol_search_surface_request(PROBE_TOKEN, None),
        RequestedOutputFormat::Json,
        Some(client),
    )
    .await
    .expect("MCP application dispatch")
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

/// The project-open completion boundary may not be published before its
/// production owners are callable. Fresh concurrent connections and repeated
/// reads must therefore observe the same real owner, never a placeholder,
/// duplicate-registration failure, or partially mounted route.
#[tokio::test(flavor = "multi_thread")]
async fn immediate_concurrent_and_repeated_opens_publish_one_callable_owner() {
    let fixture = production_fixture().await;

    // This is the first application request after the packaged `init` returns.
    // It makes publication ordering externally observable at the only boundary
    // users can rely on: an independent owner must already be callable without
    // waiting for the generation-backed symbol owner used below.
    let immediate_tests = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::TestResults,
        RequestId::new("request.application-reachability.open.immediate-tests")
            .expect("request id"),
        parse_application_surface_request(
            ApplicationSurfaceOperation::TestResults,
            serde_json::json!({}),
        )
        .expect("test-results request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("immediate test-results dispatch");
    let immediate_tests = evidence_payload(&immediate_tests);
    assert!(
        immediate_tests["head_commit_id"].is_string(),
        "immediate independent owner must publish a commit identity: {immediate_tests:#}"
    );
    assert!(
        immediate_tests["code_generation_id"].is_string(),
        "immediate independent owner must publish a generation identity: {immediate_tests:#}"
    );
    assert!(
        immediate_tests["results"].is_array(),
        "immediate independent owner must return its typed result collection: {immediate_tests:#}"
    );

    let immediate =
        invoke_mcp_symbol_search(
            &fixture.client,
            "request.application-reachability.open.immediate",
        )
        .await;
    let immediate = evidence_payload(&immediate).clone();
    assert_first_page_of_many("immediate post-open MCP", &immediate);

    let fresh_client = || {
        let handshake =
            DaemonHandshake::for_current_client(Some(fixture.project.clone()), None, false, false)
                .expect("daemon handshake");
        DaemonInvocationClient::for_current(handshake).expect("daemon client")
    };
    let client_a = fresh_client();
    let client_b = fresh_client();
    let client_c = fresh_client();
    let client_d = fresh_client();
    let (a, b, c, d) = tokio::join!(
        invoke_mcp_symbol_search(&client_a, "request.application-reachability.open.concurrent-a"),
        invoke_mcp_symbol_search(&client_b, "request.application-reachability.open.concurrent-b"),
        invoke_mcp_symbol_search(&client_c, "request.application-reachability.open.concurrent-c"),
        invoke_mcp_symbol_search(&client_d, "request.application-reachability.open.concurrent-d"),
    );
    for (label, result) in [
        ("concurrent-a", a),
        ("concurrent-b", b),
        ("concurrent-c", c),
        ("concurrent-d", d),
    ] {
        let payload = evidence_payload(&result);
        assert_first_page_of_many(label, payload);
        assert_eq!(
            payload["items"], immediate["items"],
            "{label} must reach the same mounted owner"
        );
    }

    for suffix in ["repeat-a", "repeat-b"] {
        let result = invoke_mcp_symbol_search(
            &fixture.client,
            &format!("request.application-reachability.open.{suffix}"),
        )
        .await;
        assert_eq!(
            evidence_payload(&result)["items"],
            immediate["items"],
            "{suffix} must reuse the production owner without duplication"
        );
    }
}

/// Every catalog operation must execute through all three shipped adapters.
/// Missing fixture resources are
/// allowed to produce a typed terminal problem, but an absent binding, adapter
/// rejection, daemon disconnect, or placeholder response fails this test.
#[tokio::test(flavor = "multi_thread")]
async fn operation_family_executes_through_cli_mcp_and_http() {
    let fixture = production_fixture().await;
    let cases = [
        (
            ApplicationSurfaceOperation::FeedbackImpact,
            serde_json::json!({ "request_handle": "rh_missing-application-reachability" }),
        ),
        (
            ApplicationSurfaceOperation::AffectedTests,
            serde_json::json!({ "request_handle": "rh_missing-application-reachability" }),
        ),
        (
            ApplicationSurfaceOperation::TestResults,
            serde_json::json!({}),
        ),
        (
            ApplicationSurfaceOperation::SessionLookup,
            serde_json::json!({
                "session_id": "session.application-reachability.missing",
                "meta": {
                    "temporal": { "kind": "current" },
                    "page": { "page_size": 10, "cursor": null },
                    "projection": "summary",
                    "order": "stable_identity"
                }
            }),
        ),
        (
            ApplicationSurfaceOperation::DiagnosticsRead,
            serde_json::json!({
                "scope": "workspace",
                "maximum_diagnostics": 10
            }),
        ),
    ];

    for (operation, arguments) in cases {
        let request = parse_application_surface_request(operation, arguments.clone())
            .unwrap_or_else(|error| panic!("parse {} request: {error}", operation.as_str()));
        let mcp = resolve_mcp_application_surface(
            operation,
            RequestId::new(format!(
                "request.application-family.mcp.{}",
                operation.as_str()
            ))
                .expect("MCP request id"),
            request,
            RequestedOutputFormat::Json,
            Some(&fixture.client),
        )
        .await
        .unwrap_or_else(|error| panic!("dispatch MCP {}: {error}", operation.as_str()));
        let request = parse_application_surface_request(operation, arguments.clone())
            .unwrap_or_else(|error| panic!("parse {} request: {error}", operation.as_str()));
        let http = resolve_http_application_surface(
            operation,
            RequestId::new(format!(
                "request.application-family.http.{}",
                operation.as_str()
            ))
                .expect("HTTP request id"),
            request,
            RequestedOutputFormat::Json,
            Some(&fixture.client),
        )
        .await
        .unwrap_or_else(|error| panic!("dispatch HTTP {}: {error}", operation.as_str()));
        let cli = run_application_cli(&fixture, operation, &arguments);
        assert_eq!(
            cli.status.success(),
            mcp.result.is_ok(),
            "CLI {} exit must agree with the daemon's typed terminal state\nstdout:\n{}\nstderr:\n{}",
            operation.as_str(),
            String::from_utf8_lossy(&cli.stdout),
            String::from_utf8_lossy(&cli.stderr)
        );
        let cli: Value = serde_json::from_slice(&cli.stdout)
            .unwrap_or_else(|error| panic!("parse CLI {} JSON: {error}", operation.as_str()));

        assert_eq!(
            mcp.binding_id.as_str(),
            format!("binding.mcp.{}.v1", operation.as_str())
        );
        assert_eq!(
            http.binding_id.as_str(),
            format!("binding.http.{}.v1", operation.as_str())
        );
        let mcp = invocation_terminal(&mcp);
        let http = invocation_terminal(&http);
        assert_eq!(
            mcp["contract"],
            http["contract"],
            "{} MCP and HTTP contracts",
            operation.as_str()
        );
        assert_eq!(
            mcp["contract"],
            cli["contract"],
            "{} daemon and CLI contracts",
            operation.as_str()
        );
        assert_eq!(
            terminal_disposition(&mcp),
            terminal_disposition(&http),
            "{} MCP and HTTP terminal behavior",
            operation.as_str()
        );
        assert_eq!(
            terminal_disposition(&mcp),
            terminal_disposition(&cli),
            "{} daemon and CLI terminal behavior",
            operation.as_str()
        );
    }
}

/// Project open must mount the primitive runtime, and that runtime must
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
        RequestId::new("request.application-reachability.symbol-search.mcp").expect("request id"),
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
        RequestId::new("request.application-reachability.symbol-search.resume")
            .expect("request id"),
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
        RequestId::new("request.application-reachability.symbol-search.single")
            .expect("request id"),
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
        RequestId::new("request.application-reachability.parity.mcp").expect("request id"),
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
        RequestId::new("request.application-reachability.parity.http").expect("request id"),
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
