//! Reachability contract for the PR12 daemon-owned production path.
//!
//! These tests boot a real daemon, open a real project, and drive a PR12
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

/// Every surface pins its page size to ten rows, so a query with more matches
/// than this must cross a page boundary to answer at all.
const SURFACE_PAGE_SIZE: usize = 10;
const PROBE_SYMBOL_COUNT: usize = 24;
const PROBE_TOKEN: &str = "pr12_pagination_probe";

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
