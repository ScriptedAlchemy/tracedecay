//! `tracedecay_diagnostics` is the public MCP spelling of `diagnostics_read`.
//!
//! These tests call that tool through a real MCP `tools/call` on the production
//! composition. A workspace with no diagnostic publication must not look like a
//! clean page: callers have to see the producer state and the route that
//! changes it. A TypeScript project with its own compiler gets that producer
//! automatically at admission.

use std::time::Duration;

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture, wait_for_current_graph,
};

const JSON_RPC_ID_ONE_DIGEST: &str = "6b86b273ff34fce19d6b804eff5a3f57";
const ABSENT_PRODUCER_CODE: &str = "application.diagnostics.unsupported";
const ABSENT_PRODUCER_MESSAGE: &str = "No diagnostic producer is configured for this scope: no tsconfig.json was found under the project root, so no compiler runs automatically. Run the project's own build or type check and publish its output with tracedecay_diagnose (`cargo_output`), then read again.";
/// A file read names every tsconfig location the owner search checked.
const ABSENT_FILE_PRODUCER_MESSAGE: &str = "No diagnostic producer is configured for this scope: no tsconfig owns `src/main.rs` (searched src/tsconfig.json, tsconfig.json, and their project references), so no compiler runs automatically. Run the project's own build or type check and publish its output with tracedecay_diagnose (`cargo_output`), then read again.";
/// The same message as the markdown renderer escapes it.
const ABSENT_PRODUCER_MESSAGE_MARKDOWN: &str = "No diagnostic producer is configured for this scope: no tsconfig.json was found under the project root, so no compiler runs automatically. Run the project's own build or type check and publish its output with tracedecay\\_diagnose (\\`cargo\\_output\\`), then read again.";

#[tokio::test]
async fn diagnostics_read_names_a_missing_producer_and_rejects_a_bad_scope() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    for (arguments, message) in [
        (
            json!({"scope": "workspace", "maximum_diagnostics": 1}),
            ABSENT_PRODUCER_MESSAGE,
        ),
        (
            json!({"scope": "file", "path": "src/main.rs", "maximum_diagnostics": 1}),
            ABSENT_FILE_PRODUCER_MESSAGE,
        ),
    ] {
        let result =
            handle_real_server_tool_call(&server, "tracedecay_diagnostics", arguments).await;
        assert_absent_producer(&result, message);
    }

    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "workspace", "maximum_diagnostics": 1, "format": "markdown"}),
    )
    .await;
    assert_absent_producer_markdown(&markdown);

    let missing_path = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "maximum_diagnostics": 1}),
    )
    .await;
    assert_eq!(
        missing_path["error"],
        rejected_diagnostics_request(
            "application surface request does not match its reviewed schema: `path` is required when `scope` is file",
        )
    );

    let package_scope = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "package", "path": "src"}),
    )
    .await;
    assert_eq!(
        package_scope["error"],
        rejected_diagnostics_request(
            "application surface request does not match its reviewed schema: `scope` package is not supported for diagnostics",
        )
    );

    fixture.harness.shutdown().await;
}

