use std::sync::Arc;

use tracedecay_contracts::retained_surfaces::{
    RetainedOutcomeStatusV1, RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
    SessionRefreshActionRequestV1, SessionRefreshActionV1, SessionRefreshFrontierV1,
    SessionRefreshGrainV1, SessionRefreshRequestV1, SessionRefreshScopeV1, SessionRefreshSessionV1,
    SessionRefreshSourceV1, SessionRefreshTargetV1, SessionRefreshTemporalModeV1,
};
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemKind, ApplicationResult, CancellationSignal, Deadline,
    RequestId, now_micros,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::UtcMicros;
use tracedecay_session_memory::context::ResolvedSessionIdentity;
use tracedecay_session_runtime::retained::{
    ProfileRetainedAuthoritiesV1, ProfileRetainedConnectionAuthorityV1,
    RetainedSessionRefreshPortV1, execute_profile_retained_application,
    profile_retained_connection_authority, profile_session_retrieval_serving_identity,
};
use tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalRoot;
use tracedecay_session_runtime::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry;

use crate::mcp::server::DaemonSessionRefreshService;

fn profile_retrieval_root(
    profile_identity: &dyn tracedecay_contracts::ProfileIdentityReadPort,
) -> DaemonSessionRetrievalRoot {
    let shard = tracedecay_store::StoreShardIdV1::profile_sessions(
        profile_identity.brain_id().clone(),
        profile_identity.profile_id().clone(),
    );
    let serving_db =
        tracedecay_sessions::runtime::user_sessions_db_path(profile_identity.profile_root());
    let serving = profile_session_retrieval_serving_identity(profile_identity, &shard, &serving_db)
        .expect("profile serving identity");
    DaemonSessionRetrievalRoot::profile(serving).expect("profile retrieval root")
}

fn refresh_request(
    action: SessionRefreshActionV1,
    scope: SessionRefreshScopeV1,
    handle: Option<String>,
) -> RetainedSurfaceRequestV1 {
    RetainedSurfaceRequestV1::SessionRefresh(SessionRefreshRequestV1::with_action(
        action,
        SessionRefreshActionRequestV1 {
            scope,
            session: SessionRefreshSessionV1 {
                id: "session.profile-refresh".to_owned(),
            },
            source: SessionRefreshSourceV1 {
                scope: "codex".to_owned(),
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
    ))
}

fn profile_scope() -> SessionRefreshScopeV1 {
    SessionRefreshScopeV1::Profile {}
}

async fn execute_refresh(
    profile_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    session_identity: &ResolvedSessionIdentity,
    connection: &ProfileRetainedConnectionAuthorityV1,
    refresh: Option<&dyn RetainedSessionRefreshPortV1>,
    request: RetainedSurfaceRequestV1,
    label: &str,
) -> ApplicationResult<RetainedSurfaceResultV1> {
    execute_profile_retained_application(
        ProfileRetainedAuthoritiesV1 {
            profile_sessions: Some(Arc::new(move || {
                let database = profile_sessions.clone();
                Box::pin(async move { Ok(database) })
            })),
            session_identity: session_identity.clone(),
            configuration_digest: connection.configuration_digest().clone(),
            lcm_authority: None,
            session_refresh: refresh,
            memory: None,
        },
        connection,
        request,
        RequestId::new(format!("request.profile-refresh.{label}")).expect("request identity"),
        Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
        CancellationSignal::active(format!("cancellation.profile-refresh.{label}"))
            .expect("cancellation"),
    )
    .await
    .expect("profile refresh transport")
}

/// The profile session authority owns profile-scoped refreshes end to end:
/// begin issues an opaque handle bound to the profile store, status reads
/// it back, and cancel settles a receipt — all through one canonical
/// request shape and without any project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_retained_session_refresh_begins_reads_and_cancels_in_the_profile_store() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let profile_identity =
        profile_identity::load_or_create(&profile_root).expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "profile retained session refresh",
    )
    .expect("daemon database scope");
    let runtime_registry =
        tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(profile_identity.clone())
            .await
            .expect("profile session runtime registry");
    let profile_database = runtime_registry
        .profile_sessions()
        .await
        .expect("profile session database");
    let schedulers = SessionTemporalRefreshSchedulerRegistry::default();
    let wake = schedulers
        .ensure_profile(
            profile_database.db_path().to_path_buf(),
            profile_database.clone(),
        )
        .await;
    let refresh = DaemonSessionRefreshService::new(profile_database.clone(), Arc::new(wake), None);
    let session_identity = profile_retrieval_root(&profile_identity).identity().clone();
    let connection = profile_retained_connection_authority(&profile_identity, &session_identity)
        .expect("profile retained connection authority");

    let begun = execute_refresh(
        profile_database.clone(),
        &session_identity,
        &connection,
        Some(&refresh),
        refresh_request(SessionRefreshActionV1::Begin, profile_scope(), None),
        "begin",
    )
    .await
    .expect("profile refresh begin must be mounted");
    let ApplicationOutcome::Effect(effect) = begun.outcome else {
        panic!("begin must be an effect")
    };
    let Some(RetainedSurfaceResultV1::SessionRefreshBegin(begin)) = effect.payload else {
        panic!("begin must return its typed payload")
    };
    assert!(matches!(
        begin.outcome,
        RetainedOutcomeStatusV1::Started | RetainedOutcomeStatusV1::Joined
    ));
    assert_eq!(begin.scope, "profile");
    assert_eq!(begin.tool, "tracedecay_session_refresh_begin");
    let handle = begin.handle.expect("opaque refresh handle");
    assert!(handle.starts_with("srh_"), "{handle}");
    let operation_id = begin.operation_id.expect("durable operation id");
    assert_ne!(handle, operation_id);

    let status = execute_refresh(
        profile_database.clone(),
        &session_identity,
        &connection,
        Some(&refresh),
        refresh_request(
            SessionRefreshActionV1::Status,
            profile_scope(),
            Some(handle.clone()),
        ),
        "status",
    )
    .await
    .expect("profile refresh status must be mounted");
    let ApplicationOutcome::Evidence(packet) = status.outcome else {
        panic!("status must be evidence")
    };
    let Some(RetainedSurfaceResultV1::SessionRefreshStatus(status)) = packet.payload else {
        panic!("status must return its typed payload")
    };
    assert!(
        matches!(
            status.outcome,
            RetainedOutcomeStatusV1::Running | RetainedOutcomeStatusV1::Complete
        ),
        "{status:?}"
    );
    assert_eq!(status.scope, "profile");
    assert_eq!(status.tool, "tracedecay_session_refresh_status");

    let cancelled = execute_refresh(
        profile_database,
        &session_identity,
        &connection,
        Some(&refresh),
        refresh_request(
            SessionRefreshActionV1::Cancel,
            profile_scope(),
            Some(handle.clone()),
        ),
        "cancel",
    )
    .await
    .expect("profile refresh cancel must be mounted");
    let ApplicationOutcome::Effect(effect) = cancelled.outcome else {
        panic!("cancel must be an effect")
    };
    let Some(RetainedSurfaceResultV1::SessionRefreshCancel(cancel)) = effect.payload else {
        panic!("cancel must return its typed payload")
    };
    assert!(
        matches!(
            cancel.outcome,
            RetainedOutcomeStatusV1::Cancelled | RetainedOutcomeStatusV1::Complete
        ),
        "{cancel:?}"
    );
    let receipt = cancel.receipt.expect("terminal receipt");
    assert_eq!(receipt.operation_id, operation_id);
    assert_eq!(cancel.scope, "profile");
    assert_eq!(cancel.tool, "tracedecay_session_refresh_cancel");
    schedulers.shutdown().await;
}

