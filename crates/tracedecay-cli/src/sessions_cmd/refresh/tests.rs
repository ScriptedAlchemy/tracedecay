use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clap::Parser;
use serde_json::{Value, json};
use tracedecay_contracts::retained_surfaces::{
    RetainedOutcomeStatusV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    SessionRefreshActionRequestV1, SessionRefreshBeginResultV1, SessionRefreshCancelResultV1,
    SessionRefreshScopeV1, SessionRefreshStatusResultV1,
};
use tracedecay_contracts::{
    ApplicationEnvelope, AuthorityReceipt, CancellationContext, CancellationSignal,
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, EvidencePacket, OperationReceipt, PageState, PolicyDecisionRef, RequestContext,
    RequestId, ResolvedScope, RetainedSurfaceExecutionContextV1, RetrievalEvidence, TemporalState,
    retained_receipts, retained_surface_application_operation,
};
use tracedecay_daemon_service::application_surface::retained::decode_request;
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::SortContractId;

use super::{
    SessionRefreshDaemonFuture, SessionRefreshDaemonTransport, SessionRefreshOperation,
    SessionRefreshOutcomeView, SessionRefreshSelectors, dispatch_session_refresh,
    execute_session_refresh, session_refresh_human_outcome,
};
use crate::cli::Cli;

const PROFILE_ID: &str = "profile.0f2f1c3d4e5f60718293a4b5c6d7e8f9";

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.cli.refresh").unwrap(),
        RepositoryId::new("repository.cli.refresh").unwrap(),
        WorktreeId::new("worktree.cli.refresh").unwrap(),
        None,
    )
    .unwrap()
}

fn context(operation: RetainedSurfaceOperation) -> RequestContext {
    let operation = retained_surface_application_operation(operation).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.cli.refresh").unwrap(),
        1,
        digest('a'),
        ActorId::new("actor.cli.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        scope(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.cli.caller").unwrap(),
        scope(),
        grant,
        RequestId::new("request.cli.refresh").unwrap(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationContext::active("cancel.cli.refresh").unwrap(),
    )
    .unwrap()
}

/// A daemon reply as the retained effect routes actually send it: the typed
/// begin/cancel payload inside a receipt-bearing effect envelope.
fn effect_reply(operation: RetainedSurfaceOperation, result: RetainedSurfaceResultV1) -> Value {
    let application_operation = retained_surface_application_operation(operation).unwrap();
    let context = context(operation);
    let cancellation = CancellationSignal::active("cancel.cli.refresh").unwrap();
    let execution = RetainedSurfaceExecutionContextV1 {
        request_context: &context,
        cancellation_signal: &cancellation,
        operation: &application_operation,
        observed_at: UtcMicros(1),
    };
    let outcome = retained_receipts::session_refresh_effect_outcome(
        &execution,
        operation,
        &digest('b'),
        &"request fixture",
        "refresh.operation.fixture",
        result,
        false,
    )
    .unwrap();
    serde_json::to_value(ApplicationEnvelope {
        contract: application_operation.result_contract().clone(),
        request_id: context.request_id().clone(),
        scope: scope(),
        outcome,
    })
    .unwrap()
}

/// A daemon reply as the read-only status route sends it.
fn evidence_reply(result: RetainedSurfaceResultV1) -> Value {
    let operation = RetainedSurfaceOperation::SessionRefreshStatus;
    let application_operation = retained_surface_application_operation(operation).unwrap();
    let context = context(operation);
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.cli.refresh",
            1,
            digest('b'),
            ComponentVersion::new("policy.evaluator.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap();
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let evidence = RetrievalEvidence {
        payload: Some(serde_json::to_value(result).unwrap()),
        temporal: TemporalState::current(UtcMicros(2)),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.cli.refresh.v1").unwrap(),
            1,
            Some(1),
            1,
        )
        .unwrap(),
        finished_at: UtcMicros(3),
        budget: Default::default(),
        cancellation: None,
    };
    let packet = EvidencePacket::from_retrieval(evidence, authority, receipt).unwrap();
    serde_json::to_value(ApplicationEnvelope::evidence(
        application_operation.result_contract().clone(),
        context.request_id().clone(),
        scope(),
        packet,
    ))
    .unwrap()
}

