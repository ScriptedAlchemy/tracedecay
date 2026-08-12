#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::{
    ApplicationProblemKind, CancellationContext, Deadline, MultiRootExecuteRequestV1,
    MultiRootOperationV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasStatusV1,
    MultiRootScopeSetReadRequestV1, RegisteredRootSelectorV1,
};
use tracedecay_domain::{ScopeSetId, UtcMicros};

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
#[test]
fn authenticated_multi_root_journey_reaches_scope_set_storage() {
    const STACK_SIZE: usize = 16 * 1024 * 1024;

    std::thread::Builder::new()
        .name("multi-root-journey".to_owned())
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .expect("multi-root journey runtime")
                .block_on(run_authenticated_multi_root_journey());
        })
        .expect("multi-root journey thread")
        .join()
        .expect("multi-root journey thread must not panic");
}

#[test]
fn multi_root_direct_routes_refuse_a_quiesced_project() {
    const STACK_SIZE: usize = 16 * 1024 * 1024;

    std::thread::Builder::new()
        .name("multi-root-quiescence".to_owned())
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .expect("multi-root quiescence runtime")
                .block_on(run_multi_root_quiescence());
        })
        .expect("multi-root quiescence thread")
        .join()
        .expect("multi-root quiescence thread must not panic");
}

async fn run_multi_root_quiescence() {
    let home = TempDir::new().expect("home");
    let profile_root = home.path().join("profile");
    let repository = repository();
    let handshake = DaemonHandshake {
        project_path: Some(repository.path().to_path_buf()),
        allow_init: true,
        client_identity: test_client_identity_for(profile_root.clone()),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "multi-root-quiescence");
    let (server_key, _, _, _) = engine
        .open_project_server(&handshake)
        .await
        .expect("project owner");
    let project_id = tracedecay_domain::ProjectId::new(
        server_key
            .owner
            .project_id
            .clone()
            .expect("project identity"),
    )
    .expect("valid project identity");
    let profile_id = engine
        .store_administration
        .profile_identity()
        .expect("profile identity")
        .profile_id()
        .clone();
    let project_root = server_key.project_root;
    let cancellation_scope_set =
        ScopeSetId::new("scope-set.direct-cancellation").expect("scope set id");
    let cancellation_observed_at = now();
    let (active_deadline, active_cancellation) =
        controls("direct-cancellation", cancellation_observed_at);
    let cancelled_read_id = "request.multi-root.cancelled-read";
    let cancelled_cas_id = "request.multi-root.cancelled-cas";
    let cancelled_execute_id = "request.multi-root.cancelled-execute";
    assert!(!crate::daemon::request_cancellation::cancel(
        cancelled_read_id
    ));
    assert!(!crate::daemon::request_cancellation::cancel(
        cancelled_cas_id
    ));
    assert!(!crate::daemon::request_cancellation::cancel(
        cancelled_execute_id
    ));
    let interrupted = [
        (
            DaemonInvocationRequest::multi_root_scope_set_read(
                cancelled_read_id,
                MultiRootScopeSetReadRequestV1::new(cancellation_scope_set.clone())
                    .expect("read request"),
                cancellation_observed_at,
                active_deadline.clone(),
                active_cancellation.clone(),
            ),
            ApplicationProblemKind::Cancelled,
        ),
        (
            DaemonInvocationRequest::multi_root_scope_set_read(
                "request.multi-root.expired-read",
                MultiRootScopeSetReadRequestV1::new(cancellation_scope_set.clone())
                    .expect("read request"),
                cancellation_observed_at,
                Deadline::new(cancellation_observed_at).expect("expired deadline"),
                active_cancellation.clone(),
            ),
            ApplicationProblemKind::TimedOut,
        ),
        (
            DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                cancelled_cas_id,
                MultiRootScopeSetCasRequestV1::new(
                    cancellation_scope_set.clone(),
                    None,
                    vec![
                        RegisteredRootSelectorV1::new(project_id.clone(), project_root.clone())
                            .expect("registered root selector"),
                    ],
                )
                .expect("CAS request"),
                cancellation_observed_at,
                active_deadline.clone(),
                active_cancellation.clone(),
            ),
            ApplicationProblemKind::Cancelled,
        ),
        (
            DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                "request.multi-root.expired-cas",
                MultiRootScopeSetCasRequestV1::new(
                    cancellation_scope_set.clone(),
                    None,
                    vec![
                        RegisteredRootSelectorV1::new(project_id.clone(), project_root.clone())
                            .expect("registered root selector"),
                    ],
                )
                .expect("CAS request"),
                cancellation_observed_at,
                Deadline::new(cancellation_observed_at).expect("expired deadline"),
                active_cancellation.clone(),
            ),
            ApplicationProblemKind::TimedOut,
        ),
        (
            DaemonInvocationRequest::multi_root_execute(
                cancelled_execute_id,
                MultiRootExecuteRequestV1::new(
                    cancellation_scope_set.clone(),
                    tracedecay_domain::ScopeSetRevision::new(1).expect("scope revision"),
                    tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                        .expect("scope digest"),
                    MultiRootOperationV1::Query { request: json!({}) },
                    0,
                    None,
                )
                .expect("execute request"),
                cancellation_observed_at,
                active_deadline,
                active_cancellation,
            ),
            ApplicationProblemKind::Cancelled,
        ),
        (
            DaemonInvocationRequest::multi_root_execute(
                "request.multi-root.expired-execute",
                MultiRootExecuteRequestV1::new(
                    cancellation_scope_set,
                    tracedecay_domain::ScopeSetRevision::new(1).expect("scope revision"),
                    tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                        .expect("scope digest"),
                    MultiRootOperationV1::Query { request: json!({}) },
                    0,
                    None,
                )
                .expect("execute request"),
                cancellation_observed_at,
                Deadline::new(cancellation_observed_at).expect("expired deadline"),
                CancellationContext::active("cancel.multi-root.expired-execute")
                    .expect("cancellation"),
            ),
            ApplicationProblemKind::TimedOut,
        ),
    ];
    for (request, expected) in interrupted {
        let response = execute_daemon_invocation(&engine, &handshake, request).await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::ApplicationProblem { problem }
                if problem.kind() == expected
        ));
    }

    let quiescence = engine
        .invocation
        .service
        .quiesce_project(
            &engine.invocation.lsp_session_registry,
            &profile_id,
            &project_id,
            &std::collections::BTreeSet::from([project_root.clone()]),
        )
        .await
        .expect("project quiescence");
    let scope_set_id = ScopeSetId::new("scope-set.quiesced").expect("scope set id");
    let observed_at = now();
    let (read_deadline, read_cancellation) = controls("quiesced-read", observed_at);
    let (cas_deadline, cas_cancellation) = controls("quiesced-cas", observed_at);
    let (execute_deadline, execute_cancellation) = controls("quiesced-execute", observed_at);
    let requests = [
        DaemonInvocationRequest::multi_root_scope_set_read(
            "request.multi-root.quiesced-read",
            MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
            observed_at,
            read_deadline,
            read_cancellation,
        ),
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.quiesced-cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![
                    RegisteredRootSelectorV1::new(project_id, project_root.clone())
                        .expect("registered root selector"),
                ],
            )
            .expect("CAS request"),
            observed_at,
            cas_deadline,
            cas_cancellation,
        ),
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.quiesced-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id,
                tracedecay_domain::ScopeSetRevision::new(1).expect("scope revision"),
                tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("scope digest"),
                MultiRootOperationV1::Query { request: json!({}) },
                0,
                None,
            )
            .expect("execute request"),
            observed_at,
            execute_deadline,
            execute_cancellation,
        ),
    ];

    for request in requests {
        let response = execute_daemon_invocation(&engine, &handshake, request).await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
    }

    drop(quiescence);
}

