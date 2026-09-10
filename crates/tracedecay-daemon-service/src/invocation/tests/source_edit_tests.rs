use super::*;
use tracedecay_contracts::{
    ApplicationProblemKind, EffectId, IdempotencyKey, SourceEditInvocationV1, SourceEditKind,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationInvocationV1, SourceEditRequest,
    SourceEditRollbackInvocationV1,
};
use tracedecay_domain::ManifestDigest;

fn source_edit_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("source-edit deadline")
}

fn source_edit_cancellation(token: &str) -> CancellationContext {
    CancellationContext::active(token).expect("source-edit cancellation")
}

fn fixture_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest")
}

fn source_edit_invocation() -> SourceEditInvocationV1 {
    SourceEditInvocationV1 {
        edit: SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: "old".to_owned(),
            new_str: "new".to_owned(),
            dry_run: true,
            verify: false,
        },
        idempotency_key: Some(IdempotencyKey::new("source-edit.dispatch.preview").unwrap()),
        expected_state: Some(fixture_digest()),
    }
}

fn source_edit_reconcile_invocation() -> SourceEditReconciliationInvocationV1 {
    SourceEditReconciliationInvocationV1 {
        kind: SourceEditKind::StrReplace,
        effect_id: EffectId::new("effect.source-edit.dispatch").unwrap(),
        idempotency_key: IdempotencyKey::new("source-edit.dispatch.original").unwrap(),
        attempt_idempotency_key: IdempotencyKey::new("source-edit.dispatch.attempt").unwrap(),
        input_digest: fixture_digest(),
        disposition: SourceEditReconciliationDispositionV1::ConfirmRolledBack,
    }
}

fn source_edit_rollback_invocation() -> SourceEditRollbackInvocationV1 {
    SourceEditRollbackInvocationV1 {
        effect_id: EffectId::new("effect.source-edit.rollback").unwrap(),
        original_idempotency_key: IdempotencyKey::new("source-edit.rollback.original").unwrap(),
        idempotency_key: IdempotencyKey::new("source-edit.rollback.attempt").unwrap(),
        original_input_digest: fixture_digest(),
        expected_state: fixture_digest(),
    }
}

fn assert_source_edit_authority_unavailable(response: DaemonInvocationResponse, request_id: &str) {
    assert_eq!(response.request_id, request_id);
    let DaemonInvocationOutcome::ApplicationProblem { problem } = response.outcome else {
        panic!(
            "missing source-edit authority must be a typed application problem, not {:?}",
            response.outcome
        );
    };
    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("source_edit.authority_unavailable")
    );
}

