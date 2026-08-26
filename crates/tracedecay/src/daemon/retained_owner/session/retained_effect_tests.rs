use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceOperation, RetainedSurfaceRequestV1, SessionRefreshActionRequestV1,
    SessionRefreshActionV1, SessionRefreshFrontierV1, SessionRefreshGrainV1,
    SessionRefreshProjectV1, SessionRefreshRequestV1, SessionRefreshSessionV1,
    SessionRefreshSourceV1, SessionRefreshTargetV1, SessionRefreshTemporalModeV1,
};
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemKind, CancellationContext, CancellationSignal,
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, EffectReceipt,
    EffectTermination, LegalAction, ProblemTerminality, RequestContext, RequestId,
    RetainedSurfacePortsV1, RetainedSurfaceServiceV1, RetryDirective,
    retained_surface_application_operation,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, SessionId,
    SessionRefreshOperationIdV1, UserProfileId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_store::{
    SessionRefreshReceiptRequestV1, SessionRefreshStore, SessionRefreshTerminalStateV1,
};
use tracedecay_usecases::context::{SessionRootId, SessionStoreId};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use super::{DirectRetainedSessionPortV1, ProjectRetainedSessionAuthoritiesV1};
use crate::daemon::StoreOwnerKey;
use crate::daemon::retained_owner::session_refresh::admitted_session_refresh_command;
use crate::daemon::session_retrieval::{
    DaemonSessionRetrievalRoot, DaemonSessionRetrievalService, SessionApplicationRetrievalPortV1,
};
use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry;
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use crate::host_admission::HostAdmissionTestRuntimeV1;
use crate::mcp::server::{DaemonSessionRefreshService, DaemonWorkflowIndexReadService};
use crate::mcp::tools::{SessionRefreshServiceOutcome, SessionRefreshServicePort};
use crate::store::GlobalDbSessionTemporalStore;

const DIGEST: &str = "sha256:6161616161616161616161616161616161616161616161616161616161616161";
const BRANCH_ID: &str = "branch.project.test";

struct RetiredRefreshFixture {
    _runtime: HostAdmissionTestRuntimeV1,
    database: RegisteredGlobalDbLeaseV1,
    registry: SessionTemporalRefreshSchedulerRegistry,
    refresh: Arc<DaemonSessionRefreshService>,
    application: RetainedSurfaceServiceV1<'static>,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    profile_id: UserProfileId,
    session_store_id: SessionStoreId,
    session_root_id: SessionRootId,
    configuration_digest: ManifestDigest,
}

impl RetiredRefreshFixture {
    async fn open(temp: &TempDir, label: &str) -> Self {
        let profile_root = temp.path().join(format!("{label}-profile"));
        let project_root = temp.path().join(format!("{label}-project"));
        let project_id = ProjectId::new(format!("project.refresh-{label}")).expect("project id");
        let repository_id =
            RepositoryId::new(format!("repository.refresh-{label}")).expect("repository id");
        let worktree_id =
            WorktreeId::new(format!("worktree.refresh-{label}")).expect("worktree id");
        let runtime =
            HostAdmissionTestRuntimeV1::project(&profile_root, &project_root, project_id.clone())
                .await
                .expect("registered project runtime");
        let database = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .expect("registered project database");
        let owner = owner_for(&database, &project_id);
        let registry = SessionTemporalRefreshSchedulerRegistry::default();
        let wake = registry
            .ensure_project(owner.clone(), database.clone())
            .await;
        registry.retire_project(&owner).await;

        let retrieval_root = DaemonSessionRetrievalRoot::project_identity_for_test(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            project_root.display().to_string(),
        );
        let identity = retrieval_root.identity().clone();
        let profile_id =
            UserProfileId::new(identity.profile_id().as_str().to_owned()).expect("profile id");
        let session_store_id = identity.store_id().clone();
        let session_root_id = identity.root_id().clone();
        let configuration_digest = ManifestDigest::new(DIGEST).expect("configuration digest");
        let retrieval = DaemonSessionRetrievalService::new(
            database.clone(),
            retrieval_root,
            Some(wake.clone()),
        )
        .expect("project retrieval service");
        let refresh = Arc::new(DaemonSessionRefreshService::new(
            database.clone(),
            wake,
            Some(project_id.as_str().to_owned()),
        ));
        let workflow_index = Arc::new(DaemonWorkflowIndexReadService::new(database.clone()));
        let port = DirectRetainedSessionPortV1::project(ProjectRetainedSessionAuthoritiesV1 {
            project_root: project_root.clone(),
            project_id: project_id.clone(),
            profile_id: profile_id.clone(),
            session_store_id: session_store_id.clone(),
            session_root_id: session_root_id.clone(),
            configuration_digest: configuration_digest.clone(),
            refresh: refresh.clone(),
            retrieval: Arc::new(retrieval) as Arc<dyn SessionApplicationRetrievalPortV1>,
            session_database: database.clone(),
            workflow_index,
        });
        let application = RetainedSurfaceServiceV1::new(
            RetainedSurfacePortsV1::default().with_session(Arc::new(port)),
        );
        Self {
            _runtime: runtime,
            database,
            registry,
            refresh,
            application,
            project_id,
            repository_id,
            worktree_id,
            profile_id,
            session_store_id,
            session_root_id,
            configuration_digest,
        }
    }

