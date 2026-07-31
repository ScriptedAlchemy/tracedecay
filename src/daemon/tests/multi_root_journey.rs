use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, Deadline, MultiRootExecuteRequestV1, MultiRootOperationV1,
    MultiRootScopeSetCasRequestV1, MultiRootScopeSetReadRequestV1,
};
use tracedecay_domain::{ManifestDigest, ScopeSetId, ScopeSetRevision, UtcMicros};

use super::{
    enter_test_daemon_database_scope, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::service::invocation::{
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, parse_daemon_invocation_request,
};
use crate::daemon::{
    DaemonHandshake, execute_daemon_invocation, execute_portable_daemon_invocation,
};

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("run Git fixture command");
    assert!(status.success(), "git {args:?}");
}

fn repository() -> TempDir {
    let repository = TempDir::new().expect("repository");
    git(repository.path(), &["init", "--quiet"]);
    git(
        repository.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    git(
        repository.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    std::fs::write(
        repository.path().join("lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "base"]);
    repository
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn controls(suffix: &str, observed_at: UtcMicros) -> (Deadline, CancellationContext) {
    (
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline"),
        CancellationContext::active(format!("cancel.multi-root.{suffix}")).expect("cancellation"),
    )
}

fn wire_round_trip(request: &DaemonInvocationRequest) -> DaemonInvocationRequest {
    let wire = serde_json::to_string(request).expect("daemon invocation wire");
    parse_daemon_invocation_request(&wire)
        .expect("daemon invocation protocol")
        .expect("valid daemon invocation envelope")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_multi_root_cas_is_quarantined_before_storage() {
    let home = TempDir::new().expect("home");
    let profile_root = home.path().join("profile");
    let first = repository();
    let second = repository();
    let first_handshake = DaemonHandshake {
        project_path: Some(first.path().to_path_buf()),
        allow_init: true,
        client_identity: test_client_identity_for(profile_root.clone()),
        ..test_handshake_defaults()
    };
    let second_handshake = DaemonHandshake {
        project_path: Some(second.path().to_path_buf()),
        allow_init: true,
        client_identity: first_handshake.client_identity.clone(),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "multi-root-journey");
    let scope_set_id = ScopeSetId::new("scope-set.daemon-journey").expect("scope set id");
    let observed_at = now();
    let (deadline, cancellation) = controls("pre-admission-read", observed_at);
    let read_request = DaemonInvocationRequest::multi_root_scope_set_read(
        "request.multi-root.pre-admission-read",
        MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
        observed_at,
        deadline,
        cancellation,
    );
    let mut pre_admission_requests = vec![wire_round_trip(&read_request)];
    pre_admission_requests.push(DaemonInvocationRequest::lsp_open(
        "request.multi-root.pre-admission-lsp",
        "client.multi-root",
        None,
        vec![
            url::Url::from_file_path(first.path())
                .expect("first URI")
                .to_string(),
            url::Url::from_file_path(second.path())
                .expect("second URI")
                .to_string(),
        ],
    ));
    let (cas_deadline, cas_cancellation) = controls("pre-admission-cas", observed_at);
    pre_admission_requests.push(
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.pre-admission-cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![tracedecay_domain::ProjectId::new("project.pre-admission").expect("project")],
            )
            .expect("CAS request"),
            observed_at,
            cas_deadline,
            cas_cancellation,
        ),
    );
    let pre_admission_revision = ScopeSetRevision::new(1).expect("revision");
    let pre_admission_digest =
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest");
    for (index, operation) in [
        MultiRootOperationV1::Work { request: json!({}) },
        MultiRootOperationV1::Git { request: json!({}) },
        MultiRootOperationV1::Feedback { request: json!({}) },
        MultiRootOperationV1::Impact { request: json!({}) },
        MultiRootOperationV1::Query { request: json!({}) },
    ]
    .into_iter()
    .enumerate()
    {
        let (deadline, cancellation) =
            controls(&format!("pre-admission-execute-{index}"), observed_at);
        pre_admission_requests.push(DaemonInvocationRequest::multi_root_execute(
            format!("request.multi-root.pre-admission-execute-{index}"),
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                pre_admission_revision.clone(),
                pre_admission_digest.clone(),
                operation,
                0,
                None,
            )
            .expect("execute request"),
            observed_at,
            deadline,
            cancellation,
        ));
    }
    for request in &pre_admission_requests {
        let unix =
            execute_daemon_invocation(&engine, &first_handshake, wire_round_trip(request)).await;
        assert!(matches!(
            unix.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
        let portable = execute_portable_daemon_invocation(
            engine.lifecycle.clone(),
            engine.store_administration.clone(),
            Arc::clone(&engine.project_open_gates),
            &first_handshake,
            &engine.invocation,
            engine.http_application_registry.clone(),
            wire_round_trip(request),
            Some(Arc::clone(&engine.project_open_attempts)),
        )
        .await;
        assert!(matches!(
            portable.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
    }
    assert_eq!(
        engine.project_open_attempts.load(Ordering::Relaxed),
        0,
        "all quarantined payloads must refuse before Unix or portable project admission"
    );

    let mut invalid_read = read_request;
    let DaemonInvocationPayload::MultiRootScopeSetRead { observed_at, .. } =
        &mut invalid_read.payload
    else {
        unreachable!("constructed read payload")
    };
    *observed_at = UtcMicros(0);
    let invalid_response =
        execute_daemon_invocation(&engine, &first_handshake, wire_round_trip(&invalid_read)).await;
    assert!(matches!(
        invalid_response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::InvalidRequest
        }
    ));
    assert_eq!(
        engine.project_open_attempts.load(Ordering::Relaxed),
        0,
        "invalid quarantined payload must be validated before project admission"
    );
    let (first_key, _, _, _) = engine
        .open_project_server(&first_handshake)
        .await
        .expect("first owner");
    let (second_key, _, _, _) = engine
        .open_project_server(&second_handshake)
        .await
        .expect("second owner");
    let first_project = tracedecay_domain::ProjectId::new(
        first_key
            .owner
            .project_id
            .clone()
            .expect("first project id"),
    )
    .expect("first project");
    let second_project = tracedecay_domain::ProjectId::new(
        second_key
            .owner
            .project_id
            .clone()
            .expect("second project id"),
    )
    .expect("second project");
    let first_uri = url::Url::from_file_path(first.path())
        .expect("first URI")
        .to_string();
    let second_uri = url::Url::from_file_path(second.path())
        .expect("second URI")
        .to_string();
    let mut lsp_scopes = vec![
        crate::daemon::project_open_owners::resolved_scope_for_project(
            first.path(),
            &first_project,
        )
        .expect("first scope"),
        crate::daemon::project_open_owners::resolved_scope_for_project(
            second.path(),
            &second_project,
        )
        .expect("second scope"),
    ];
    lsp_scopes.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
    let selector_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.lsp-workspace-selector.v1",
        lsp_scopes
            .iter()
            .map(|scope| &scope.scope_digest)
            .collect::<Vec<_>>(),
    ))
    .expect("selector digest");
    let quarantined_scope_set_id = ScopeSetId::new(format!(
        "scope-set.lsp.{}",
        selector_digest.as_str().trim_start_matches("sha256:")
    ))
    .expect("scope set id");
    let lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.multi-root.lsp",
            "client.multi-root",
            Some(first_uri.clone()),
            vec![second_uri.clone(), first_uri.clone()],
        ),
    )
    .await;
    assert!(matches!(
        lsp.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    assert_eq!(
        engine
            .invocation
            .lsp_session_registry
            .lock()
            .await
            .active_sessions(),
        0,
        "quarantined multi-root initialize must not mount an LSP runtime"
    );
    assert_eq!(
        engine.invocation.service.active_lsp_runtime_count().await,
        0,
        "quarantined multi-root initialize must not create a runtime actor"
    );
    assert!(
        engine
            .invocation
            .service
            .persisted_scope_set(first.path(), &quarantined_scope_set_id)
            .await
            .is_none(),
        "quarantined initialize must not persist the scope set in the first store"
    );
    assert!(
        engine
            .invocation
            .service
            .persisted_scope_set(second.path(), &quarantined_scope_set_id)
            .await
            .is_none(),
        "quarantined initialize must not persist the scope set in the second store"
    );
    let sibling_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.sibling-root.lsp",
            "client.sibling-root",
            Some(second_uri.clone()),
            vec![second_uri.clone()],
        ),
    )
    .await;
    assert!(matches!(
        sibling_root_lsp.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    assert_eq!(
        engine.invocation.service.active_lsp_runtime_count().await,
        0,
        "a sibling single-folder hint must not mount a runtime"
    );
    let single_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.single-root.lsp",
            "client.single-root",
            Some(first_uri.clone()),
            vec![first_uri.clone()],
        ),
    )
    .await;
    assert!(matches!(
        single_root_lsp.outcome,
        DaemonInvocationOutcome::LspOpened {
            scope_set_id: None,
            scope_set_digest: None,
            ..
        }
    ));
    assert_eq!(
        engine
            .invocation
            .lsp_session_registry
            .lock()
            .await
            .active_sessions(),
        1,
        "single-root initialize must keep the existing runtime path working"
    );

    let observed_at = now();
    let (deadline, cancellation) = controls("cas", observed_at);
    let cas = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![second_project.clone(), first_project.clone()],
            )
            .expect("CAS request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        cas.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    assert!(
        engine
            .invocation
            .service
            .persisted_scope_set(first.path(), &scope_set_id)
            .await
            .is_none(),
        "quarantined CAS must not write scope-set storage"
    );

    let observed_at = now();
    let (deadline, cancellation) = controls("read", observed_at);
    let read = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_scope_set_read(
            "request.multi-root.read",
            MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        read.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));

    let digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest");
    let revision = ScopeSetRevision::new(1).expect("revision");
    let operations = [
        MultiRootOperationV1::Work { request: json!({}) },
        MultiRootOperationV1::Git { request: json!({}) },
        MultiRootOperationV1::Feedback { request: json!({}) },
        MultiRootOperationV1::Impact { request: json!({}) },
        MultiRootOperationV1::Query { request: json!({}) },
    ];
    for (index, operation) in operations.into_iter().enumerate() {
        let observed_at = now();
        let (deadline, cancellation) = controls(&format!("execute-{index}"), observed_at);
        let response = execute_daemon_invocation(
            &engine,
            &first_handshake,
            DaemonInvocationRequest::multi_root_execute(
                format!("request.multi-root.execute-{index}"),
                MultiRootExecuteRequestV1::new(
                    scope_set_id.clone(),
                    revision.clone(),
                    digest.clone(),
                    operation,
                    0,
                    None,
                )
                .expect("execute request"),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
    }
    assert!(
        engine
            .invocation
            .service
            .persisted_scope_set(first.path(), &scope_set_id)
            .await
            .is_none(),
        "quarantined read and fan-out must not create scope-set storage"
    );

    engine.shutdown_all().await;
}