fn begin_result(outcome: RetainedOutcomeStatusV1, handle: &str) -> RetainedSurfaceResultV1 {
    RetainedSurfaceResultV1::SessionRefreshBegin(SessionRefreshBeginResultV1 {
        outcome,
        scope: "profile".to_owned(),
        tool: "tracedecay_session_refresh_begin".to_owned(),
        accepted_at: Some(2),
        handle: Some(handle.to_owned()),
        operation_id: Some("refresh.operation.fixture".to_owned()),
        progress: None,
        receipt: None,
        error: None,
    })
}

fn status_result(value: Value) -> RetainedSurfaceResultV1 {
    let mut result = json!({
        "outcome": "running",
        "scope": "profile",
        "tool": "tracedecay_session_refresh_status",
        "progress": null,
        "receipt": null,
        "error": null,
    });
    for (key, value) in value.as_object().unwrap() {
        result[key] = value.clone();
    }
    RetainedSurfaceResultV1::SessionRefreshStatus(
        serde_json::from_value::<SessionRefreshStatusResultV1>(result).unwrap(),
    )
}

fn cancel_result(value: Value) -> RetainedSurfaceResultV1 {
    let mut result = json!({
        "outcome": "cancelled",
        "scope": "profile",
        "tool": "tracedecay_session_refresh_cancel",
        "accepted_at": null,
        "handle": "srh_fixture",
        "operation_id": "refresh.operation.fixture",
        "progress": null,
        "receipt": null,
        "error": null,
    });
    for (key, value) in value.as_object().unwrap() {
        result[key] = value.clone();
    }
    RetainedSurfaceResultV1::SessionRefreshCancel(
        serde_json::from_value::<SessionRefreshCancelResultV1>(result).unwrap(),
    )
}

fn receipt(state: &str) -> Value {
    json!({
        "operation_id": "refresh.operation.fixture",
        "session_id": "session.profile",
        "frontier": { "observed_through": 9, "committed_through": 4 },
        "coverage": { "visible": 3, "hidden": 1, "unknown": 0, "redacted": 0 },
        "state": state,
        "failure_code": null,
        "terminal_at": 7,
    })
}

fn project_selectors() -> SessionRefreshSelectors {
    SessionRefreshSelectors {
        project_id: None,
        project_path: Some("registered-alias".to_owned()),
        profile_id: None,
        session_id: "session.refresh".to_owned(),
        provider: "cursor".to_owned(),
        source: 4,
        target: 9,
    }
}

fn profile_selectors() -> SessionRefreshSelectors {
    SessionRefreshSelectors {
        project_id: None,
        project_path: None,
        profile_id: Some(PROFILE_ID.to_owned()),
        session_id: "session.profile".to_owned(),
        provider: "claude".to_owned(),
        source: 2,
        target: 7,
    }
}

fn project_fixture_authorities() -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join("tracedecay-session-refresh-fixture");
    assert!(base.is_absolute(), "session refresh fixture base");
    (
        base.join("authoritative-worktree"),
        base.join("repository").join(".git"),
    )
}

