//! Observable behavior of `tracedecay_session_refresh_cancel`.
//!
//! Agents call this tool with the opaque handle from
//! `tracedecay_session_refresh_begin`. Success is the durable receipt the
//! daemon stored, not the word "cancelled". A refresh that already finished
//! keeps that terminal receipt. A refresh the worker has not finished is
//! cancelled in the store. Missing, unknown, and stale handles, and an
//! unmounted profile refresh authority, are typed refusals.

use crate::support::{GLOBAL_DB_ENV_LOCK, GlobalDbEnvGuard, HomeEnvGuard, extract_text};
#[cfg(feature = "test-transport")]
use crate::{common, fixture};
use serde_json::{Value, json};
use std::path::Path;
#[cfg(feature = "test-transport")]
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::tools::{ToolCallRegistryOptions, handle_tool_call_with_registry_options};
use tracedecay::project::TraceDecay;
use tracedecay_contracts::SessionTemporalRefreshWakePort;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_daemon_service::DaemonSessionRefreshService;
use tracedecay_mcp::handlers::SessionAuthorities;
use tracedecay_runtime_core::storage::default_profile_root;

const CANCEL_RESULT_SCHEMA: &str = "schema.application.retained.session-refresh-cancel.result";
const SESSION_ID: &str = "session.cancel-proof";

/// Wake that the refresh service treats as delivered, without projecting.
/// The admitted operation therefore stays non-terminal until cancel writes
/// its receipt.
struct AcceptedIdleSessionRefreshWake;

impl SessionTemporalRefreshWakePort for AcceptedIdleSessionRefreshWake {
    fn wake(&self) -> bool {
        true
    }

    fn is_unavailable(&self) -> bool {
        false
    }

    fn wake_and_wait_until_idle(
        &self,
        _timeout: Duration,
    ) -> tracedecay_contracts::SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(async { true })
    }
}

fn refresh_arguments(session_id: &str, handle: Option<&str>) -> Value {
    let mut arguments = json!({
        "scope": { "kind": "profile" },
        "session": { "id": session_id },
        "source": { "scope": "codex" },
        "target": {
            "temporal_mode": { "kind": "current" },
            "grain": "logical_message",
            "frontier": { "observed_through": 0, "committed_through": 0 }
        },
        "format": "json"
    });
    if let Some(handle) = handle {
        arguments["handle"] = json!(handle);
    }
    arguments
}

fn tool_envelope(result: &Value) -> Value {
    serde_json::from_str(extract_text(result)).unwrap_or_else(|error| {
        panic!("session refresh cancel must answer JSON, got {error}: {result}")
    })
}

fn assert_cancel_contract(envelope: &Value) {
    assert_eq!(
        envelope["contract"]["schema_id"], CANCEL_RESULT_SCHEMA,
        "{envelope}"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1, "{envelope}");
}

fn assert_problem(envelope: &Value, expected: Value) {
    assert_cancel_contract(envelope);
    let request_id = envelope["request_id"]
        .as_str()
        .unwrap_or_else(|| panic!("problem envelope omitted request_id: {envelope}"));
    assert_eq!(envelope["problem"]["request_id"], request_id, "{envelope}");
    assert_eq!(envelope["problem"]["trace_id"], request_id, "{envelope}");
    let mut observed = envelope["problem"].clone();
    observed["request_id"] = json!("request.cancel-proof");
    observed["trace_id"] = json!("request.cancel-proof");
    assert_eq!(observed, expected, "problem record diverged: {envelope}");
}

fn problem_record(
    kind: &str,
    code: &str,
    message: &str,
    diagnostic: Value,
    terminality: &str,
    retryable: bool,
    retry: &str,
    retry_scope: Value,
    retry_after_millis: Value,
    unavailable_classification: Value,
    legal_actions: Value,
) -> Value {
    json!({
        "revision": 1,
        "kind": kind,
        "code": code,
        "message": message,
        "diagnostic": diagnostic,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": terminality,
        "retryable": retryable,
        "retry": retry,
        "retry_scope": retry_scope,
        "retry_after_millis": retry_after_millis,
        "cancellation_stage": null,
        "unavailable_classification": unavailable_classification,
        "execution_failure_classification": null,
        "request_id": "request.cancel-proof",
        "trace_id": "request.cancel-proof",
        "details": [],
        "legal_actions": legal_actions,
        "coverage": null
    })
}