async fn admit_project_without_source_edit_owner(
    service: &DaemonInvocationService,
    project_root: PathBuf,
) {
    DaemonLspOwnerRegistrar::new(service)
        .register_factory_for_project(
            project_root,
            UserProfileId::new("profile.test.source-edit").expect("profile"),
            ProjectId::new("project.test.source-edit").expect("project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("admit project runtime");
}

#[test]
fn source_edit_invocation_round_trips_on_the_daemon_protocol() {
    let request = DaemonInvocationRequest::source_edit(
        "request.source-edit.round-trip",
        source_edit_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit.round-trip"),
    );
    assert_eq!(request.operation(), DaemonInvocationOperation::SourceEdit);
    assert!(request.validate().is_ok());
    let encoded = serde_json::to_string(&request).expect("encode");
    let decoded = parse_daemon_invocation_request(&encoded)
        .expect("daemon protocol")
        .expect("valid request");
    assert_eq!(decoded.operation(), DaemonInvocationOperation::SourceEdit);
    assert!(matches!(
        decoded.payload,
        DaemonInvocationPayload::SourceEdit { .. }
    ));
}

#[test]
fn source_edit_reconcile_invocation_round_trips_on_the_daemon_protocol() {
    let request = DaemonInvocationRequest::source_edit_reconcile(
        "request.source-edit-reconcile.round-trip",
        source_edit_reconcile_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit-reconcile.round-trip"),
    );
    assert_eq!(
        request.operation(),
        DaemonInvocationOperation::SourceEditReconcile
    );
    assert!(request.validate().is_ok());
    let encoded = serde_json::to_string(&request).expect("encode");
    let decoded = parse_daemon_invocation_request(&encoded)
        .expect("daemon protocol")
        .expect("valid request");
    assert_eq!(
        decoded.operation(),
        DaemonInvocationOperation::SourceEditReconcile
    );
    assert!(matches!(
        decoded.payload,
        DaemonInvocationPayload::SourceEditReconcile { .. }
    ));
}

#[test]
fn source_edit_rollback_invocation_round_trips_on_the_daemon_protocol() {
    let request = DaemonInvocationRequest::source_edit_rollback(
        "request.source-edit-rollback.round-trip",
        source_edit_rollback_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit-rollback.round-trip"),
    );
    assert_eq!(
        request.operation(),
        DaemonInvocationOperation::SourceEditRollback
    );
    assert!(request.validate().is_ok());
    let encoded = serde_json::to_string(&request).expect("encode");
    let decoded = parse_daemon_invocation_request(&encoded)
        .expect("daemon protocol")
        .expect("valid request");
    assert_eq!(
        decoded.operation(),
        DaemonInvocationOperation::SourceEditRollback
    );
    assert!(matches!(
        decoded.payload,
        DaemonInvocationPayload::SourceEditRollback { .. }
    ));
}

#[tokio::test]
async fn source_edit_dispatch_reaches_the_project_authority_and_refuses_when_it_is_missing() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/source-edit-missing-owner");
    admit_project_without_source_edit_owner(&service, project_root.clone()).await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let request = DaemonInvocationRequest::source_edit(
        "request.source-edit.missing-owner",
        source_edit_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit.missing-owner"),
    );

    let response = service
        .invoke(&registry, Some(&project_root), None, None, None, request)
        .await;

    assert_source_edit_authority_unavailable(response, "request.source-edit.missing-owner");
}

#[tokio::test]
async fn source_edit_reconcile_dispatch_reaches_the_project_authority_and_refuses_when_it_is_missing()
 {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/source-edit-reconcile-missing-owner");
    admit_project_without_source_edit_owner(&service, project_root.clone()).await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let request = DaemonInvocationRequest::source_edit_reconcile(
        "request.source-edit-reconcile.missing-owner",
        source_edit_reconcile_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit-reconcile.missing-owner"),
    );

    let response = service
        .invoke(&registry, Some(&project_root), None, None, None, request)
        .await;

    assert_source_edit_authority_unavailable(
        response,
        "request.source-edit-reconcile.missing-owner",
    );
}

#[tokio::test]
async fn source_edit_rollback_dispatch_reaches_the_project_authority_and_refuses_when_it_is_missing()
 {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/source-edit-rollback-missing-owner");
    admit_project_without_source_edit_owner(&service, project_root.clone()).await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let request = DaemonInvocationRequest::source_edit_rollback(
        "request.source-edit-rollback.missing-owner",
        source_edit_rollback_invocation(),
        UtcMicros(1),
        source_edit_deadline(),
        source_edit_cancellation("cancel.source-edit-rollback.missing-owner"),
    );

    let response = service
        .invoke(&registry, Some(&project_root), None, None, None, request)
        .await;

    assert_source_edit_authority_unavailable(
        response,
        "request.source-edit-rollback.missing-owner",
    );
}

#[tokio::test]
async fn source_edit_dispatch_refuses_an_expired_deadline_without_a_silent_success() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/source-edit-expired");
    admit_project_without_source_edit_owner(&service, project_root.clone()).await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let now = current_micros();
    let request = DaemonInvocationRequest::source_edit(
        "request.source-edit.expired",
        source_edit_invocation(),
        now,
        Deadline::new(now).expect("elapsed deadline"),
        source_edit_cancellation("cancel.source-edit.expired"),
    );

    let response = service
        .invoke(&registry, Some(&project_root), None, None, None, request)
        .await;

    assert!(
        !matches!(response.outcome, DaemonInvocationOutcome::SourceEdit { .. }),
        "an expired source-edit invocation must not report a completed edit"
    );
    assert!(
        matches!(
            response.outcome,
            DaemonInvocationOutcome::ApplicationProblem { .. }
                | DaemonInvocationOutcome::Problem { .. }
        ),
        "expired source-edit authority must stay a typed refusal, got {:?}",
        response.outcome
    );
}