fn registry_context(project_root: &Path, git_common_dir: &Path) -> Value {
    let project_root = project_root.to_string_lossy().into_owned();
    let git_common_dir = git_common_dir.to_string_lossy().into_owned();
    json!({
        "status": "ok",
        "profile_id": PROFILE_ID,
        "project": {
            "project_id": "project.registered",
            "display_root": project_root,
            "canonical_root": project_root,
            "git_common_dir": git_common_dir,
            "default_branch": "master"
        },
        "aliases": [{
            "alias_path": "registered-alias",
            "project_id": "project.registered",
            "last_seen_at": 42
        }],
        "stores": [{
            "store": {
                "store_id": "store.authoritative",
                "project_id": "project.registered",
                "store_kind": "code_project",
                "storage_mode": "profile_sharded",
                "store_relpath": "projects/project.registered"
            },
            "graph_scopes": [
                {
                    "graph_scope_id": "scope.default",
                    "project_id": "project.registered",
                    "store_id": "store.authoritative",
                    "branch_name": "master",
                    "db_relpath": "codegraph-master.db",
                    "writable": true
                },
                {
                    "graph_scope_id": "scope.selected",
                    "project_id": "project.registered",
                    "store_id": "store.authoritative",
                    "branch_name": "feature/selected",
                    "db_relpath": "codegraph-selected.db",
                    "writable": true
                }
            ],
            "artifacts": []
        }]
    })
}