/// A fresh TypeScript project with its own `node_modules/.bin/tsc` needs no
/// pasted compiler output: the daemon runs that compiler after the first
/// complete generation and the read returns its `TS4023` on the file.
#[cfg(unix)]
#[tokio::test]
async fn typescript_project_publishes_diagnostics_from_its_own_compiler() {
    use crate::common::fixture::{
        TYPESCRIPT_FIXTURE_TSC_INVOCATIONS, TypeScriptFixtureCompiler,
        write_typescript_diagnostics_fixture,
    };
    use crate::support::production_composition_fixture_with_sources;

    let fixture = production_composition_fixture_with_sources(|project| {
        write_typescript_diagnostics_fixture(project, TypeScriptFixtureCompiler::Present);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    let file_read = await_published_diagnostics(
        &server,
        json!({"scope": "file", "path": "src/index.ts", "maximum_diagnostics": 10}),
    )
    .await;
    let records = published_records(&file_read);
    assert_eq!(records.len(), 1, "{file_read}");
    assert_eq!(records[0]["logical_path"], "src/index.ts", "{file_read}");
    let diagnostic = &records[0]["diagnostic"];
    assert_eq!(diagnostic["code"], "TS4023", "{file_read}");
    assert_eq!(diagnostic["severity"], "error", "{file_read}");
    assert!(
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("cannot be named")),
        "{file_read}"
    );

    let workspace_read = await_published_diagnostics(
        &server,
        json!({"scope": "workspace", "maximum_diagnostics": 10}),
    )
    .await;
    assert_eq!(
        published_records(&workspace_read).len(),
        1,
        "{workspace_read}"
    );

    // The compiler that ran is the project's own, from the project root, with
    // the check-only arguments; nothing else could have produced the record.
    let invocations = std::fs::read_to_string(
        fixture
            .project_root
            .join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS),
    )
    .expect("the fixture compiler records every invocation");
    let canonical_root = fixture
        .project_root
        .canonicalize()
        .expect("canonical project root");
    let root = canonical_root.display();
    for invocation in invocations.lines() {
        assert_eq!(
            invocation,
            format!("{root} -p {root}/tsconfig.json --noEmit --pretty false"),
            "{invocations}"
        );
    }
    assert!(
        !invocations.is_empty(),
        "the producer ran the project's tsc"
    );

    fixture.harness.shutdown().await;
}

/// The same project before `npm install`: the read must carry the exact setup
/// command and a legal action, never `Legal actions: none`.
#[cfg(unix)]
#[tokio::test]
async fn typescript_project_without_a_compiler_names_the_install_command() {
    use crate::common::fixture::{TypeScriptFixtureCompiler, write_typescript_diagnostics_fixture};
    use crate::support::production_composition_fixture_with_sources;

    let fixture = production_composition_fixture_with_sources(|project| {
        write_typescript_diagnostics_fixture(project, TypeScriptFixtureCompiler::Missing);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "path": "src/index.ts", "maximum_diagnostics": 10}),
    )
    .await;
    assert_eq!(result["isError"], json!(true), "{result}");
    let problem = &result["structuredContent"]["problem"];
    assert_eq!(problem["kind"], "unsupported", "{problem}");
    assert_eq!(
        problem["code"], "application.diagnostics.producer-missing",
        "{problem}"
    );
    assert_eq!(problem["legal_actions"], json!(["refresh"]), "{problem}");
    assert!(
        problem["message"]
            .as_str()
            .is_some_and(|message| message.contains("`npm install --save-dev typescript`")),
        "{problem}"
    );

    fixture.harness.shutdown().await;
}

/// Issue #2025: a pnpm monorepo with per-package tsconfigs and no root
/// `tsconfig.json`. The producer checks each package's own tsconfig with the
/// workspace root's compiler, and a package's finding reaches the read.
#[cfg(unix)]
#[tokio::test]
async fn typescript_monorepo_checks_each_package_tsconfig() {
    use crate::common::fixture::{
        TYPESCRIPT_FIXTURE_TSC_INVOCATIONS, TYPESCRIPT_MONOREPO_APP_FILE,
        TYPESCRIPT_MONOREPO_UNOWNED_FILE, TypeScriptFixtureCompiler,
        write_typescript_monorepo_diagnostics_fixture,
    };
    use crate::support::production_composition_fixture_with_sources;

    let fixture = production_composition_fixture_with_sources(|project| {
        write_typescript_monorepo_diagnostics_fixture(project, TypeScriptFixtureCompiler::Present);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    let app_read = await_published_diagnostics(
        &server,
        json!({"scope": "file", "path": TYPESCRIPT_MONOREPO_APP_FILE, "maximum_diagnostics": 10}),
    )
    .await;
    let records = published_records(&app_read);
    assert_eq!(records.len(), 1, "{app_read}");
    assert_eq!(
        records[0]["logical_path"], TYPESCRIPT_MONOREPO_APP_FILE,
        "{app_read}"
    );
    assert_eq!(records[0]["diagnostic"]["code"], "TS4023", "{app_read}");

    // The clean package was checked too, so its empty page is evidence.
    let lib_read = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "path": "packages/lib/src/lib.ts", "maximum_diagnostics": 10}),
    )
    .await;
    assert_ne!(lib_read["isError"], json!(true), "{lib_read}");
    assert!(published_records(&lib_read).is_empty(), "{lib_read}");

    // A TypeScript file no tsconfig owns was checked by nothing; another
    // package's publication must not read as its clean page.
    let unowned = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "path": TYPESCRIPT_MONOREPO_UNOWNED_FILE, "maximum_diagnostics": 10}),
    )
    .await;
    assert_eq!(
        unowned["structuredContent"]["problem"]["code"], "application.diagnostics.unsupported",
        "{unowned}"
    );

    let root = fixture
        .project_root
        .canonicalize()
        .expect("canonical project root");
    let root = root.display();
    let invocations = std::fs::read_to_string(
        fixture
            .project_root
            .join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS),
    )
    .expect("the fixture compiler records every invocation");
    let expected = ["app", "lib"].map(|package| {
        format!("{root} -p {root}/packages/{package}/tsconfig.json --noEmit --pretty false")
    });
    assert!(
        invocations
            .lines()
            .all(|line| expected.contains(&line.to_owned())),
        "every run checks one package tsconfig from the workspace root: {invocations}"
    );
    for invocation in &expected {
        assert!(
            invocations.lines().any(|line| line == invocation),
            "{invocations}"
        );
    }

    fixture.harness.shutdown().await;
}