#[cfg(unix)]
async fn run_authenticated_multi_root_journey() {
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

    // A malformed multi-root payload is still rejected on its own terms, and
    // still before the daemon spends a project admission on it.
    let observed_at = now();
    let (deadline, cancellation) = controls("invalid-read", observed_at);
    let mut invalid_read = DaemonInvocationRequest::multi_root_scope_set_read(
        "request.multi-root.invalid-read",
        MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
        observed_at,
        deadline,
        cancellation,
    );
    let DaemonInvocationPayload::MultiRootScopeSetRead {
        observed_at: invalid_observed_at,
        ..
    } = &mut invalid_read.payload
    else {
        unreachable!("constructed read payload")
    };
    *invalid_observed_at = UtcMicros(0);
    let invalid_response =
        execute_daemon_invocation(&engine, &first_handshake, wire_round_trip(&invalid_read)).await;
    assert!(matches!(
        invalid_response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::InvalidRequest
        }
    ));
    let portable_invalid = execute_portable_daemon_invocation(
        engine.lifecycle.clone(),
        engine.store_administration.clone(),
        Arc::clone(&engine.project_open_gates),
        &first_handshake,
        &engine.invocation,
        engine.http_application_registry.clone(),
        wire_round_trip(&invalid_read),
        Some(Arc::clone(&engine.project_open_attempts)),
    )
    .await;
    assert!(matches!(
        portable_invalid.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::InvalidRequest
        }
    ));
    assert_eq!(
        engine.project_open_attempts.load(Ordering::Relaxed),
        0,
        "an invalid multi-root payload must be rejected before project admission"
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

    // A single folder that is not the active project is still refused: a lone
    // sibling hint must not reroute the session.
    let (deadline, cancellation) = controls("sibling-root-lsp", now());
    let sibling_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.sibling-root.lsp",
            "client.sibling-root",
            Some(second_uri.clone()),
            vec![second_uri.clone()],
            deadline,
            cancellation,
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

    let (deadline, cancellation) = controls("single-root-lsp", now());
    let single_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.single-root.lsp",
            "client.single-root",
            Some(first_uri.clone()),
            vec![first_uri.clone()],
            deadline,
            cancellation,
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

    // A multi-folder initialize now admits a federated workspace and reports
    // the authorized scope set it was bound to.
    let (deadline, cancellation) = controls("multi-root-lsp", now());
    let lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.multi-root.lsp",
            "client.multi-root",
            Some(first_uri.clone()),
            vec![second_uri.clone(), first_uri.clone()],
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::LspOpened {
        scope_set_id: lsp_scope_set_id,
        scope_set_digest: lsp_scope_set_digest,
        ..
    } = &lsp.outcome
    else {
        panic!(
            "multi-folder initialize must open a session: {:?}",
            lsp.outcome
        );
    };
    let lsp_scope_set_id = lsp_scope_set_id
        .clone()
        .expect("federated initialize must report its scope set id");
    assert!(
        lsp_scope_set_digest.is_some(),
        "federated initialize must report its scope set digest"
    );
    for root in [first.path(), second.path()] {
        assert!(
            engine
                .invocation
                .service
                .persisted_scope_set(root, &lsp_scope_set_id)
                .await
                .is_none(),
            "federated LSP admission must retain its frozen scope only in daemon session state"
        );
    }

    // Compare-and-swap is the authorization boundary for an explicit scope set.
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
                vec![
                    RegisteredRootSelectorV1::new(
                        second_project.clone(),
                        second.path().canonicalize().expect("canonical second root"),
                    )
                    .expect("second registered root selector"),
                    RegisteredRootSelectorV1::new(
                        first_project.clone(),
                        first.path().canonicalize().expect("canonical first root"),
                    )
                    .expect("first registered root selector"),
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
        panic!("multi-root CAS must reach the executor: {:?}", cas.outcome);
    };
    let tracedecay_application::ApplicationOutcome::Evidence(packet) = outcome else {
        panic!("multi-root CAS must return evidence");
    };
    let cas_result = packet
        .payload
        .clone()
        .expect("multi-root CAS evidence must carry a result");
    assert!(matches!(
        cas_result.status,
        MultiRootScopeSetCasStatusV1::Applied
    ));
    let stored = cas_result
        .scope_set
        .expect("applied CAS must return the scope set");
    for root in [first.path(), second.path()] {
        assert_eq!(
            engine
                .invocation
                .service
                .persisted_scope_set(root, &scope_set_id)
                .await
                .as_ref(),
            Some(&stored),
            "an applied CAS must be durable in every participating store"
        );
    }

    // The read surface returns exactly what the CAS persisted.
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
    let DaemonInvocationOutcome::MultiRootScopeSetRead { outcome, .. } = &read.outcome else {
        panic!(
            "multi-root read must reach the executor: {:?}",
            read.outcome
        );
    };
    let tracedecay_application::ApplicationOutcome::Evidence(packet) = outcome else {
        panic!("multi-root read must return evidence");
    };
    assert_eq!(packet.payload.clone().flatten().as_ref(), Some(&stored));

    // A stale revision or digest is refused by the executor, not by a gate.
    let observed_at = now();
    let (deadline, cancellation) = controls("stale-execute", observed_at);
    let stale = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.stale-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("digest"),
                MultiRootOperationV1::Query { request: json!({}) },
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
        stale.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));

    // Every operation family fans out over the authorized scope set.
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
        let observed_at = now();
        let (deadline, cancellation) = controls(&format!("execute-{index}"), observed_at);
        let response = execute_daemon_invocation(
            &engine,
            &first_handshake,
            DaemonInvocationRequest::multi_root_execute(
                format!("request.multi-root.execute-{index}"),
                MultiRootExecuteRequestV1::new(
                    scope_set_id.clone(),
                    stored.revision(),
                    stored.digest().clone(),
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
        assert!(
            matches!(
                response.outcome,
                DaemonInvocationOutcome::MultiRootQueryPage { .. }
            ),
            "execute-{index} must reach the multi-root executor: {:?}",
            response.outcome
        );
    }

    // The portable route must authorize the active daemon scope before it
    // resolves or mounts any selected root, then reach the same executor.
    let observed_at = now();
    let (deadline, cancellation) = controls("portable-execute", observed_at);
    let portable_execute = execute_portable_daemon_invocation(
        engine.lifecycle.clone(),
        engine.store_administration.clone(),
        Arc::clone(&engine.project_open_gates),
        &first_handshake,
        &engine.invocation,
        engine.http_application_registry.clone(),
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.portable-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                stored.digest().clone(),
                MultiRootOperationV1::Query { request: json!({}) },
                0,
                None,
            )
            .expect("portable execute request"),
            observed_at,
            deadline,
            cancellation,
        ),
        Some(Arc::clone(&engine.project_open_attempts)),
    )
    .await;
    assert!(
        matches!(
            portable_execute.outcome,
            DaemonInvocationOutcome::MultiRootQueryPage { .. }
        ),
        "the portable route must reach the multi-root executor: {:?}",
        portable_execute.outcome
    );

    engine.shutdown_all().await;
}