/// The profile authority never serves a project-scoped refresh, a foreign
/// profile's refresh, or a refresh without a mounted refresh service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_retained_session_refresh_refuses_foreign_owners_and_unmounted_service() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    let profile_identity =
        profile_identity::load_or_create(&profile_root).expect("durable profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "profile retained session refresh refusals",
    )
    .expect("daemon database scope");
    let runtime_registry =
        tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(profile_identity.clone())
            .await
            .expect("profile session runtime registry");
    let profile_database = runtime_registry
        .profile_sessions()
        .await
        .expect("profile session database");
    let schedulers = SessionTemporalRefreshSchedulerRegistry::default();
    let wake = schedulers
        .ensure_profile(
            profile_database.db_path().to_path_buf(),
            profile_database.clone(),
        )
        .await;
    let refresh = DaemonSessionRefreshService::new(profile_database.clone(), Arc::new(wake), None);
    let session_identity = profile_retrieval_root(&profile_identity).identity().clone();
    let connection = profile_retained_connection_authority(&profile_identity, &session_identity)
        .expect("profile retained connection authority");

    let project_scoped = execute_refresh(
        profile_database.clone(),
        &session_identity,
        &connection,
        Some(&refresh),
        refresh_request(
            SessionRefreshActionV1::Begin,
            SessionRefreshScopeV1::Project {},
            None,
        ),
        "project-scoped",
    )
    .await
    .expect_err("a project-scoped refresh must not reach the profile store");
    assert_eq!(
        project_scoped.problem.kind,
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );

    let unmounted = execute_refresh(
        profile_database,
        &session_identity,
        &connection,
        None,
        refresh_request(SessionRefreshActionV1::Begin, profile_scope(), None),
        "unmounted",
    )
    .await
    .expect_err("an unmounted refresh service is a typed unavailable terminal");
    assert_eq!(unmounted.problem.kind, ApplicationProblemKind::Unavailable);
    assert!(
        unmounted
            .problem
            .message
            .contains("profile session refresh authority is not mounted"),
        "{}",
        unmounted.problem.message
    );
    schedulers.shutdown().await;
}