fn effect_payload(envelope: &Value) -> Value {
    assert_cancel_contract(envelope);
    assert_eq!(envelope["outcome"]["outcome"], "effect", "{envelope}");
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("cancel effect omitted its payload: {envelope}"))
}

fn assert_cancel_payload(
    payload: &Value,
    outcome: &str,
    receipt_state: &str,
    session_id: &str,
    operation_id: &str,
    handle: &str,
) {
    assert_eq!(payload["outcome"], outcome, "{payload}");
    assert_eq!(payload["scope"], "profile", "{payload}");
    assert_eq!(
        payload["tool"], "tracedecay_session_refresh_cancel",
        "{payload}"
    );
    assert_eq!(payload["accepted_at"], Value::Null, "{payload}");
    assert_eq!(payload["handle"], handle, "{payload}");
    assert_eq!(payload["operation_id"], operation_id, "{payload}");
    assert_eq!(payload["progress"], Value::Null, "{payload}");
    assert_eq!(payload["error"], Value::Null, "{payload}");
    assert_eq!(payload["receipt"]["state"], receipt_state, "{payload}");
    assert_eq!(
        payload["receipt"]["operation_id"], operation_id,
        "{payload}"
    );
    assert_eq!(payload["receipt"]["session_id"], session_id, "{payload}");
    assert_eq!(payload["receipt"]["failure_code"], Value::Null, "{payload}");
}

async fn dispatch_cancel(
    graph: &TraceDecay,
    profile_root: &Path,
    authority: &tracedecay_session_runtime::retained::ProfileRetainedConnectionAuthorityV1,
    refresh: Option<&dyn tracedecay_session_runtime::retained::RetainedSessionRefreshPortV1>,
    arguments: Value,
) -> Value {
    let result = handle_tool_call_with_registry_options(
        graph,
        "tracedecay_session_refresh_cancel",
        arguments,
        None,
        None,
        ToolCallRegistryOptions {
            profile_root: Some(profile_root),
            session_authorities: SessionAuthorities::default()
                .with_profile_retained_authority(Some(authority))
                .with_profile_session_refresh(refresh),
            ..Default::default()
        },
    )
    .await
    .expect("session refresh cancel dispatch");
    tool_envelope(&result.value)
}

#[cfg(feature = "test-transport")]
async fn production_call(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} invocation failed: {error}"));
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} returned a transport error: {:?}", response.error));
    tool_envelope(&result)
}

#[cfg(feature = "test-transport")]
fn git(project: &Path, args: &[&str]) {
    let status = Command::new(common::git_program())
        .args(args)
        .current_dir(project)
        .status()
        .unwrap_or_else(|error| panic!("git {args:?} failed to start: {error}"));
    assert!(status.success(), "git {args:?} failed: {status}");
}