/// The issue's exact state: the package tsconfig exists but dependencies are
/// not installed. The read names the owning tsconfig and the workspace's
/// install command instead of claiming there is no tsconfig.
#[cfg(unix)]
#[tokio::test]
async fn typescript_monorepo_without_install_names_the_workspace_install() {
    use crate::common::fixture::{
        TYPESCRIPT_MONOREPO_APP_FILE, TypeScriptFixtureCompiler,
        write_typescript_monorepo_diagnostics_fixture,
    };
    use crate::support::production_composition_fixture_with_sources;

    let fixture = production_composition_fixture_with_sources(|project| {
        write_typescript_monorepo_diagnostics_fixture(project, TypeScriptFixtureCompiler::Missing);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "path": TYPESCRIPT_MONOREPO_APP_FILE, "maximum_diagnostics": 10}),
    )
    .await;
    assert_eq!(result["isError"], json!(true), "{result}");
    let problem = &result["structuredContent"]["problem"];
    assert_eq!(
        problem["code"], "application.diagnostics.producer-missing",
        "{problem}"
    );
    assert_eq!(problem["legal_actions"], json!(["refresh"]), "{problem}");
    let message = problem["message"].as_str().expect("message");
    assert!(
        message.contains("`packages/app/tsconfig.json`") && message.contains("`pnpm install`"),
        "{problem}"
    );

    fixture.harness.shutdown().await;
}

/// A TypeScript file outside every package names each tsconfig location the
/// owner search checked, and routes to `tracedecay_diagnose`.
#[cfg(unix)]
#[tokio::test]
async fn typescript_file_no_tsconfig_owns_names_the_searched_paths() {
    use crate::common::fixture::{
        TYPESCRIPT_MONOREPO_UNOWNED_FILE, TypeScriptFixtureCompiler,
        write_typescript_monorepo_diagnostics_fixture,
    };
    use crate::support::production_composition_fixture_with_sources;

    let fixture = production_composition_fixture_with_sources(|project| {
        write_typescript_monorepo_diagnostics_fixture(project, TypeScriptFixtureCompiler::Missing);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "path": TYPESCRIPT_MONOREPO_UNOWNED_FILE, "maximum_diagnostics": 10}),
    )
    .await;
    assert_eq!(result["isError"], json!(true), "{result}");
    let problem = &result["structuredContent"]["problem"];
    assert_eq!(problem["code"], ABSENT_PRODUCER_CODE, "{problem}");
    assert_eq!(
        problem["legal_actions"],
        json!(["correct_request"]),
        "{problem}"
    );
    assert!(
        problem["message"].as_str().is_some_and(|message| message.contains(
            "no tsconfig owns `scripts/release.ts` (searched scripts/tsconfig.json, tsconfig.json, and their project references)"
        )),
        "{problem}"
    );

    fixture.harness.shutdown().await;
}