    fn request(
        &self,
        action: SessionRefreshActionV1,
        session_id: &SessionId,
        handle: Option<String>,
    ) -> SessionRefreshRequestV1 {
        SessionRefreshRequestV1::with_action(
            action,
            SessionRefreshActionRequestV1 {
                project: SessionRefreshProjectV1 {
                    id: self.project_id.as_str().to_owned(),
                    profile_id: self.profile_id.as_str().to_owned(),
                    repository_id: self.repository_id.as_str().to_owned(),
                    worktree_id: self.worktree_id.as_str().to_owned(),
                    branch_id: BRANCH_ID.to_owned(),
                },
                session: SessionRefreshSessionV1 {
                    id: session_id.as_str().to_owned(),
                    store_id: self.session_store_id.as_str().to_owned(),
                    root_id: self.session_root_id.as_str().to_owned(),
                },
                source: SessionRefreshSourceV1 {
                    scope: "cursor".to_owned(),
                },
                target: SessionRefreshTargetV1 {
                    temporal_mode: SessionRefreshTemporalModeV1::Current,
                    grain: SessionRefreshGrainV1::LogicalMessage,
                    frontier: SessionRefreshFrontierV1 {
                        observed_through: 0,
                        committed_through: 0,
                    },
                },
                handle,
                format: None,
            },
        )
    }
}

fn owner_for(database: &RegisteredGlobalDb, project_id: &ProjectId) -> StoreOwnerKey {
    let graph_db_path = database.db_path().to_path_buf();
    let store_root = graph_db_path
        .parent()
        .expect("registered database has a store root")
        .to_path_buf();
    StoreOwnerKey {
        profile_root: store_root.clone(),
        global_db_path: graph_db_path.clone(),
        project_id: Some(project_id.as_str().to_owned()),
        store_root,
        graph_db_path,
    }
}

fn application_context(
    request: &SessionRefreshRequestV1,
    request_id: &str,
) -> (RequestContext, CancellationSignal) {
    let route = &request.request.project;
    let scope = tracedecay_application::ResolvedScope::new(
        ProjectId::new(route.id.clone()).expect("scope project"),
        RepositoryId::new(route.repository_id.clone()).expect("scope repository"),
        WorktreeId::new(route.worktree_id.clone()).expect("scope worktree"),
        Some(
            RefId::new(format!("refs/heads/{}", route.branch_id)).expect("scope branch reference"),
        ),
    )
    .expect("project scope");
    let operation = retained_surface_application_operation(request.operation())
        .expect("session refresh application operation");
    let cancellation_id = format!("cancel.{request_id}");
    let cancellation =
        CancellationContext::active(cancellation_id.clone()).expect("cancellation context");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.{request_id}")).expect("grant id"),
        1,
        ManifestDigest::new(DIGEST).expect("grant digest"),
        ActorId::new("actor.retained.refresh").expect("actor id"),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("capability grant");
    let context = RequestContext::new(
        ActorId::new("actor.retained.refresh").expect("actor id"),
        scope,
        grant,
        RequestId::new(request_id).expect("request id"),
        Deadline::new(UtcMicros(i64::MAX - 1)).expect("deadline"),
        cancellation,
    )
    .expect("request context");
    let signal = CancellationSignal::active(cancellation_id).expect("cancellation signal");
    (context, signal)
}

