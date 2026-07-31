use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use tracedecay_application::{
    ApplicationOutcome, CancellationContext, Deadline, MultiRootExecuteRequestV1,
    MultiRootOperationV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasStatusV1,
    WorkProjectionSnapshotRequestV1,
};
use tracedecay_domain::{ScopeOutcome, ScopeSetId, UtcMicros};

use super::{
    enter_test_daemon_database_scope, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::service::invocation::{
    DaemonInvocationOutcome, DaemonInvocationRequest, WorkApplicationInvocationV1,
};
use crate::daemon::{DaemonHandshake, execute_daemon_invocation};

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

fn assert_stale_digest_rejected<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    scope_set_id: &'a ScopeSetId,
    scope_set: &'a AuthorizedScopeSet,
    operation: &'a MultiRootOperationV1,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let observed_at = now();
        let (deadline, cancellation) = controls("stale-digest", observed_at);
        let stale_digest = execute_daemon_invocation(
            engine,
            handshake,
            DaemonInvocationRequest::multi_root_execute(
                "request.multi-root.stale-digest",
                MultiRootExecuteRequestV1::new(
                    scope_set_id.clone(),
                    scope_set.revision(),
                    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
                        .expect("stale digest"),
                    operation.clone(),
                    0,
                    None,
                )
                .expect("stale digest request"),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        assert!(matches!(
            stale_digest.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
            }
        ));
    })
}

fn assert_git_preflight<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    scope_set_id: &'a ScopeSetId,
    scope_set: &'a AuthorizedScopeSet,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let observed_at = now();
        let (deadline, cancellation) = controls("git-preflight", observed_at);
        let git_preflight = execute_daemon_invocation(
            engine,
            handshake,
            DaemonInvocationRequest::multi_root_execute(
                "request.multi-root.git-preflight",
                MultiRootExecuteRequestV1::new(
                    scope_set_id.clone(),
                    scope_set.revision(),
                    scope_set.digest().clone(),
                    MultiRootOperationV1::Git {
                        request: json!({
                            "operation": "git_status",
                            "request": {}
                        }),
                    },
                    0,
                    None,
                )
                .expect("Git preflight request"),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        let DaemonInvocationOutcome::MultiRootQueryPage {
            outcome: ApplicationOutcome::Evidence(git_preflight),
            ..
        } = git_preflight.outcome
        else {
            panic!("Git preflight must reach the production daemon");
        };
        let git_preflight = git_preflight.payload.expect("Git preflight page");
        assert!(matches!(git_preflight.aggregate, ScopeOutcome::Exact(_)));
        assert_eq!(git_preflight.roots.len(), 2);
        assert!(
            git_preflight
                .roots
                .iter()
                .all(|root| matches!(root.outcome, ScopeOutcome::Exact(_)))
        );
    })
}

#[cfg(unix)]
#[tokio::test]
async fn two_registered_roots_survive_cas_partial_query_and_restart() {
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
    let lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.multi-root.lsp",
            "client.multi-root",
            Some(first_uri.clone()),
            vec![second_uri, first_uri],
        ),
    )
    .await;
    assert!(matches!(
        lsp.outcome,
        DaemonInvocationOutcome::LspOpened {
            scope_set_id: Some(_),
            scope_set_digest: Some(_),
            ..
        }
    ));

    let scope_set_id = ScopeSetId::new("scope-set.daemon-journey").expect("scope set id");
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
    let DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
        outcome: ApplicationOutcome::Evidence(cas),
        ..
    } = cas.outcome
    else {
        panic!("scope-set CAS must reach the production daemon");
    };
    let applied = cas.payload.expect("CAS result");
    assert_eq!(applied.status, MultiRootScopeSetCasStatusV1::Applied);
    let scope_set = applied.scope_set.expect("applied scope set");
    assert_eq!(scope_set.roots().len(), 2);

    let work_operation = MultiRootOperationV1::Work {
        request: serde_json::to_value(WorkApplicationInvocationV1::Snapshot(
            WorkProjectionSnapshotRequestV1 { page_size: 100 },
        ))
        .expect("snapshot request"),
    };
    assert_stale_digest_rejected(
        &engine,
        &first_handshake,
        &scope_set_id,
        &scope_set,
        &work_operation,
    )
    .await;
    assert_git_preflight(&engine, &first_handshake, &scope_set_id, &scope_set).await;

    let observed_at = now();
    let (deadline, cancellation) = controls("stale", observed_at);
    let stale = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.stale",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![first_project, second_project],
            )
            .expect("stale request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
        outcome: ApplicationOutcome::Evidence(stale),
        ..
    } = stale.outcome
    else {
        panic!("stale CAS must return a typed result");
    };
    assert_eq!(
        stale.payload.expect("stale result").status,
        MultiRootScopeSetCasStatusV1::Conflict
    );

    let missing_root = second.path().with_extension("unavailable");
    std::fs::rename(second.path(), &missing_root).expect("withdraw second root");
    let operation = MultiRootOperationV1::Work {
        request: serde_json::to_value(WorkApplicationInvocationV1::Snapshot(
            WorkProjectionSnapshotRequestV1 { page_size: 100 },
        ))
        .expect("snapshot request"),
    };
    let observed_at = now();
    let (deadline, cancellation) = controls("query", observed_at);
    let query = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.query",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                operation.clone(),
                0,
                None,
            )
            .expect("query request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome: ApplicationOutcome::Evidence(query),
        ..
    } = query.outcome
    else {
        panic!("query must reach the production daemon");
    };
    let first_page = query.payload.expect("query page");
    assert!(
        matches!(first_page.aggregate, ScopeOutcome::Partial { .. }),
        "expected one exact and one unavailable root: {first_page:#?}"
    );
    assert!(first_page.roots.iter().any(|root| {
        matches!(
            root.outcome,
            ScopeOutcome::Unavailable {
                reason: tracedecay_domain::ScopeUnavailableReasonV1::RootMissing
            }
        )
    }));

    engine.shutdown_all().await;
    let restarted = test_daemon_engine_for_profile(&profile_root);
    restarted
        .open_project_server(&first_handshake)
        .await
        .expect("restart first owner");
    let observed_at = now();
    let (deadline, cancellation) = controls("resume", observed_at);
    let resumed = execute_daemon_invocation(
        &restarted,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.resume",
            MultiRootExecuteRequestV1::new(
                scope_set_id,
                scope_set.revision(),
                scope_set.digest().clone(),
                operation,
                1,
                Some(first_page.continuation),
            )
            .expect("resume request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        resumed.outcome,
        DaemonInvocationOutcome::MultiRootQueryPage { .. }
    ));
    restarted.shutdown_all().await;
    let _ = std::fs::rename(missing_root, second.path());
}
