use std::path::Path;
use std::process::Command;
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
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
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