fn active_project_context(project_root: &Path) -> Value {
    json!({
        "project_root": project_root.to_string_lossy(),
        "resolution_source": "active_project",
        "branch": {
            "current_branch": "feature/selected",
            "open_active_branch": "feature/selected",
            "serving_branch": "feature/selected",
            "branch_drifted": false,
            "is_fallback": false
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
struct RecordedCall {
    project_root: Option<PathBuf>,
    tool_name: String,
    arguments: Value,
}

struct FakeDaemonTransport {
    responses: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakeDaemonTransport {
    fn new(responses: impl IntoIterator<Item = Value>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl SessionRefreshDaemonTransport for FakeDaemonTransport {
    fn call<'a>(
        &'a self,
        project_root: Option<&'a Path>,
        tool_name: &'a str,
        arguments: Value,
    ) -> SessionRefreshDaemonFuture<'a> {
        self.calls.lock().unwrap().push(RecordedCall {
            project_root: project_root.map(Path::to_path_buf),
            tool_name: tool_name.to_owned(),
            arguments,
        });
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("fake daemon response");
        Box::pin(async move { Ok(response) })
    }
}

/// Every payload the CLI sends must be the exact canonical request the daemon
/// decodes for that operation: no `action`, no `profile` object, no
/// transport-only selector.
fn assert_canonical(operation: RetainedSurfaceOperation, payload: &Value) {
    let decoded = decode_request(operation, payload.clone())
        .unwrap_or_else(|error| panic!("{} payload must decode: {error}", operation.as_str()));
    assert_eq!(decoded.operation(), operation);
    let request: SessionRefreshActionRequestV1 = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), *payload);
}

#[tokio::test]
async fn project_refresh_uses_registered_authorities_and_the_canonical_payload() {
    let (project_root, git_common_dir) = project_fixture_authorities();
    let project_root_text = project_root.to_string_lossy().into_owned();
    let repository_id = git_common_dir.to_string_lossy().into_owned();
    let transport = FakeDaemonTransport::new([
        registry_context(&project_root, &git_common_dir),
        active_project_context(&project_root),
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshBegin,
            begin_result(RetainedOutcomeStatusV1::Started, "opaque-refresh-handle"),
        ),
    ]);

    let (view, _) = execute_session_refresh(
        &transport,
        SessionRefreshOperation::Begin,
        &project_selectors(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(view.handle.as_deref(), Some("opaque-refresh-handle"));
    let calls = transport.calls();
    assert_eq!(
        calls[0],
        RecordedCall {
            project_root: None,
            tool_name: "tracedecay_admin_cli".to_owned(),
            arguments: json!({
                "action": "registry_context",
                "project_arg": "registered-alias"
            }),
        }
    );
    assert_eq!(
        calls[1],
        RecordedCall {
            project_root: Some(project_root.clone()),
            tool_name: "tracedecay_active_project".to_owned(),
            arguments: json!({ "format": "json" }),
        }
    );
    assert_eq!(
        calls[2],
        RecordedCall {
            project_root: Some(project_root),
            tool_name: "tracedecay_session_refresh_begin".to_owned(),
            arguments: json!({
                "scope": {
                    "kind": "project",
                    "project": {
                        "id": "project.registered",
                        "profile_id": PROFILE_ID,
                        "repository_id": repository_id,
                        "worktree_id": project_root_text,
                        "branch_id": "scope.selected"
                    }
                },
                "session": {
                    "id": "session.refresh",
                    "store_id": "store.authoritative",
                    "root_id": "scope.selected"
                },
                "source": { "scope": "cursor" },
                "target": {
                    "temporal_mode": { "kind": "current" },
                    "grain": "logical_message",
                    "frontier": {
                        "observed_through": 9,
                        "committed_through": 4
                    }
                },
                "handle": null,
                "format": "json"
            }),
        }
    );
    assert_canonical(
        RetainedSurfaceOperation::SessionRefreshBegin,
        &calls[2].arguments,
    );
}

#[tokio::test]
async fn profile_refresh_stays_projectless_and_roundtrips_only_the_opaque_handle() {
    let transport = FakeDaemonTransport::new([
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshBegin,
            begin_result(RetainedOutcomeStatusV1::Started, "opaque-profile-handle"),
        ),
        evidence_reply(status_result(json!({ "outcome": "running" }))),
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshCancel,
            cancel_result(json!({
                "handle": "opaque-profile-handle",
                "receipt": receipt("cancelled")
            })),
        ),
    ]);
    let selectors = profile_selectors();

    let (begun, _) =
        execute_session_refresh(&transport, SessionRefreshOperation::Begin, &selectors, None)
            .await
            .unwrap();
    let handle = begun.handle.as_deref().expect("begin handle");
    execute_session_refresh(
        &transport,
        SessionRefreshOperation::Status,
        &selectors,
        Some(handle),
    )
    .await
    .unwrap();
    execute_session_refresh(
        &transport,
        SessionRefreshOperation::Cancel,
        &selectors,
        Some(handle),
    )
    .await
    .unwrap();

    let calls = transport.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.tool_name.as_str())
            .collect::<Vec<_>>(),
        [
            "tracedecay_session_refresh_begin",
            "tracedecay_session_refresh_status",
            "tracedecay_session_refresh_cancel",
        ]
    );
    let suffix = PROFILE_ID.strip_prefix("profile.").unwrap();
    for (call, operation) in calls.iter().zip([
        RetainedSurfaceOperation::SessionRefreshBegin,
        RetainedSurfaceOperation::SessionRefreshStatus,
        RetainedSurfaceOperation::SessionRefreshCancel,
    ]) {
        assert_eq!(
            call.project_root, None,
            "profile refresh never names a project"
        );
        assert_eq!(
            call.arguments["scope"],
            json!({ "kind": "profile", "profile_id": PROFILE_ID })
        );
        assert_eq!(
            call.arguments["session"]["store_id"],
            format!("store.profile.{suffix}")
        );
        assert_eq!(
            call.arguments["session"]["root_id"],
            format!("root.profile.{suffix}")
        );
        assert!(call.arguments.get("project").is_none());
        assert!(call.arguments.get("profile").is_none());
        assert!(call.arguments.get("action").is_none());
        assert_canonical(operation, &call.arguments);
    }
    assert_eq!(calls[0].arguments["handle"], Value::Null);
    assert_eq!(calls[1].arguments["handle"], "opaque-profile-handle");
    assert_eq!(calls[2].arguments["handle"], "opaque-profile-handle");
    assert_ne!(
        calls[1].arguments["handle"],
        Value::String("refresh.operation.fixture".to_owned()),
        "the CLI must forward the opaque handle, never the durable operation id"
    );
}

