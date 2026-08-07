//! Reachability contract for the daemon-owned application production path.
//!
//! These tests boot a real daemon, open a real project, and drive the
//! primitive through the shipped MCP, HTTP, and CLI surfaces. They fail when
//! the path stops executing; source text and catalog declarations are not
//! accepted as reachability evidence.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

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
    ApplicationEnvelope, ApplicationOutcome, LegalAction, OpaqueCursor, OperationTermination,
    ProblemTerminality, RequestId, ResultProjection, RetrievalOrder,
};

/// Every surface pins its page size to ten rows, so a query with more matches
/// than this must cross a page boundary to answer at all.
const SURFACE_PAGE_SIZE: usize = 10;
const PROBE_SYMBOL_COUNT: usize = 24;
const PROBE_TOKEN: &str = "application_pagination_probe";
/// Called by every `*_caller_NN` probe, so its callers and references both
/// exceed one page.
const PROBE_SINK: &str = "application_pagination_probe_sink";
/// Calls every `*_leaf_NN` probe, so its callees exceed one page.
const PROBE_FANOUT: &str = "application_pagination_probe_fanout";
/// Implemented by every `ApplicationPaginationProbeImplNN`.
const PROBE_TRAIT: &str = "ApplicationPaginationProbeBehavior";
/// Extends every `ApplicationPaginationProbeBaseNN`.
const PROBE_HIERARCHY_LEAF: &str = "ApplicationPaginationProbeLeafTrait";
/// Takes one parameter of every `ApplicationPaginationProbeTypeNN`.
const PROBE_TYPE_ANCHOR: &str = "application_pagination_probe_type_anchor";
/// Size of `operation_carries_callable_code_meta` in `application_surface`: the
/// operations whose decoded request carries a `CallableCodeSurfaceMeta`, and so
/// can be handed a continuation cursor.
const CURSOR_CARRYING_CODE_OPERATIONS: usize = 14;

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
///
/// Each generated file feeds a different continuation-capable operation, and
/// every one of them is sized past [`SURFACE_PAGE_SIZE`] so the operation is
/// forced to mint a cursor instead of answering in one page:
///
/// * uniform free functions — symbol, signature, exact, and phrase reads;
/// * one call sink with many callers and one caller with many callees —
///   the relation and reference reads;
/// * one trait with many implementors — the implementations read;
/// * one trait with many supertrait bounds — the type-hierarchy read;
/// * one function with many distinctly typed parameters — the type-definition
///   read.
fn write_pagination_probe(project: &Path) {
    let destination = project.join("src");
    std::fs::create_dir_all(&destination).expect("probe destination");
    for (name, source) in [
        ("application_pagination_probe.rs", probe_symbol_source()),
        (
            "application_pagination_relations.rs",
            probe_relation_source(),
        ),
        ("application_pagination_traits.rs", probe_trait_source()),
        (
            "application_pagination_hierarchy.rs",
            probe_hierarchy_source(),
        ),
        ("application_pagination_types.rs", probe_type_source()),
    ] {
        std::fs::write(destination.join(name), source)
            .unwrap_or_else(|error| panic!("write the paginated probe {name}: {error}"));
    }
}

fn probe_symbol_source() -> String {
    let mut source = String::new();
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub fn {PROBE_TOKEN}_{index:02}(input: u32) -> u32 {{\n    input + {index}\n}}\n\n"
        ));
    }
    source
}

/// One heavily called function and one heavily calling function, so `Calls`
/// edges are dense in both directions from a single named anchor.
fn probe_relation_source() -> String {
    let mut source = format!("pub fn {PROBE_SINK}(input: u32) -> u32 {{\n    input\n}}\n\n");
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub fn {PROBE_TOKEN}_caller_{index:02}(input: u32) -> u32 {{\n    {PROBE_SINK}(input) + {index}\n}}\n\n"
        ));
    }
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub fn {PROBE_TOKEN}_leaf_{index:02}(input: u32) -> u32 {{\n    input + {index}\n}}\n\n"
        ));
    }
    source.push_str(&format!("pub fn {PROBE_FANOUT}(input: u32) -> u32 {{\n"));
    for index in 0..PROBE_SYMBOL_COUNT {
        let joiner = if index == 0 { "    " } else { "        + " };
        source.push_str(&format!("{joiner}{PROBE_TOKEN}_leaf_{index:02}(input)\n"));
    }
    source.push_str("}\n");
    source
}

