//! Behavior of `tracedecay_multi_root_execute` through the MCP tool dispatcher.
//!
//! The call is the production handler path: argument decoding, the in-process
//! daemon executor, and a saved scope set over two enrolled repositories.
//! Expected values are the symbols and typed refusals a caller observes.

#![cfg(unix)]

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::{
    ApplicationOutcome, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasStatusV1,
    RegisteredRootSelectorV1, RequestId,
};
use tracedecay_domain::{ProjectId, ScopeSetId, UtcMicros};

use super::{
    enter_test_daemon_database_scope, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::{
    DaemonHandshake, InProcessDaemonInvocationExecutor, execute_daemon_invocation,
};
use crate::mcp::tools::{ToolCallRegistryOptions, handle_tool_call_with_registry_options};
use crate::project::TraceDecay;
use tracedecay_daemon_protocol::DaemonInvocationExecutor;
use tracedecay_daemon_service::{DaemonInvocationOutcome, DaemonInvocationRequest};

const SCOPE_SET_ID: &str = "scope-set.mcp-execute-proof";
const ALPHA_NAME: &str = "alpha_marker";
const BETA_NAME: &str = "beta_marker";

fn repository(source: &str) -> TempDir {
    let repository = TempDir::new().expect("repository");
    super::git(repository.path(), &["init", "--quiet"]);
    super::git(
        repository.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    super::git(
        repository.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    std::fs::write(repository.path().join("lib.rs"), source).expect("source");
    super::git(repository.path(), &["add", "."]);
    super::git(repository.path(), &["commit", "--quiet", "-m", "base"]);
    repository
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn controls(
    suffix: &str,
    observed_at: UtcMicros,
) -> (
    tracedecay_contracts::Deadline,
    tracedecay_contracts::CancellationContext,
) {
    (
        tracedecay_contracts::Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
            .expect("deadline"),
        tracedecay_contracts::CancellationContext::active(format!(
            "cancel.multi-root-execute.{suffix}"
        ))
        .expect("cancellation"),
    )
}

fn signature_query() -> Value {
    json!({
        "kind": "query",
        "request": {
            "operation": "code_signature_search",
            "request": {
                "returns": "u8",
                "params": [],
                "is_async": null,
                "scope": {"path_prefix": null},
                "meta": {"projection": "summary", "order": "source_position", "cursor": null}
            }
        }
    })
}

fn execute_arguments(scope_set_id: &str, revision: u64, digest: &str, page: u64) -> Value {
    json!({
        "scope_set_id": scope_set_id,
        "scope_set_revision": revision,
        "scope_set_digest": digest,
        "operation": signature_query(),
        "page": page,
        "continuation": null
    })
}

fn envelope(result: &tracedecay_mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text: {}", result.value));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool result is not JSON: {error}\n{text}"))
}

fn assert_problem(
    result: &tracedecay_mcp::ToolResult,
    request_id: &str,
    kind: &str,
    code: &str,
    message: &str,
    retry: &str,
    legal_actions: &[&str],
) {
    assert_eq!(result.semantic_error(), Some(true));
    let body = envelope(result);
    assert_eq!(body["binding_id"], "binding.http.multi_root.execute.v1");
    let application = &body["application"];
    assert_eq!(
        application["contract"]["schema_id"],
        "schema.tracedecay.multi-root.execute-result.v1"
    );
    assert_eq!(application["contract"]["schema_revision"], 1);
    assert_eq!(application["request_id"], request_id);
    let problem = &application["problem"];
    assert_eq!(problem["kind"], kind);
    assert_eq!(problem["code"], code);
    assert_eq!(problem["message"], message);
    assert_eq!(problem["retry"], retry);
    assert_eq!(problem["owning_layer"], "runtime");
    assert_eq!(problem["request_id"], request_id);
    assert_eq!(
        problem["legal_actions"]
            .as_array()
            .expect("legal actions")
            .iter()
            .map(|action| action.as_str().expect("legal action"))
            .collect::<Vec<_>>(),
        legal_actions
    );
}

fn symbol_names(page: &Value) -> Vec<String> {
    let roots = page["roots"]
        .as_array()
        .unwrap_or_else(|| panic!("execute page has no roots: {page}"));
    assert_eq!(roots.len(), 2, "the saved scope set has two roots: {page}");
    let mut names = Vec::new();
    for root in roots {
        assert_eq!(
            root["outcome"]["outcome"], "exact",
            "each enrolled root must return an exact page: {root}"
        );
        let pages = root["outcome"]["value"]
            .as_array()
            .unwrap_or_else(|| panic!("exact root has no page values: {root}"));
        assert_eq!(pages.len(), 1, "one child page per root: {root}");
        let items = pages[0]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("signature page has no items: {pages:?}"));
        assert_eq!(items.len(), 1, "one u8 function per root: {items:?}");
        names.push(
            items[0]["name"]
                .as_str()
                .unwrap_or_else(|| panic!("signature item has no name: {}", items[0]))
                .to_owned(),
        );
    }
    names.sort();
    names
}

async fn call_execute(
    graph: &TraceDecay,
    executor: Option<&InProcessDaemonInvocationExecutor>,
    request_id: &str,
    arguments: Value,
) -> tracedecay_mcp::ToolResult {
    let request_id = RequestId::new(request_id).expect("request id");
    let options = ToolCallRegistryOptions {
        application_invocation_executor: executor
            .map(|executor| executor as &dyn DaemonInvocationExecutor),
        application_request_id: Some(request_id),
        ..ToolCallRegistryOptions::default()
    };
    handle_tool_call_with_registry_options(
        graph,
        "tracedecay_multi_root_execute",
        arguments,
        None,
        None,
        options,
    )
    .await
    .expect("multi-root execute dispatch")
}

#[test]
fn multi_root_execute_returns_both_root_symbols_and_typed_refusals() {
    const STACK_SIZE: usize = 16 * 1024 * 1024;
    std::thread::Builder::new()
        .name("multi-root-execute-mcp".to_owned())
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .expect("multi-root execute runtime")
                .block_on(run_multi_root_execute());
        })
        .expect("multi-root execute thread")
        .join()
        .expect("multi-root execute thread must not panic");
}