#[tokio::test]
async fn refresh_without_explicit_scope_never_calls_daemon_or_discovers_cwd() {
    let transport = FakeDaemonTransport::new([]);
    let mut selectors = project_selectors();
    selectors.project_path = None;

    let error =
        execute_session_refresh(&transport, SessionRefreshOperation::Begin, &selectors, None)
            .await
            .expect_err("refresh must not use the current directory as implicit scope");

    assert!(error.to_string().contains("never falls back"));
    assert!(transport.calls().is_empty());
}

#[test]
fn profile_selector_must_be_the_typed_profile_identity() {
    for invalid in ["primary", "profile.", ""] {
        let error = super::profile_refresh_scope(invalid).expect_err("untyped profile id");
        assert!(error.to_string().contains("profile.<id>"), "{invalid:?}");
    }
    let resolved = super::profile_refresh_scope("profile.primary").unwrap();
    assert_eq!(
        resolved.scope,
        SessionRefreshScopeV1::Profile {
            profile_id: "profile.primary".to_owned()
        }
    );
    assert_eq!(resolved.store_id, "store.profile.primary");
    assert_eq!(resolved.root_id, "root.profile.primary");
    assert_eq!(resolved.project_root, None);
}

#[tokio::test]
async fn typed_semantic_refusal_produces_a_nonzero_command_result() {
    let transport = FakeDaemonTransport::new([evidence_reply(status_result(json!({
        "outcome": "wrong_scope",
        "error": {
            "code": "refresh_wrong_scope",
            "message": "the refresh handle does not belong to the requested scope"
        }
    })))]);

    let error = dispatch_session_refresh(
        &transport,
        SessionRefreshOperation::Status,
        &profile_selectors(),
        Some("opaque-other-scope"),
        false,
    )
    .await
    .expect_err("semantic daemon failures must produce a nonzero CLI result");

    assert!(error.to_string().contains("wrong scope"), "{error}");
}