/// One trait carrying more implementors than a page holds.
fn probe_trait_source() -> String {
    let mut source =
        format!("pub trait {PROBE_TRAIT} {{\n    fn {PROBE_TOKEN}_behavior(&self) -> u32;\n}}\n\n");
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub struct ApplicationPaginationProbeImpl{index:02};\n\n\
             impl {PROBE_TRAIT} for ApplicationPaginationProbeImpl{index:02} {{\n    \
             fn {PROBE_TOKEN}_behavior(&self) -> u32 {{\n        {index}\n    }}\n}}\n\n"
        ));
    }
    source
}

/// One trait whose supertrait bounds exceed a page, which is the only shape in
/// Rust that gives a single node more outgoing hierarchy edges than a page.
fn probe_hierarchy_source() -> String {
    let mut source = String::new();
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub trait ApplicationPaginationProbeBase{index:02} {{}}\n\n"
        ));
    }
    source.push_str(&format!("pub trait {PROBE_HIERARCHY_LEAF}:\n"));
    for index in 0..PROBE_SYMBOL_COUNT {
        let joiner = if index == 0 { "    " } else { "    + " };
        source.push_str(&format!(
            "{joiner}ApplicationPaginationProbeBase{index:02}\n"
        ));
    }
    source.push_str("{\n}\n");
    source
}

/// One function whose parameters name more distinct types than a page holds, so
/// its outgoing `TypeOf` edges cross the page boundary.
fn probe_type_source() -> String {
    let mut source = String::new();
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "pub struct ApplicationPaginationProbeType{index:02};\n\n"
        ));
    }
    source.push_str(&format!("pub fn {PROBE_TYPE_ANCHOR}(\n"));
    for index in 0..PROBE_SYMBOL_COUNT {
        source.push_str(&format!(
            "    argument_{index:02}: ApplicationPaginationProbeType{index:02},\n"
        ));
    }
    source.push_str(") -> u32 {\n    0\n}\n");
    source
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
    // Every problem variant serializes a top-level `code` — the diagnostic
    // code when a diagnostic exists, else the variant's canonical code
    // (`ApplicationProblemRecord::new` in `result/envelope.rs`). Variants like
    // `NotFoundOrNotAuthorized` structurally carry no diagnostic
    // (anti-enumeration, `result/problem.rs`), so the diagnostic code is only
    // required when a diagnostic is present.
    assert!(
        problem["code"].is_string(),
        "application problem must identify its observed terminal state: {value:#}"
    );
    if !problem["diagnostic"].is_null() {
        assert!(
            problem["diagnostic"]["code"].is_string(),
            "a problem that carries a diagnostic must code it: {value:#}"
        );
    }
    ("problem", kind)
}

/// Total budget for honouring production's pre-admission retry contract on
/// one operation before the test fails with the last published problem.
const READINESS_RETRY_DEADLINE: Duration = Duration::from_secs(60);
/// The canonical `after_delay` floor: an advertised `retry_after_millis`
/// below this is never spun faster than the contract default.
const READINESS_RETRY_FLOOR_MILLIS: u64 = 250;