/// Polls the read until the producer has published for the current
/// generation; the pending state is the only one worth waiting through.
async fn await_published_diagnostics(
    server: &tracedecay::mcp::McpServer,
    arguments: Value,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..120 {
        let result =
            handle_real_server_tool_call(server, "tracedecay_diagnostics", arguments.clone()).await;
        if result["isError"] != json!(true) {
            return result;
        }
        let code = result["structuredContent"]["problem"]["code"].clone();
        assert!(
            code == "application.diagnostics.pending" || code == "application.diagnostics.stale",
            "the producer reported a terminal state instead of publishing: {result}"
        );
        last = result;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the TypeScript producer did not publish within the polling budget: {last}");
}

fn published_records(result: &Value) -> Vec<Value> {
    let payload: Value = serde_json::from_str(extract_real_server_text(result))
        .unwrap_or_else(|error| panic!("diagnostics evidence must be JSON: {error}\n{result}"));
    payload["outcome"]["value"]["payload"]["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("diagnostics evidence lists its records: {payload}"))
}

fn rejected_diagnostics_request(detail: &str) -> Value {
    json!({
        "code": -32603,
        "message": format!("tool execution failed: config error: {detail}"),
        "data": {
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool diagnostics ...` (`tracedecay tool diagnostics --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
            "tool": "tracedecay_diagnostics"
        }
    })
}

fn assert_absent_producer(result: &Value, message: &str) {
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["content"][0]["type"], "text");
    let text = extract_real_server_text(result);
    let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("diagnostics JSON should be the problem envelope: {error}\n{text}")
    });
    let request_id = assert_mcp_request_id(envelope["request_id"].as_str());
    assert_eq!(envelope, absent_producer_envelope(&request_id, message));
    assert_eq!(result["structuredContent"]["problem"], envelope["problem"]);
}

fn assert_absent_producer_markdown(result: &Value) {
    assert_eq!(result["isError"], json!(true));
    let request_id =
        assert_mcp_request_id(result["structuredContent"]["problem"]["request_id"].as_str());
    assert_eq!(
        result["structuredContent"]["problem"],
        absent_producer_problem(&request_id, ABSENT_PRODUCER_MESSAGE)
    );
    assert_eq!(
        extract_real_server_text(result),
        format!(
            "\
## diagnostics\\_read

- Operation: `diagnostics_read`
- Binding: `binding.mcp.diagnostics_read.v1`
- Status: `problem`
- Contract: `schema.application.primitive.diagnostics-read.result@1`
- Problem: `{ABSENT_PRODUCER_CODE}`
- Problem kind: `unsupported`
- Problem revision: `1`
- Owning layer: `application`
- Terminality: `pre_admission`
- Request: `{request_id}`
- Trace: `{request_id}`
- Message: {ABSENT_PRODUCER_MESSAGE_MARKDOWN}
- Retryable: `false`
- Retry: `never`
- Retry scope: `none`
- Retry after: `none`
- Cancellation stage: `none`
- Details: none
- Legal actions: `correct_request`
- Coverage: `not_available`"
        )
    );
}

fn assert_mcp_request_id(request_id: Option<&str>) -> String {
    let request_id = request_id.expect("diagnostics request id");
    assert!(
        request_id.starts_with("request.mcp.")
            && request_id.ends_with(&format!(".{JSON_RPC_ID_ONE_DIGEST}")),
        "diagnostics request id must bind JSON-RPC id 1, got {request_id}"
    );
    request_id.to_owned()
}

fn absent_producer_envelope(request_id: &str, message: &str) -> Value {
    json!({
        "contract": {
            "schema_id": "schema.application.primitive.diagnostics-read.result",
            "schema_revision": 1
        },
        "request_id": request_id,
        "problem": absent_producer_problem(request_id, message)
    })
}

fn absent_producer_problem(request_id: &str, message: &str) -> Value {
    json!({
        "revision": 1,
        "kind": "unsupported",
        "code": ABSENT_PRODUCER_CODE,
        "message": message,
        "diagnostic": {
            "code": ABSENT_PRODUCER_CODE,
            "message": message
        },
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "request_id": request_id,
        "trace_id": request_id,
        "details": [],
        "legal_actions": ["correct_request"],
        "coverage": null
    })
}