async fn run_multi_root_execute() {
    let home = TempDir::new().expect("home");
    let profile_root = home.path().join("profile");
    let alpha =
        repository("pub fn alpha_marker() -> u8 { 11 }\npub fn ignored_marker() -> u16 { 7 }\n");
    let beta = repository("pub fn beta_marker() -> u8 { 22 }\n");
    let handshake = DaemonHandshake {
        project_path: Some(alpha.path().to_path_buf()),
        allow_init: true,
        client_identity: test_client_identity_for(profile_root.clone()),
        ..test_handshake_defaults()
    };
    let beta_handshake = DaemonHandshake {
        project_path: Some(beta.path().to_path_buf()),
        ..handshake.clone()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "multi-root-execute");
    let (alpha_key, _, _, _) = engine
        .open_project_server(&handshake)
        .await
        .expect("register alpha project");
    let (beta_key, _, _, _) = engine
        .open_project_server(&beta_handshake)
        .await
        .expect("register beta project");
    tokio::time::timeout(std::time::Duration::from_mins(1), async {
        loop {
            if engine
                .invocation
                .code_index_schedulers
                .latest_complete_ready(alpha.path())
                .await
                .is_some()
                && engine
                    .invocation
                    .code_index_schedulers
                    .latest_complete_ready(beta.path())
                    .await
                    .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both roots must publish a query generation");

    let alpha_project = ProjectId::new(
        alpha_key
            .owner
            .project_id
            .clone()
            .expect("alpha project id"),
    )
    .expect("alpha project");
    let beta_project = ProjectId::new(beta_key.owner.project_id.clone().expect("beta project id"))
        .expect("beta project");
    let scope_set_id = ScopeSetId::new(SCOPE_SET_ID).expect("scope set id");
    let observed_at = now();
    let (deadline, cancellation) = controls("cas", observed_at);
    let cas = execute_daemon_invocation(
        &engine,
        &handshake,
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root-execute.cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![
                    RegisteredRootSelectorV1::new(
                        beta_project,
                        beta.path().canonicalize().expect("canonical beta root"),
                    )
                    .expect("beta selector"),
                    RegisteredRootSelectorV1::new(
                        alpha_project,
                        alpha.path().canonicalize().expect("canonical alpha root"),
                    )
                    .expect("alpha selector"),
                ],
            )
            .expect("CAS request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { outcome, .. } = &cas.outcome
    else {
        panic!(
            "scope-set save must be applied before execute: {:?}",
            cas.outcome
        );
    };
    let ApplicationOutcome::Evidence(packet) = outcome else {
        panic!("scope-set save must return evidence");
    };
    let saved_result = packet.payload.clone().expect("saved scope set");
    assert!(matches!(
        saved_result.status,
        MultiRootScopeSetCasStatusV1::Applied
    ));
    let saved = saved_result.scope_set.expect("applied scope set");
    let digest = serde_json::to_value(saved.digest().clone()).expect("saved digest");
    let digest = digest.as_str().expect("digest string").to_owned();
    assert_eq!(saved.revision().get(), 1);

    let server = engine
        .project_server(&handshake)
        .await
        .expect("alpha MCP server");
    let graph = server.cg().await;
    let target = graph.configuration_runtime().configuration_target().clone();
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        graph.project_root(),
        &target.project_id,
    )
    .expect("active project scope");
    let executor = InProcessDaemonInvocationExecutor::new(
        engine.invocation.clone(),
        engine.store_administration.clone(),
        graph.project_root().to_path_buf(),
        scope,
    );

    let unavailable = call_execute(
        &graph,
        None,
        "request.mcp.multi-root-execute.unavailable",
        execute_arguments(SCOPE_SET_ID, 1, &digest, 0),
    )
    .await;
    assert_problem(
        &unavailable,
        "request.mcp.multi-root-execute.unavailable",
        "unavailable",
        "multi_root.daemon_unavailable",
        "The multi-root daemon invocation owner is unavailable",
        "after_delay",
        &["retry"],
    );
    assert_eq!(
        envelope(&unavailable)["application"]["problem"]["unavailable_classification"],
        "authority"
    );
    assert_eq!(
        envelope(&unavailable)["application"]["problem"]["diagnostic"]["code"],
        "multi_root.daemon_unavailable"
    );
    assert_eq!(
        envelope(&unavailable)["application"]["problem"]["diagnostic"]["message"],
        "The multi-root daemon invocation owner is unavailable"
    );

    let malformed = call_execute(
        &graph,
        Some(&executor),
        "request.mcp.multi-root-execute.malformed",
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "unexpected_field": true
        }),
    )
    .await;
    assert_problem(
        &malformed,
        "request.mcp.multi-root-execute.malformed",
        "invalid_request",
        "multi_root.invalid_request",
        "The multi-root application request is invalid",
        "never",
        &["correct_request"],
    );

    let page_zero = call_execute(
        &graph,
        Some(&executor),
        "request.mcp.multi-root-execute.page-zero",
        execute_arguments(SCOPE_SET_ID, 1, &digest, 0),
    )
    .await;
    assert_eq!(page_zero.semantic_error(), None);
    let page_zero_body = envelope(&page_zero);
    assert_eq!(
        page_zero_body["binding_id"],
        "binding.http.multi_root.execute.v1"
    );
    assert_eq!(
        page_zero_body["application"]["contract"]["schema_id"],
        "schema.tracedecay.multi-root.execute-result.v1"
    );
    assert_eq!(
        page_zero_body["application"]["contract"]["schema_revision"],
        1
    );
    assert_eq!(
        page_zero_body["application"]["request_id"],
        "request.mcp.multi-root-execute.page-zero"
    );
    assert_eq!(
        page_zero_body["application"]["outcome"]["outcome"],
        "evidence"
    );
    let page = page_zero_body
        .pointer("/application/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("execute evidence has no payload: {page_zero_body}"));
    assert_eq!(page["scope_set_id"], SCOPE_SET_ID);
    assert_eq!(page["scope_set_revision"], 1);
    assert_eq!(page["scope_set_digest"], digest);
    assert!(page["continuation"].is_null());
    assert_eq!(
        symbol_names(&page),
        vec![ALPHA_NAME.to_owned(), BETA_NAME.to_owned()]
    );

    let stale = call_execute(
        &graph,
        Some(&executor),
        "request.mcp.multi-root-execute.stale",
        execute_arguments(SCOPE_SET_ID, 1, &format!("sha256:{}", "a".repeat(64)), 0),
    )
    .await;
    assert_problem(
        &stale,
        "request.mcp.multi-root-execute.stale",
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        "The requested resource was not found or is not authorized",
        "never",
        &[],
    );

    let missing = call_execute(
        &graph,
        Some(&executor),
        "request.mcp.multi-root-execute.missing",
        execute_arguments("scope-set.mcp-execute-proof.missing", 1, &digest, 0),
    )
    .await;
    assert_problem(
        &missing,
        "request.mcp.multi-root-execute.missing",
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        "The requested resource was not found or is not authorized",
        "never",
        &[],
    );

    let next_page = call_execute(
        &graph,
        Some(&executor),
        "request.mcp.multi-root-execute.next-page",
        execute_arguments(SCOPE_SET_ID, 1, &digest, 1),
    )
    .await;
    assert_problem(
        &next_page,
        "request.mcp.multi-root-execute.next-page",
        "invalid_request",
        "multi_root.invalid_request",
        "The multi-root application request is invalid",
        "never",
        &["correct_request"],
    );
}