/// Re-invokes an MCP application read while production publishes its typed
/// pre-admission readiness problem.
///
/// Production's cold-open design completes project open before the first
/// code-index generation seals (`project_open_owners.rs` defers owner
/// registration as `SkippedUnindexed`, and the symbol-graph identity gate
/// abstains while the index is busy or stale). The first generation-backed
/// read may therefore legally return a `PreAdmission` problem with
/// `retryable: true` and the `Retry` legal action. That problem is a published
/// instruction, and this helper spends it exactly as published: re-invoke
/// after the greater of the advertised `retry_after_millis` and the 250ms
/// contract floor, bounded by [`READINESS_RETRY_DEADLINE`] in total.
///
/// No assertion is loosened here. Any problem that does not offer `Retry`
/// (wrong terminality, not retryable, or without the `Retry` legal action) is
/// returned untouched for the caller's existing assertions, and the eventual
/// admitted result is asserted exactly as before. On deadline the panic
/// carries the LAST problem record verbatim, so a never-resolving admission
/// (for example a defective initial identity capture) stays diagnostic
/// instead of collapsing into an opaque timeout.
async fn admitted_mcp_invocation(
    client: &DaemonInvocationClient,
    operation: ApplicationSurfaceOperation,
    request_id: &str,
    build_request: impl Fn() -> ApplicationSurfaceRequest,
) -> ApplicationSurfaceInvocationResult {
    let started = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let result = resolve_mcp_application_surface(
            operation,
            RequestId::new(format!("{request_id}.attempt-{attempt:03}")).expect("request id"),
            build_request(),
            RequestedOutputFormat::Json,
            Some(client),
        )
        .await
        .unwrap_or_else(|error| panic!("dispatch MCP {}: {error}", operation.as_str()));
        let Err(problem) = &result.result else {
            return result;
        };
        let record = problem.problem.as_ref();
        let retry_offered = record.retryable
            && record.terminality == ProblemTerminality::PreAdmission
            && record.legal_actions.contains(&LegalAction::Retry);
        if !retry_offered {
            return result;
        }
        let delay = Duration::from_millis(
            record
                .retry_after_millis
                .unwrap_or(READINESS_RETRY_FLOOR_MILLIS)
                .max(READINESS_RETRY_FLOOR_MILLIS),
        );
        if started.elapsed() + delay > READINESS_RETRY_DEADLINE {
            panic!(
                "{} ({request_id}) retried the published Retry legal action for {:?} across \
                 {attempt} attempts and was still refused admission; last problem code \
                 {:?}: {record:#?}",
                operation.as_str(),
                started.elapsed(),
                record.code,
            );
        }
        tokio::time::sleep(delay).await;
    }
}

async fn invoke_mcp_symbol_search(
    client: &DaemonInvocationClient,
    request_id: &str,
) -> ApplicationSurfaceInvocationResult {
    admitted_mcp_invocation(
        client,
        ApplicationSurfaceOperation::CodeSymbolSearch,
        request_id,
        || symbol_search_surface_request(PROBE_TOKEN, None),
    )
    .await
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
    assert!(
        !payload["next_cursor"].is_null(),
        "{surface} must issue a continuation cursor for the remaining {} rows: {payload:#}",
        total - items.len() as u64
    );
    assert_truncation_agrees_with_cursor(surface, payload);
}