/// The production MCP server answers cancel the way an agent calls it:
/// typed refusals for bad handles, and the already-written complete receipt
/// once the refresh has finished.
#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_mcp_cancel_refuses_bad_handles_and_keeps_a_finished_receipt() {
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let root = crate::support::test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let project = isolation.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    fixture::write_indexed_fixture_sources(&project);
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "session refresh cancel fixture",
        ],
    );

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition");

    let missing = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, None),
    )
    .await;
    assert_problem(
        &missing,
        problem_record(
            "invalid_request",
            "application.retained.invalid-request",
            "The retained operation request is invalid.",
            json!({
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            }),
            "pre_admission",
            false,
            "never",
            Value::Null,
            Value::Null,
            Value::Null,
            json!(["correct_request"]),
        ),
    );

    let blank = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, Some(" ")),
    )
    .await;
    assert_problem(
        &blank,
        problem_record(
            "invalid_request",
            "application.retained.invalid-request",
            "The retained operation request is invalid.",
            json!({
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            }),
            "pre_admission",
            false,
            "never",
            Value::Null,
            Value::Null,
            Value::Null,
            json!(["correct_request"]),
        ),
    );

    let unknown = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, Some("not-a-handle")),
    )
    .await;
    assert_problem(
        &unknown,
        problem_record(
            "not_found_or_not_authorized",
            "not_found_or_not_authorized",
            "The requested resource was not found or is not authorized",
            Value::Null,
            "pre_admission",
            false,
            "never",
            Value::Null,
            Value::Null,
            Value::Null,
            json!([]),
        ),
    );

    let stale_handle = format!("srh_{}", "a".repeat(64));
    let stale = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, Some(&stale_handle)),
    )
    .await;
    assert_problem(
        &stale,
        problem_record(
            "stale",
            "application.retained.stale",
            "The retained authority is stale for this request.",
            json!({
                "code": "application.retained.stale",
                "message": "The retained authority is stale for this request."
            }),
            "pre_admission",
            true,
            "after_revalidate",
            json!("fresh_request"),
            Value::Null,
            Value::Null,
            json!(["refresh"]),
        ),
    );

    let begun = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_begin",
        refresh_arguments(SESSION_ID, None),
    )
    .await;
    let begin = begun
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("begin omitted its payload: {begun}"));
    assert_eq!(begin["outcome"], "started", "{begin}");
    assert_eq!(begin["scope"], "profile", "{begin}");
    assert_eq!(begin["tool"], "tracedecay_session_refresh_begin", "{begin}");
    let handle = begin["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted its handle: {begin}"))
        .to_owned();
    let operation_id = begin["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted its operation id: {begin}"))
        .to_owned();
    assert_eq!(handle.len(), "srh_".len() + 64, "{handle}");
    assert!(handle.starts_with("srh_"), "{handle}");
    assert_ne!(handle, operation_id);

    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = production_call(
                &harness,
                &project,
                "tracedecay_session_refresh_status",
                refresh_arguments(SESSION_ID, Some(&handle)),
            )
            .await;
            let payload = status
                .pointer("/outcome/value/payload")
                .cloned()
                .unwrap_or(status);
            if payload["outcome"] == "complete" {
                break payload;
            }
            assert_eq!(payload["outcome"], "running", "{payload}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("empty session refresh should reach a complete receipt");
    assert_eq!(finished["receipt"]["state"], "complete", "{finished}");
    assert_eq!(
        finished["receipt"]["operation_id"], operation_id,
        "{finished}"
    );

    let cancelled = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, Some(&handle)),
    )
    .await;
    let payload = effect_payload(&cancelled);
    assert_cancel_payload(
        &payload,
        "complete",
        "complete",
        SESSION_ID,
        &operation_id,
        &handle,
    );
    assert_eq!(payload["receipt"], finished["receipt"], "{payload}");

    let repeated = production_call(
        &harness,
        &project,
        "tracedecay_session_refresh_cancel",
        refresh_arguments(SESSION_ID, Some(&handle)),
    )
    .await;
    let repeated_payload = effect_payload(&repeated);
    assert_eq!(
        repeated_payload["outcome"], "complete",
        "{repeated_payload}"
    );
    assert_eq!(
        repeated_payload["receipt"], payload["receipt"],
        "a second cancel must return the same durable receipt"
    );

    harness.shutdown().await;
}