#[tokio::test]
async fn a_problem_envelope_is_reported_as_the_daemon_refusal() {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::SessionRefreshBegin)
            .unwrap();
    let reply = serde_json::to_value(
        tracedecay_contracts::ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            RequestId::new("request.cli.refresh").unwrap(),
            tracedecay_contracts::ApplicationProblem::not_found_or_not_authorized(
                tracedecay_contracts::RetryDirective::Never,
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let transport = FakeDaemonTransport::new([reply]);

    let error = execute_session_refresh(
        &transport,
        SessionRefreshOperation::Begin,
        &profile_selectors(),
        None,
    )
    .await
    .expect_err("a problem envelope is a refusal");

    assert!(
        error
            .to_string()
            .contains("tracedecay_session_refresh_begin refused"),
        "{error}"
    );
}

#[test]
fn refresh_outcomes_have_human_labels_and_failure_semantics() {
    let view = |outcome| SessionRefreshOutcomeView {
        outcome,
        handle: None,
        operation_id: None,
        progress: None,
        receipt: None,
        error: None,
    };
    for (outcome, failure) in [
        (RetainedOutcomeStatusV1::Started, false),
        (RetainedOutcomeStatusV1::Joined, false),
        (RetainedOutcomeStatusV1::Running, false),
        (RetainedOutcomeStatusV1::Complete, false),
        (RetainedOutcomeStatusV1::Busy, true),
        (RetainedOutcomeStatusV1::Failed, true),
        (RetainedOutcomeStatusV1::Denied, true),
        (RetainedOutcomeStatusV1::WrongScope, true),
        (RetainedOutcomeStatusV1::Stale, true),
        (RetainedOutcomeStatusV1::NotFound, true),
        (RetainedOutcomeStatusV1::Aborted, true),
        (RetainedOutcomeStatusV1::DeadlineExceeded, true),
        (RetainedOutcomeStatusV1::Unavailable, true),
        (RetainedOutcomeStatusV1::Error, true),
    ] {
        assert_eq!(
            view(outcome).is_failure(),
            failure,
            "unexpected CLI failure semantics for {outcome:?}"
        );
    }
    assert_eq!(
        view(RetainedOutcomeStatusV1::DeadlineExceeded).label(),
        "deadline exceeded"
    );

    let started = SessionRefreshOutcomeView {
        handle: Some("opaque-refresh-handle".to_owned()),
        operation_id: Some("internal-operation".to_owned()),
        ..view(RetainedOutcomeStatusV1::Started)
    };
    assert_eq!(
        session_refresh_human_outcome(&started),
        "session refresh started (handle opaque-refresh-handle); operation internal-operation"
    );

    let running = SessionRefreshOutcomeView {
        progress: Some(
            serde_json::from_value(json!({
                "operation_id": "refresh-operation",
                "session_id": "session.profile",
                "frontier": { "observed_through": 9, "committed_through": 4 },
                "coverage": { "visible": 3, "hidden": 1, "unknown": 0, "redacted": 0 },
                "committed_batches": 2,
                "committed_records": 4,
                "updated_at": 5
            }))
            .unwrap(),
        ),
        ..view(RetainedOutcomeStatusV1::Running)
    };
    assert_eq!(
        session_refresh_human_outcome(&running),
        "session refresh running; frontier 4/9; coverage visible 3, hidden 1, unknown 0, redacted 0; committed batches 2; committed records 4"
    );

    let invalid = SessionRefreshOutcomeView {
        error: Some(
            serde_json::from_value(json!({
                "code": "invalid_request",
                "message": "status requires a handle"
            }))
            .unwrap(),
        ),
        ..view(RetainedOutcomeStatusV1::Error)
    };
    assert!(invalid.is_failure());
    assert_eq!(
        session_refresh_human_outcome(&invalid),
        "session refresh error: status requires a handle"
    );
}

#[test]
fn a_cancelled_outcome_without_a_receipt_is_not_durable() {
    let reply = effect_reply(
        RetainedSurfaceOperation::SessionRefreshCancel,
        cancel_result(json!({ "receipt": null })),
    );
    let error = SessionRefreshOutcomeView::decode(SessionRefreshOperation::Cancel, reply)
        .expect_err("durable cancellation requires a receipt");
    assert!(
        error
            .to_string()
            .contains("omitted durable cancellation receipt"),
        "{error}"
    );

    let (view, _) = SessionRefreshOutcomeView::decode(
        SessionRefreshOperation::Cancel,
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshCancel,
            cancel_result(json!({ "receipt": receipt("cancelled") })),
        ),
    )
    .unwrap();
    assert!(!view.is_failure());
    assert!(
        session_refresh_human_outcome(&view).contains("receipt cancelled"),
        "{}",
        session_refresh_human_outcome(&view)
    );
}

#[test]
fn refresh_cli_accepts_begin_status_cancel_only() {
    let selectors = [
        "--profile-id",
        PROFILE_ID,
        "--session-id",
        "session.profile",
        "--provider",
        "cursor",
        "--source",
        "2",
        "--target",
        "7",
    ];
    Cli::try_parse_from(
        ["tracedecay", "sessions", "refresh", "begin"]
            .into_iter()
            .chain(selectors),
    )
    .unwrap_or_else(|error| panic!("refresh begin should parse: {error}"));
    for action in ["status", "cancel"] {
        Cli::try_parse_from(
            ["tracedecay", "sessions", "refresh", action]
                .into_iter()
                .chain(selectors)
                .chain(["--handle", "opaque-handle"]),
        )
        .unwrap_or_else(|error| panic!("refresh action `{action}` should parse: {error}"));
        assert!(
            Cli::try_parse_from(
                ["tracedecay", "sessions", "refresh", action]
                    .into_iter()
                    .chain(selectors)
                    .chain(["--operation-id", "opaque-handle"]),
            )
            .is_err(),
            "the removed --operation-id alias must not parse for `{action}`"
        );
    }
    for removed in ["start", "join", "resume"] {
        assert!(
            Cli::try_parse_from(
                ["tracedecay", "sessions", "refresh", removed]
                    .into_iter()
                    .chain(selectors),
            )
            .is_err(),
            "removed alias `{removed}` must not parse"
        );
    }
}