fn assert_partial_effect(
    problem: ApplicationProblem,
    operation: RetainedSurfaceOperation,
    durable_operation_id: &str,
) -> EffectReceipt {
    assert_eq!(problem.kind(), ApplicationProblemKind::PartialEffect);
    assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
    assert_eq!(problem.retry(), RetryDirective::Never);
    assert_eq!(problem.legal_actions(), &[LegalAction::Reconcile]);
    assert_eq!(
        serde_json::to_value(&problem).expect("serialized problem")["kind"],
        "partial_effect"
    );
    let receipt = problem
        .committed_receipt()
        .expect("partial effect committed receipt")
        .clone();
    receipt.validate().expect("valid committed receipt");
    assert_eq!(receipt.outcome, EffectTermination::Partial);
    let expected = canonical_sha256(&(
        "tracedecay.retained.effect.committed-state.v1",
        operation.as_str(),
        durable_operation_id,
    ))
    .expect("canonical committed-state digest");
    assert_eq!(receipt.committed_state, Some(expected));
    receipt
}

async fn reopen_and_settle(
    temp: &TempDir,
    label: &str,
    project_id: &ProjectId,
    session_id: SessionId,
    operation_id: &str,
) {
    let runtime = HostAdmissionTestRuntimeV1::project(
        temp.path().join(format!("{label}-profile")),
        temp.path().join(format!("{label}-project")),
        project_id.clone(),
    )
    .await
    .expect("reopened registered project runtime");
    let database = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .expect("reopened project database");
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let wake = registry
        .ensure_project(owner_for(&database, project_id), database.clone())
        .await;
    assert!(
        wake.wake_and_wait_until_idle(Duration::from_secs(2)).await,
        "restarted scheduler must settle persisted recovery"
    );
    let receipt = GlobalDbSessionTemporalStore::new(database.as_ref())
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            SessionRefreshOperationIdV1::new(operation_id.to_owned())
                .expect("persisted operation id"),
            session_id,
        ))
        .await
        .expect("read restarted refresh receipt")
        .expect("restarted scheduler settled the operation");
    assert_eq!(receipt.operation_id().as_str(), operation_id);
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    registry.shutdown().await;
}

