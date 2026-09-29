//! The daemon's owner of profile configuration requests.
//!
//! A configuration request that reads or writes only profile settings names
//! no project, so the composition root serves it from the authenticated
//! profile's registered `ProfileSessions` store for every transport, inside a
//! project or outside one.

use tracedecay_contracts::{CancellationContext, ConfigurationWireRequestV1, Deadline};
use tracedecay_daemon_protocol::DaemonInvocationResponse;
use tracedecay_daemon_service::{ProfileConfigurationAuthorityV1, execute_profile_configuration};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ActorId, UtcMicros};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::StoreAdministration;
use super::profile_retained::{authority_problem, profile_session_scope};

const PROFILE_CONFIGURATION_ACTOR: &str = "actor.tracedecay-daemon.profile";

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.profile_configuration.invoke", future = true)]
pub(super) async fn invoke_profile_configuration(
    store_administration: &StoreAdministration,
    request_id: String,
    surface_operation: ApplicationSurfaceOperation,
    request: ConfigurationWireRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let authority = match Box::pin(profile_configuration_authority(store_administration)).await {
        Ok(authority) => authority,
        Err(error) => {
            return DaemonInvocationResponse::application_problem(
                request_id,
                authority_problem(&error),
            );
        }
    };
    Box::pin(execute_profile_configuration(
        request_id,
        authority,
        surface_operation,
        request,
        observed_at,
        deadline,
        cancellation,
    ))
    .await
}

async fn profile_configuration_authority(
    store_administration: &StoreAdministration,
) -> Result<ProfileConfigurationAuthorityV1> {
    let identity = store_administration.profile_identity()?;
    let database = Box::pin(store_administration.registered_profile_session_database()).await?;
    Ok(ProfileConfigurationAuthorityV1 {
        database,
        profile_id: identity.profile_id().clone(),
        scope: profile_session_scope(identity)?,
        actor: ActorId::new(PROFILE_CONFIGURATION_ACTOR).map_err(|error| {
            TraceDecayError::Config {
                message: format!("profile configuration actor is invalid: {error}"),
            }
        })?,
    })
}
