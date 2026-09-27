//! The daemon's owner of profile-targeted retained requests.
//!
//! A retained request with the profile invocation target reads or writes the
//! authenticated profile's own memory, session, and LCM stores. It names no
//! project, so the composition root serves it from the profile's store
//! administration for every transport: CLI invocations, project MCP servers,
//! and projectless connections.

use std::sync::Arc;

use tracedecay_contracts::retained_surfaces::{RetainedSurfaceOperation, RetainedSurfaceRequestV1};
use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, CancellationSignal, CancellationState, Deadline,
    RequestId, SafeDiagnostic, now_micros,
};
use tracedecay_daemon_protocol::{
    DaemonClientIdentity, DaemonInvocationOutcome, DaemonInvocationProblem,
    DaemonInvocationResponse,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_session_runtime::lcm_authority::mount_registered_lcm_authority;
use tracedecay_session_runtime::retained::{
    ProfileRetainedAuthoritiesV1, ProfileRetainedConnectionAuthorityV1, ProfileRetainedTerminalV1,
    RetainedSessionRefreshPortV1, execute_profile_retained_application,
    profile_retained_connection_authority, profile_session_retrieval_serving_identity,
};
use tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalRoot;
use tracedecay_sessions::runtime::user_sessions_db_path;
use tracedecay_store::StoreShardIdV1;

use super::{StoreAdministration, await_user_profile_host_admission_replay_for_identity};

/// Serve one profile-targeted retained request and settle its typed terminal.
///
/// `request_cancellation` is the daemon's registered cancellation for this
/// request; a transport that cancels relays it into the owner's signal.
#[hotpath::measure(label = "daemon.profile_retained.invoke", future = true)]
pub(super) async fn invoke_profile_retained(
    store_administration: &StoreAdministration,
    request_id: String,
    request: RetainedSurfaceRequestV1,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: Option<CancellationToken>,
) -> DaemonInvocationResponse {
    let (Ok(application_request_id), Ok(signal)) = (
        RequestId::new(request_id.clone()),
        CancellationSignal::active(cancellation.token_id.as_str()),
    ) else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    if let CancellationState::Cancelled { requested_at } = &cancellation.state {
        signal.cancel(*requested_at);
    }
    let execution = Box::pin(execute_profile_retained(
        store_administration,
        request,
        application_request_id,
        deadline,
        signal.clone(),
    ));
    let terminal = match request_cancellation {
        Some(token) => {
            tokio::pin!(execution);
            tokio::select! {
                terminal = &mut execution => terminal,
                () = token.cancelled() => {
                    signal.cancel(now_micros());
                    execution.await
                }
            }
        }
        None => execution.await,
    };
    match terminal {
        Ok(ProfileRetainedTerminalV1 {
            scope,
            outcome: Ok(outcome),
        }) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::RetainedApplication { scope, outcome },
        ),
        Ok(ProfileRetainedTerminalV1 {
            scope,
            outcome: Err(problem),
        }) => DaemonInvocationResponse::retained_application_problem(request_id, scope, problem),
        Err(error) => {
            DaemonInvocationResponse::application_problem(request_id, authority_problem(&error))
        }
    }
}

/// A profile authority that could not be mounted keeps its reason code and
/// retry verdict as a typed unavailable terminal.
fn authority_problem(error: &TraceDecayError) -> ApplicationProblem {
    if let Some((authority, reason)) = tracedecay_mcp::reset_required_context(error) {
        return ApplicationProblem::reset_required(SafeDiagnostic {
            code: "application.retained.profile-reset-required".to_owned(),
            message: format!(
                "The {authority} requires an explicit reset: {reason}. Reset it with `{}`",
                tracedecay_mcp::reset_required_command(&authority, None)
            ),
        });
    }
    let (code, retryable) = match error {
        TraceDecayError::ProjectRoute {
            reason_code,
            retryable,
            ..
        } => (reason_code.clone(), *retryable),
        _ => (
            "application.retained.profile-authority-unavailable".to_owned(),
            false,
        ),
    };
    let diagnostic = SafeDiagnostic {
        code,
        message: format!("The profile retained authority is unavailable: {error}"),
    };
    if retryable {
        return ApplicationProblem::unavailable(diagnostic);
    }
    ApplicationProblem::Unavailable {
        classification: tracedecay_contracts::ApplicationUnavailableClassV1::Authority,
        diagnostic,
        retry: tracedecay_contracts::RetryDirective::Never,
        legal_actions: Vec::new(),
        detail: None,
    }
}

