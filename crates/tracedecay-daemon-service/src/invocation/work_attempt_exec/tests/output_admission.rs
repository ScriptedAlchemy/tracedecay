//! Output admission against a real published-shape store and its convergence.
use super::*;
use tracedecay_contracts::{
    RuntimeWorkRetryEvidenceV1, WorkRetryCauseV1, WorkRetryEvidencePortV1,
    WorkRetryFailureSelectorV1, WorkRetrySourceV1,
};
use tracedecay_global_db::{RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::db::engine::{Executor, TestConnection};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
    enter_daemon_database_scope,
};

async fn recovering_store() -> (
    tempfile::TempDir,
    RegisteredGlobalDbLeaseV1,
    tracedecay_global_db::schema_stages::RegisteredSchemaConvergence,
    impl Sized,
) {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let path = tracedecay_sessions::runtime::user_sessions_db_path(&profile);
    drop(
        tracedecay_global_db::tests::harness::open_registered_test_database_fixture(
            &path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap(),
    );
    TestConnection::open(&path)
        .execute_batch(
            "DROP TABLE workflow_artifact_payloads;
             CREATE TABLE workflow_artifact_payloads (
    payload_digest TEXT NOT NULL PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (
        byte_length = length(payload) AND byte_length <= 4194304
    ),
    payload BLOB NOT NULL
) STRICT;
             UPDATE workflow_schema SET definition_digest =
             'sha256:a292df6bc47e763f0d20bdb44a4032c0b1d7ac8e4cb83173b66ae7d1ff0d03be';",
        )
        .await
        .unwrap();
    let scope = enter_daemon_database_scope(&profile, 1, "output admission").unwrap();
    let authority = DatabaseAuthority::for_owned_runtime(&path, "output admission").unwrap();
    let (database, runtime, retirement) =
        Database::publish_registered_daemon_test_runtime_with_retirement_control(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Existing,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap()
        .into_parts();
    let (owner, convergence) =
        RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(database, &CancellationToken::new())
            .await
            .unwrap();
    let lease = owner.issue_lease().unwrap();
    assert!(
        lease
            .workflow_storage()
            .await
            .err()
            .unwrap()
            .is_workflow_authority_recovering()
    );
    (
        directory,
        lease,
        convergence.unwrap(),
        (owner, runtime, retirement, scope),
    )
}

async fn execute(
    fixture: &Fixture,
    executable: PathBuf,
    store: Option<&tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority>,
    protocol: WorkProviderProtocol,
) {
    let backend = if protocol == WorkProviderProtocol::CodexAppServerJsonRpc {
        WorkProviderBackendV1::CodexAppServer
    } else {
        WorkProviderBackendV1::ClaudeCodeCli
    };
    let selection = preferred(executable, protocol, &[], requested_route(backend));
    let environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    if protocol == WorkProviderProtocol::CodexAppServerJsonRpc {
        execute_app_server(
            &fixture.steps(),
            store,
            &fixture.context,
            &fixture.attempt,
            &selection,
            &environment,
            Arc::new(Notify::new()),
            None,
            None,
            AttemptAdmissionTimingV1::for_test(),
        )
        .await;
    } else {
        execute_provider_with_environment(
            &fixture.steps(),
            store,
            &fixture.context,
            &fixture.attempt,
            &selection,
            &environment,
            Arc::new(Notify::new()),
            None,
            None,
            AttemptAdmissionTimingV1::for_test(),
        )
        .await;
    }
}

#[test]
fn recovery_completed_before_admission_retains_output_in_the_same_mount() {
    without_tokio(async {
        let (_directory, lease, convergence, _held) = recovering_store().await;
        let (_other_directory, other_store) = provider_artifact_store().await;
        let root = tempfile::tempdir().unwrap();
        let fixture = leased_attempt(
            root.path(),
            "Retain every output byte.",
            &SnapshotShape::default(),
        );
        // The attempt already exists while recovery is pending. The output
        // authority must be bound after convergence rather than remembering
        // the earlier recovering result.
        lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();
        let store = bind_attempt_output(
            &lease,
            &fixture.steps(),
            &fixture.context,
            &fixture.attempt,
            None,
            None,
        )
        .await
        .unwrap();
        let executable = fake_executable(
            root.path(),
            "provider",
            "#!/bin/sh\ncat >/dev/null\nprintf 'retained-output'\n",
        );
        execute(
            &fixture,
            executable,
            Some(&store),
            WorkProviderProtocol::ClaudeStreamJson,
        )
        .await;
        assert_eq!(fixture.state(), WorkAttemptStateV1::Succeeded);
        let settled = fixture.current_attempt();
        let artifact = settled.artifacts().first().unwrap().clone();
        let queried = artifact.clone();
        let payload = store
            .transact(move |storage| WorkflowArtifactStorePort::load(&storage, &queried))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(payload.bytes(), b"retained-output");
        assert_eq!(
            other_store
                .transact(move |storage| WorkflowArtifactStorePort::load(&storage, &artifact))
                .await
                .unwrap(),
            Err(tracedecay_contracts::WorkflowArtifactStoreError::Missing)
        );
    });
}

#[test]
fn canonical_binding_fences_recovering_work_without_terminal_failure() {
    without_tokio(async {
        let (_directory, lease, _convergence, _held) = recovering_store().await;
        let root = tempfile::tempdir().unwrap();
        let fixture = leased_attempt(
            root.path(),
            "Admission while recovering.",
            &SnapshotShape::default(),
        );
        assert!(
            bind_attempt_output(
                &lease,
                &fixture.steps(),
                &fixture.context,
                &fixture.attempt,
                None,
                None,
            )
            .await
            .is_none()
        );
        let recovery = fixture.current_attempt();
        assert_eq!(recovery.state(), WorkAttemptStateV1::RecoveryRequired);
        assert!(recovery.terminal().is_none());
        assert!(recovery.artifacts().is_empty());
        assert_eq!(
            fixture
                .effects
                .load(&fixture.context, fixture.identity())
                .unwrap()
                .unwrap()
                .resolution(),
            Some(WorkAttemptEffectResolutionV1::NoEffect)
        );
    });
}

#[test]
fn still_recovering_admission_executes_no_effect_and_leaves_retryable_work() {
    without_tokio(async {
        let (_directory, lease, _convergence, _held) = recovering_store().await;
        for protocol in [
            WorkProviderProtocol::ClaudeStreamJson,
            WorkProviderProtocol::CodexAppServerJsonRpc,
        ] {
            let root = tempfile::tempdir().unwrap();
            let marker = root.path().join("effect");
            let executable = fake_executable(
                root.path(),
                "provider",
                &format!("#!/bin/sh\ntouch '{}'\nprintf output\n", marker.display()),
            );
            let shape = SnapshotShape {
                backend: if protocol == WorkProviderProtocol::CodexAppServerJsonRpc {
                    WorkProviderBackendV1::CodexAppServer
                } else {
                    WorkProviderBackendV1::ClaudeCodeCli
                },
                ..SnapshotShape::default()
            };
            let fixture = leased_attempt_with_effect(
                root.path(),
                "Do not lose output.",
                &shape,
                WorkEffectStateV1::CompoundNonRepeatable,
            );
            let error = lease.workflow_storage().await.err().unwrap();
            assert!(error.is_workflow_authority_recovering());
            execute(&fixture, executable, None, protocol).await;
            assert!(!marker.exists());
            assert_eq!(fixture.state(), WorkAttemptStateV1::RecoveryRequired);
            assert!(fixture.current_attempt().artifacts().is_empty());
            assert_eq!(
                fixture
                    .effects
                    .load(&fixture.context, fixture.identity())
                    .unwrap()
                    .unwrap()
                    .resolution(),
                Some(WorkAttemptEffectResolutionV1::NoEffect)
            );
            assert_eq!(
                fixture.rows.observed_states(),
                vec![
                    WorkAttemptStateV1::Leased,
                    WorkAttemptStateV1::RecoveryRequired
                ]
            );
            let recovery = fixture.current_attempt();
            let failure = RuntimeWorkRetryEvidenceV1
                .resolve_failure(
                    &fixture.authority,
                    &recovery,
                    &WorkRetryFailureSelectorV1 {
                        source: WorkRetrySourceV1::Runtime,
                        cause: WorkRetryCauseV1::RestartRecoveryRequired,
                        evidence_ref: "recovery-required".to_owned(),
                    },
                )
                .unwrap();
            assert_eq!(
                failure.observed_at,
                match recovery.recovery() {
                    WorkRecoveryStateV1::RecoveryRequired { observed_at, .. } => *observed_at,
                    _ => panic!("expected recoverable output admission"),
                }
            );
        }
    });
}

#[test]
fn cancellation_wins_output_admission_during_recovery_without_launching() {
    without_tokio(async {
        let (_directory, lease, _convergence, _held) = recovering_store().await;
        for protocol in [
            WorkProviderProtocol::ClaudeStreamJson,
            WorkProviderProtocol::CodexAppServerJsonRpc,
        ] {
            let root = tempfile::tempdir().unwrap();
            let marker = root.path().join("effect");
            let executable = fake_executable(
                root.path(),
                "provider",
                &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            );
            let fixture = leased_attempt(
                root.path(),
                "Cancel before output admission.",
                &SnapshotShape::default(),
            );
            let identity = fixture.identity();
            fixture
                .attempts
                .request_cancellation(
                    &fixture.context,
                    CancelWorkAttemptCommand {
                        task_id: identity.task_id().clone(),
                        run_id: identity.run_id().clone(),
                        attempt_id: identity.attempt_id().clone(),
                        request_id: id("cancel.output-admission"),
                        occurred_at: now_micros(),
                    },
                )
                .unwrap();
            assert!(lease.workflow_recovery_pending());
            execute(&fixture, executable, None, protocol).await;
            assert!(!marker.exists());
            assert_eq!(fixture.state(), WorkAttemptStateV1::Cancelled);
            assert!(
                fixture
                    .effects
                    .load(&fixture.context, identity)
                    .unwrap()
                    .is_none()
            );
        }
    });
}