#[tokio::test]
async fn retained_begin_and_join_report_partial_effect_and_restart_recovers_same_operation() {
    let temp = TempDir::new().expect("temporary fixture");
    let label = "retained-begin-join-reopen";
    let project_id = ProjectId::new(format!("project.refresh-{label}")).expect("project id");
    let session_id = SessionId::new("session.retained.begin-join").expect("session id");

    let operation_id = {
        let fixture = RetiredRefreshFixture::open(&temp, label).await;
        let request = fixture.request(SessionRefreshActionV1::Begin, &session_id, None);
        let public_request = RetainedSurfaceRequestV1::SessionRefresh(request.clone());
        let (context, signal) = application_context(&request, "request.retained.begin");
        let first_problem = fixture
            .application
            .execute(&context, &signal, UtcMicros(2), &public_request)
            .await
            .expect_err("retired scheduler delivery must be a partial effect");
        let recovery = GlobalDbSessionTemporalStore::new(fixture.database.as_ref())
            .session_refresh_recovery(&session_id)
            .await
            .expect("read durable refresh recovery")
            .expect("begin must retain committed recovery");
        let operation_id = recovery.operation_id().as_str().to_owned();
        let first_receipt = assert_partial_effect(
            first_problem,
            RetainedSurfaceOperation::SessionRefreshBegin,
            &operation_id,
        );

        let (joined_context, joined_signal) =
            application_context(&request, "request.retained.join");
        let joined_problem = fixture
            .application
            .execute(
                &joined_context,
                &joined_signal,
                UtcMicros(3),
                &public_request,
            )
            .await
            .expect_err("an identical committed begin must join and preserve failed delivery");
        let joined_recovery = GlobalDbSessionTemporalStore::new(fixture.database.as_ref())
            .session_refresh_recovery(&session_id)
            .await
            .expect("read joined recovery")
            .expect("joined begin keeps the recovery row");
        assert_eq!(joined_recovery.operation_id().as_str(), operation_id);
        let joined_receipt = assert_partial_effect(
            joined_problem,
            RetainedSurfaceOperation::SessionRefreshBegin,
            &operation_id,
        );
        assert_eq!(
            joined_receipt.committed_state,
            first_receipt.committed_state
        );

        let (direct_context, direct_signal) =
            application_context(&request, "request.retained.join-direct");
        let command = admitted_session_refresh_command(
            &request,
            &direct_context,
            &direct_signal,
            &fixture.profile_id,
            &fixture.session_store_id,
            &fixture.session_root_id,
            &fixture.configuration_digest,
        )
        .expect("admitted joined command");
        assert!(matches!(
            fixture.refresh.execute(command).await,
            SessionRefreshServiceOutcome::JoinedReconciliationRequired {
                operation_id: found,
                ..
            } if found == operation_id
        ));

        fixture.registry.shutdown().await;
        operation_id
    };

    reopen_and_settle(&temp, label, &project_id, session_id, &operation_id).await;
}

#[tokio::test]
async fn retained_cancel_reports_partial_effect_with_canonical_cancelled_receipt() {
    let temp = TempDir::new().expect("temporary fixture");
    let label = "retained-cancel";
    let fixture = RetiredRefreshFixture::open(&temp, label).await;
    let session_id = SessionId::new("session.retained.cancel").expect("session id");
    let begin = fixture.request(SessionRefreshActionV1::Begin, &session_id, None);
    let (begin_context, begin_signal) =
        application_context(&begin, "request.retained.cancel-begin");
    let begin_command = admitted_session_refresh_command(
        &begin,
        &begin_context,
        &begin_signal,
        &fixture.profile_id,
        &fixture.session_store_id,
        &fixture.session_root_id,
        &fixture.configuration_digest,
    )
    .expect("admitted begin command");
    let (operation_id, handle) = match fixture.refresh.execute(begin_command).await {
        SessionRefreshServiceOutcome::StartedReconciliationRequired {
            operation_id,
            handle,
            ..
        } => (operation_id, handle),
        other => panic!("expected committed begin requiring reconciliation, got {other:?}"),
    };

    let cancel = fixture.request(SessionRefreshActionV1::Cancel, &session_id, Some(handle));
    let public_cancel = RetainedSurfaceRequestV1::SessionRefresh(cancel.clone());
    let (cancel_context, cancel_signal) =
        application_context(&cancel, "request.retained.cancel-effect");
    let problem = fixture
        .application
        .execute(
            &cancel_context,
            &cancel_signal,
            UtcMicros(2),
            &public_cancel,
        )
        .await
        .expect_err("cancel commit with retired delivery must be a partial effect");
    assert_partial_effect(
        problem,
        RetainedSurfaceOperation::SessionRefreshCancel,
        &operation_id,
    );

    let receipt = GlobalDbSessionTemporalStore::new(fixture.database.as_ref())
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            SessionRefreshOperationIdV1::new(operation_id.clone()).expect("operation id"),
            session_id,
        ))
        .await
        .expect("read durable cancel receipt")
        .expect("cancel must commit a terminal receipt");
    assert_eq!(receipt.operation_id().as_str(), operation_id);
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Cancelled);
    fixture.registry.shutdown().await;
}