async fn execute_profile_retained(
    store_administration: &StoreAdministration,
    request: RetainedSurfaceRequestV1,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationSignal,
) -> Result<ProfileRetainedTerminalV1> {
    let operation = request.operation();
    let lcm = matches!(
        operation,
        RetainedSurfaceOperation::LcmStatus
            | RetainedSurfaceOperation::LcmDoctor
            | RetainedSurfaceOperation::LcmLoadSession
            | RetainedSurfaceOperation::LcmGrep
            | RetainedSurfaceOperation::LcmDescribe
            | RetainedSurfaceOperation::LcmExpand
            | RetainedSurfaceOperation::LcmExpandQuery
            | RetainedSurfaceOperation::MessageSearch
    );
    let refresh = matches!(
        operation,
        RetainedSurfaceOperation::SessionRefreshBegin
            | RetainedSurfaceOperation::SessionRefreshStatus
            | RetainedSurfaceOperation::SessionRefreshCancel
    );
    let (connection, session_root) = profile_retained_connection(store_administration)?;
    if lcm {
        // Retained profile events replay into the session store before any
        // profile read answers from it.
        let profile_root = store_administration
            .profile_identity()?
            .profile_root()
            .to_path_buf();
        Box::pin(await_user_profile_host_admission_replay_for_identity(
            store_administration,
            &DaemonClientIdentity::new(profile_root.clone(), profile_root.join("global.db")),
        ))
        .await?;
    }
    let runtime_registry = Box::pin(store_administration.registered_runtime_registry()).await?;
    let (lcm_authority, session_refresh) = if lcm || refresh {
        let database = Box::pin(store_administration.registered_profile_session_database()).await?;
        let lcm_authority = session_root.expected_runtime_shard().and_then(|shard| {
            mount_registered_lcm_authority(database.clone(), session_root.identity().clone(), shard)
        });
        let session_refresh =
            Box::pin(store_administration.profile_session_refresh(&database)).await;
        (lcm_authority, Some(session_refresh))
    } else {
        (None, None)
    };
    let authorities = ProfileRetainedAuthoritiesV1 {
        profile_sessions: Some(Arc::new(|| Box::pin(runtime_registry.profile_sessions()))),
        session_identity: connection.session_identity().clone(),
        configuration_digest: connection.configuration_digest().clone(),
        lcm_authority: lcm_authority.as_deref(),
        session_refresh: session_refresh
            .as_ref()
            .map(|refresh| refresh.service.as_ref() as &dyn RetainedSessionRefreshPortV1),
        refresh_status: session_refresh
            .as_ref()
            .map(|refresh| Arc::clone(&refresh.serving)),
        memory: Some(Arc::new(
            tracedecay_store_runtime::retained_memory::DirectRetainedMemoryPortV1::profile(
                runtime_registry.as_ref(),
                connection.configuration_digest().clone(),
            ),
        )),
    };
    Box::pin(execute_profile_retained_application(
        authorities,
        &connection,
        request,
        request_id,
        deadline,
        cancellation,
    ))
    .await
}

/// The invocation executor of a connection that opened no project. It serves
/// only the profile's retained stores; every other daemon payload names a
/// project this connection does not have.
#[derive(Clone)]
pub(super) struct ProfileRetainedExecutor {
    pub(super) store_administration: StoreAdministration,
}

impl tracedecay_contracts::ApplicationInvocationExecutor for ProfileRetainedExecutor {
    fn invoke(
        &self,
        invocation: tracedecay_contracts::ApplicationInvocation,
    ) -> tracedecay_contracts::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_contracts::ApplicationResponse,
            tracedecay_contracts::InvocationError,
        >,
    > {
        Box::pin(async move {
            let (context, request) = invocation.into_parts();
            let tracedecay_contracts::ApplicationRequest::Surface { binding, payload } = request
            else {
                return Err(tracedecay_contracts::InvocationError::InvalidRequest);
            };
            tracedecay_daemon_protocol::invoke_application_surface(self, context, binding, payload)
                .await
        })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for ProfileRetainedExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        _deadline: Deadline,
        cancellation: CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        Box::pin(async move {
            let tracedecay_daemon_protocol::DaemonInvocationPayload::ProfileRetainedApplication {
                request: retained_request,
                deadline,
                cancellation: context,
                ..
            } = request.payload
            else {
                return Ok(DaemonInvocationResponse::problem(
                    request.request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                ));
            };
            let token = CancellationToken::new();
            let invocation = invoke_profile_retained(
                &self.store_administration,
                request.request_id,
                retained_request,
                deadline,
                context,
                Some(token.clone()),
            );
            tokio::pin!(invocation);
            Ok(tokio::select! {
                response = &mut invocation => response,
                () = cancellation.cancelled() => {
                    token.cancel();
                    invocation.await
                }
            })
        })
    }

    fn observe_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<'_, Result<()>> {
        // Retained operations publish no feedback observations.
        Box::pin(async { Ok(()) })
    }
}

/// The profile's retained connection authority and session retrieval root,
/// derived from the daemon's pinned profile identity.
fn profile_retained_connection(
    store_administration: &StoreAdministration,
) -> Result<(
    ProfileRetainedConnectionAuthorityV1,
    DaemonSessionRetrievalRoot,
)> {
    let profile_identity = store_administration.profile_identity()?;
    let shard = StoreShardIdV1::profile_sessions(
        profile_identity.brain_id().clone(),
        profile_identity.profile_id().clone(),
    );
    let serving_db = user_sessions_db_path(profile_identity.profile_root());
    let serving = profile_session_retrieval_serving_identity(profile_identity, &shard, &serving_db)
        .ok_or_else(|| TraceDecayError::Config {
            message: "profile session identity is unavailable".to_owned(),
        })?;
    let session_root =
        DaemonSessionRetrievalRoot::profile(serving).ok_or_else(|| TraceDecayError::Config {
            message: "profile session authority is unavailable".to_owned(),
        })?;
    let connection =
        profile_retained_connection_authority(profile_identity, session_root.identity())?;
    Ok((connection, session_root))
}