#[tokio::test]
async fn joined_complete_and_cancelled_are_successful_cli_outcomes() {
    let transport = FakeDaemonTransport::new([
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshBegin,
            begin_result(RetainedOutcomeStatusV1::Joined, "joined-handle"),
        ),
        evidence_reply(status_result(json!({
            "outcome": "complete",
            "receipt": receipt("complete")
        }))),
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshCancel,
            cancel_result(json!({
                "handle": "joined-handle",
                "receipt": receipt("cancelled")
            })),
        ),
    ]);
    let selectors = profile_selectors();

    dispatch_session_refresh(
        &transport,
        SessionRefreshOperation::Begin,
        &selectors,
        None,
        false,
    )
    .await
    .unwrap();
    dispatch_session_refresh(
        &transport,
        SessionRefreshOperation::Status,
        &selectors,
        Some("joined-handle"),
        false,
    )
    .await
    .unwrap();
    dispatch_session_refresh(
        &transport,
        SessionRefreshOperation::Cancel,
        &selectors,
        Some("joined-handle"),
        false,
    )
    .await
    .unwrap();

    let calls = transport.calls();
    assert_eq!(calls[1].arguments["handle"], "joined-handle");
    assert_eq!(calls[2].arguments["handle"], "joined-handle");
}

#[tokio::test]
async fn begin_rejects_a_handle_and_status_requires_one() {
    let transport = FakeDaemonTransport::new([]);
    let error = execute_session_refresh(
        &transport,
        SessionRefreshOperation::Begin,
        &profile_selectors(),
        Some("stray-handle"),
    )
    .await
    .expect_err("begin must not accept a handle");
    assert!(error.to_string().contains("does not accept a handle"));
    let error = execute_session_refresh(
        &transport,
        SessionRefreshOperation::Status,
        &profile_selectors(),
        Some("   "),
    )
    .await
    .expect_err("status requires a non-empty handle");
    assert!(error.to_string().contains("requires the handle"));
    assert!(transport.calls().is_empty());
}

/// The daemon answers `ApplicationOutcome::Effect` for begin and cancel; a
/// status (evidence) shape on those routes is envelope drift, not a success.
#[test]
fn begin_and_cancel_require_effect_envelopes() {
    let error = SessionRefreshOutcomeView::decode(
        SessionRefreshOperation::Begin,
        evidence_reply(begin_result(RetainedOutcomeStatusV1::Started, "srh")),
    )
    .expect_err("an evidence envelope is not a begin effect");
    assert!(error.to_string().contains("non-effect outcome"), "{error}");
    let error = SessionRefreshOutcomeView::decode(
        SessionRefreshOperation::Status,
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshBegin,
            begin_result(RetainedOutcomeStatusV1::Started, "srh"),
        ),
    )
    .expect_err("an effect envelope is not a status read");
    assert!(
        error.to_string().contains("non-evidence outcome"),
        "{error}"
    );
}

#[test]
fn outcome_view_matches_the_typed_result_it_decoded() {
    let (view, payload) = SessionRefreshOutcomeView::decode(
        SessionRefreshOperation::Begin,
        effect_reply(
            RetainedSurfaceOperation::SessionRefreshBegin,
            begin_result(RetainedOutcomeStatusV1::Started, "srh_fixture"),
        ),
    )
    .unwrap();
    assert_eq!(view.outcome, RetainedOutcomeStatusV1::Started);
    assert_eq!(payload["tool"], "tracedecay_session_refresh_begin");
    assert_eq!(payload["scope"], "profile");
    assert_eq!(payload["handle"], "srh_fixture");
}