/// The symbol-graph page carries an explicit `truncated` flag while the
/// callable-code page derives truncation from the cursor alone. Wherever the
/// flag exists it must agree with the cursor, so neither shape can advertise a
/// continuation it did not mint.
fn assert_truncation_agrees_with_cursor(surface: &str, payload: &Value) {
    let Some(truncated) = payload.get("truncated") else {
        return;
    };
    assert_eq!(
        truncated,
        &Value::Bool(!payload["next_cursor"].is_null()),
        "{surface} truncation flag must follow its continuation cursor: {payload:#}"
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
    // `test_results` is NOT generation-independent: production sources its
    // `head_commit_id`/`code_generation_id` from the code-index generation
    // (`managed_test_scope.rs`), so this read is generation-backed like the
    // symbol reads below and honours the same published pre-admission retry
    // contract.
    let immediate_tests = admitted_mcp_invocation(
        &fixture.client,
        ApplicationSurfaceOperation::TestResults,
        "request.application-reachability.open.immediate-tests",
        || {
            parse_application_surface_request(
                ApplicationSurfaceOperation::TestResults,
                serde_json::json!({}),
            )
            .expect("test-results request")
        },
    )
    .await;
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

    let immediate = invoke_mcp_symbol_search(
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
        invoke_mcp_symbol_search(
            &client_a,
            "request.application-reachability.open.concurrent-a"
        ),
        invoke_mcp_symbol_search(
            &client_b,
            "request.application-reachability.open.concurrent-b"
        ),
        invoke_mcp_symbol_search(
            &client_c,
            "request.application-reachability.open.concurrent-c"
        ),
        invoke_mcp_symbol_search(
            &client_d,
            "request.application-reachability.open.concurrent-d"
        ),
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
        // The MCP read runs first and honours the pre-admission retry
        // contract; readiness is monotonic for this static fixture, so the
        // HTTP and CLI reads below observe the same admitted owner. Terminal
        // problems that do not offer `Retry` pass through unchanged.
        let mcp = admitted_mcp_invocation(
            &fixture.client,
            operation,
            &format!("request.application-family.mcp.{}", operation.as_str()),
            || {
                parse_application_surface_request(operation, arguments.clone())
                    .unwrap_or_else(|error| panic!("parse {} request: {error}", operation.as_str()))
            },
        )
        .await;
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
/// Spending that continuation through the same shipped surface reaches the
/// cursor authority's resume path, which no surface could call while the page
/// was hard-coded to the first one. Both reads run against one live graph
/// generation, so the resumed page is the same generation's continuation rather
/// than a second generation's first page.
#[tokio::test(flavor = "multi_thread")]
async fn production_project_open_serves_a_paginated_symbol_graph_read() {
    let fixture = production_fixture().await;
    let result = invoke_mcp_symbol_search(
        &fixture.client,
        "request.application-reachability.symbol-search.mcp",
    )
    .await;
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

    let mcp = invoke_mcp_symbol_search(
        &fixture.client,
        "request.application-reachability.parity.mcp",
    )
    .await;
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

/// What the production runtime can actually page for one operation.
///
/// This is a claim about the runtime, not about the test: an operation is
/// either forced across a page boundary by the fixture, or bounded to a single
/// row by the code named in `authority`. There is no third state, so an
/// operation can never be quietly skipped.
#[derive(Clone, Copy, Debug)]
enum ContinuationExpectation {
    /// The fixture must drive this operation past [`SURFACE_PAGE_SIZE`]. A
    /// short page fails and says to grow the fixture.
    CrossesPages,
    /// The runtime materializes at most one row here whatever the fixture
    /// holds, so there is no continuation to mint. A page that does mint one
    /// fails, because the operation has become continuation-capable and owes
    /// the round trip above.
    BoundedToOneRow { authority: &'static str },
}

/// One cursor-carrying operation, the arguments that reach its paginating
/// result set, and the page behaviour its runtime can produce.
struct ContinuationCase {
    operation: ApplicationSurfaceOperation,
    arguments: fn(&ProbeAnchors, Option<&str>) -> Value,
    expectation: ContinuationExpectation,
}

/// Node ids minted by the live index for the fixture's paging anchors.
///
/// They are read back out of the running daemon rather than constructed, so a
/// change to node identity fails this test instead of silently querying a node
/// that no longer exists.
struct ProbeAnchors {
    sink: String,
    fanout: String,
    hierarchy_leaf: String,
    type_anchor: String,
}

impl ProbeAnchors {
    async fn resolve(fixture: &ProductionFixture) -> Self {
        Self {
            sink: resolve_probe_node_id(fixture, PROBE_SINK).await,
            fanout: resolve_probe_node_id(fixture, PROBE_FANOUT).await,
            hierarchy_leaf: resolve_probe_node_id(fixture, PROBE_HIERARCHY_LEAF).await,
            type_anchor: resolve_probe_node_id(fixture, PROBE_TYPE_ANCHOR).await,
        }
    }
}

async fn resolve_probe_node_id(fixture: &ProductionFixture, name: &str) -> String {
    let result = admitted_mcp_invocation(
        &fixture.client,
        ApplicationSurfaceOperation::CodeSymbolSearch,
        &format!("request.application-continuation.anchor.{name}"),
        || symbol_search_surface_request(name, None),
    )
    .await;
    let payload = evidence_payload(&result);
    payload["items"]
        .as_array()
        .unwrap_or_else(|| panic!("anchor page items for {name}: {payload:#}"))
        .iter()
        .find(|item| item["name"] == Value::from(name))
        .and_then(|item| item["node_id"].as_str())
        .unwrap_or_else(|| {
            panic!("fixture must index a symbol named {name}, saw: {payload:#}");
        })
        .to_owned()
}

fn callable_code_meta(cursor: Option<&str>) -> Value {
    serde_json::json!({
        "projection": "summary",
        "order": "relevance",
        "cursor": cursor,
    })
}

/// The reserved unpinned generation every shipped caller passes when it wants
/// the freshness-resolved latest complete generation.
fn code_query_scope() -> Value {
    serde_json::json!({
        "generation": tracedecay_application::UNPINNED_LATEST_GENERATION_SENTINEL,
        "path_prefix": Value::Null,
    })
}

fn symbol_graph_scope() -> Value {
    serde_json::json!({ "path_prefix": Value::Null })
}

/// Every operation in the surface's `operation_carries_callable_code_meta`
/// list, paired with a request that reaches a result set big enough to page.
fn continuation_cases() -> Vec<ContinuationCase> {
    vec![
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeExactOccurrence,
            arguments: |_, cursor| {
                serde_json::json!({
                    "literal": PROBE_TOKEN,
                    "kind": Value::Null,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodePhraseSearch,
            arguments: |_, cursor| {
                serde_json::json!({
                    "query": PROBE_TOKEN,
                    "phrases": [PROBE_TOKEN],
                    "field_filters": [],
                    "fuzzy_budget": 0,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeSymbolSearch,
            arguments: |_, cursor| {
                serde_json::json!({
                    "query": PROBE_TOKEN,
                    "scope": symbol_graph_scope(),
                    "lazy_index_ignored_dependencies": false,
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeSignatureSearch,
            arguments: |_, cursor| {
                serde_json::json!({
                    "returns": "u32",
                    "params": ["u32"],
                    "is_async": Value::Null,
                    "scope": symbol_graph_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeImplementations,
            arguments: |_, cursor| {
                serde_json::json!({
                    "selector": { "selector": "trait", "name": PROBE_TRAIT },
                    "scope": symbol_graph_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeTypeHierarchy,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.hierarchy_leaf,
                    "maximum_depth": 1,
                    "scope": symbol_graph_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeCallers,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.sink,
                    "maximum_depth": 1,
                    "resolve_trait_dispatch": false,
                    "scope": symbol_graph_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeCallees,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.fanout,
                    "maximum_depth": 1,
                    "resolve_trait_dispatch": false,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeFacets,
            arguments: |_, cursor| {
                serde_json::json!({
                    "dimension": "path",
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeReferences,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.sink,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeTypeDefinition,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.type_anchor,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::CrossesPages,
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeDeclaration,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.sink,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::BoundedToOneRow {
                authority: "navigation_symbol_query pushes the one resolved start symbol",
            },
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeDefinition,
            arguments: |anchors, cursor| {
                serde_json::json!({
                    "node_id": anchors.sink,
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::BoundedToOneRow {
                authority: "navigation_symbol_query pushes the one resolved start symbol",
            },
        },
        ContinuationCase {
            operation: ApplicationSurfaceOperation::CodeTimeline,
            arguments: |_, cursor| {
                serde_json::json!({
                    "scope": code_query_scope(),
                    "meta": callable_code_meta(cursor),
                })
            },
            expectation: ContinuationExpectation::BoundedToOneRow {
                authority: "the timeline query describes exactly one served generation",
            },
        },
    ]
}

async fn continuation_page(
    fixture: &ProductionFixture,
    operation: ApplicationSurfaceOperation,
    label: &str,
    arguments: Value,
) -> Value {
    let build_request = || {
        parse_application_surface_request(operation, arguments.clone()).unwrap_or_else(|error| {
            panic!(
                "parse {} {label} request: {error}\n{arguments:#}",
                operation.as_str()
            )
        })
    };
    // The surface decodes exactly its cursor-carrying operations into these two
    // request families, so a case that stopped carrying a cursor would land in
    // another family and fail here rather than pass with nothing to continue.
    assert!(
        matches!(
            build_request(),
            ApplicationSurfaceRequest::CallableCode(_)
                | ApplicationSurfaceRequest::PrimitiveCode(_)
        ),
        "{} must decode into a request that carries a surface cursor",
        operation.as_str()
    );
    let result = admitted_mcp_invocation(
        &fixture.client,
        operation,
        &format!(
            "request.application-continuation.{}.{label}",
            operation.as_str()
        ),
        build_request,
    )
    .await;
    evidence_payload(&result).clone()
}

/// The continuation contract holds for every operation whose surface request
/// carries a cursor, not only for symbol search.
///
/// A cursor is only real if it is minted from a result set the page cannot
/// hold, spent back through the shipped surface, and answered with rows the
/// first page did not contain. Each operation here is driven through that whole
/// round trip against one live generation, and the same continuation is spent
/// twice so a resume that consumed server state would fail rather than pass.
///
/// The operations that cannot page are not skipped: their runtime bounds them
/// to a single row, so they are asserted to complete with no continuation at
/// all. If one of them ever starts issuing a cursor, this test fails and the
/// operation must be moved onto the round trip above.
#[tokio::test(flavor = "multi_thread")]
async fn every_cursor_carrying_code_operation_mints_and_spends_a_continuation() {
    let fixture = production_fixture().await;
    let anchors = ProbeAnchors::resolve(&fixture).await;
    let cases = continuation_cases();

    // The surface lists fourteen operations in
    // `operation_carries_callable_code_meta`, and each is covered once. The
    // predicate is private, so the size is restated here; every case still
    // proves its own membership when its request decodes, in
    // `continuation_page`.
    let declared = cases
        .iter()
        .map(|case| case.operation.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        declared.len(),
        CURSOR_CARRYING_CODE_OPERATIONS,
        "every cursor-carrying code operation is covered exactly once"
    );
    assert_eq!(declared.len(), cases.len(), "no operation is covered twice");

    for case in cases {
        let surface = case.operation.as_str();
        let first = continuation_page(
            &fixture,
            case.operation,
            "first",
            (case.arguments)(&anchors, None),
        )
        .await;
        match case.expectation {
            ContinuationExpectation::BoundedToOneRow { authority } => {
                let items = first["items"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{surface} page items: {first:#}"));
                assert_eq!(
                    items.len(),
                    1,
                    "{surface} is bounded to one row because {authority}: {first:#}"
                );
                assert!(
                    first["next_cursor"].is_null(),
                    "{surface} now issues a continuation and must be covered by the \
                     round trip instead of this single-row assertion: {first:#}"
                );
                assert_truncation_agrees_with_cursor(surface, &first);
                continue;
            }
            ContinuationExpectation::CrossesPages => {}
        }

        let items = first["items"]
            .as_array()
            .unwrap_or_else(|| panic!("{surface} page items: {first:#}"));
        let total = first["total"]
            .as_u64()
            .unwrap_or_else(|| panic!("{surface} page total: {first:#}"));
        assert!(
            total > SURFACE_PAGE_SIZE as u64 && items.len() == SURFACE_PAGE_SIZE,
            "{surface} must be driven past one page of {SURFACE_PAGE_SIZE} rows; the \
             fixture produced {total} matching rows, so grow the probe sources in \
             `write_pagination_probe` rather than accepting single-page coverage: {first:#}"
        );
        assert_first_page_of_many(surface, &first);

        let cursor = first["next_cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("{surface} continuation cursor: {first:#}"))
            .to_owned();
        let second = continuation_page(
            &fixture,
            case.operation,
            "resume",
            (case.arguments)(&anchors, Some(cursor.as_str())),
        )
        .await;
        assert_second_page_continues(surface, &first, &second);
        assert_truncation_agrees_with_cursor(surface, &second);

        // A continuation is a value the caller holds, so spending it again must
        // return the same page rather than advancing or exhausting a server-side
        // reader.
        let replayed = continuation_page(
            &fixture,
            case.operation,
            "replay",
            (case.arguments)(&anchors, Some(cursor.as_str())),
        )
        .await;
        for field in ["items", "total", "next_cursor"] {
            assert_eq!(
                replayed[field], second[field],
                "{surface} must replay one continuation idempotently ({field}): {replayed:#}"
            );
        }
    }
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