/// An idle wake holds the durable operation open, so cancel itself writes
/// the cancelled receipt and a repeat returns that same receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_of_an_unfinished_refresh_stores_a_cancelled_receipt() {
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let root = crate::support::test_temp_dir();
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("probe source");
    let (graph, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.session-refresh-cancel",
    )
    .await
    .expect("registered fixture");
    let profile_root = default_profile_root().expect("fixture profile root");
    let profile_identity =
        profile_identity::load_or_create(&profile_root).expect("fixture profile identity");
    let profile_id = profile_identity.profile_id().as_str().to_owned();
    let suffix = profile_id
        .strip_prefix("profile.")
        .expect("canonical profile identity prefix");
    let session_identity = tracedecay_session_memory::context::ResolvedSessionIdentity::for_profile(
        tracedecay_session_memory::context::ProfileId::new(profile_id).expect("profile id"),
        tracedecay_session_memory::context::SessionStoreId::new(format!("store.profile.{suffix}"))
            .expect("profile store id"),
        tracedecay_session_memory::context::SessionRootId::new(format!("root.profile.{suffix}"))
            .expect("profile root id"),
    );
    let authority = tracedecay_session_runtime::retained::profile_retained_connection_authority(
        &profile_identity,
        &session_identity,
    )
    .expect("profile retained authority");
    let profile_database = graph
        .store_runtime_registry()
        .profile_sessions()
        .await
        .expect("profile session database");
    let refresh = DaemonSessionRefreshService::new(
        profile_database,
        Arc::new(AcceptedIdleSessionRefreshWake),
        None,
    );
    let session_id = "session.idle-cancel";

    let unmounted = dispatch_cancel(
        &graph,
        &profile_root,
        &authority,
        None,
        refresh_arguments(session_id, Some("not-a-handle")),
    )
    .await;
    assert_problem(
        &unmounted,
        problem_record(
            "unavailable",
            "application.retained.authority-unavailable",
            "The retained operation authority is unavailable: the profile session refresh authority is not mounted for this connection",
            json!({
                "code": "application.retained.authority-unavailable",
                "message": "The retained operation authority is unavailable: the profile session refresh authority is not mounted for this connection"
            }),
            "pre_admission",
            true,
            "after_delay",
            json!("same_request"),
            json!(250),
            json!("authority"),
            json!(["retry"]),
        ),
    );

    let begun = {
        let result = handle_tool_call_with_registry_options(
            &graph,
            "tracedecay_session_refresh_begin",
            refresh_arguments(session_id, None),
            None,
            None,
            ToolCallRegistryOptions {
                profile_root: Some(&profile_root),
                session_authorities: SessionAuthorities::default()
                    .with_profile_retained_authority(Some(&authority))
                    .with_profile_session_refresh(Some(
                        &refresh
                            as &dyn tracedecay_session_runtime::retained::RetainedSessionRefreshPortV1,
                    )),
                ..Default::default()
            },
        )
        .await
        .expect("session refresh begin");
        tool_envelope(&result.value)
    };
    let begin = begun
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("begin omitted its payload: {begun}"));
    assert_eq!(begin["outcome"], "started", "{begin}");
    let handle = begin["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted its handle: {begin}"))
        .to_owned();
    let operation_id = begin["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted its operation id: {begin}"))
        .to_owned();

    let cancelled = dispatch_cancel(
        &graph,
        &profile_root,
        &authority,
        Some(&refresh),
        refresh_arguments(session_id, Some(&handle)),
    )
    .await;
    let payload = effect_payload(&cancelled);
    assert_cancel_payload(
        &payload,
        "cancelled",
        "cancelled",
        session_id,
        &operation_id,
        &handle,
    );
    assert_eq!(
        payload["receipt"]["frontier"],
        json!({ "observed_through": 0, "committed_through": 0 }),
        "{payload}"
    );
    assert_eq!(
        payload["receipt"]["coverage"],
        json!({ "visible": 0, "hidden": 0, "unknown": 0, "redacted": 0 }),
        "{payload}"
    );
    assert_eq!(
        payload["receipt"]["source_coverage"],
        json!([{
            "source_id": "session.idle-cancel:codex",
            "observed_frontier": 0,
            "committed_frontier": 0,
            "target_watermark": 0,
            "request": { "mode": { "kind": "current" } },
            "covered_intervals": [],
            "missing_intervals": [],
            "state": "fresh",
            "reason": { "kind": "caught_up" }
        }]),
        "{payload}"
    );

    let repeated = dispatch_cancel(
        &graph,
        &profile_root,
        &authority,
        Some(&refresh),
        refresh_arguments(session_id, Some(&handle)),
    )
    .await;
    let repeated_payload = effect_payload(&repeated);
    assert_eq!(
        repeated_payload["outcome"], "cancelled",
        "{repeated_payload}"
    );
    assert_eq!(repeated_payload["receipt"], payload["receipt"]);

    drop(refresh);
    graph.close();
}
