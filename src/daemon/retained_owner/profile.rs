//! Profile-scoped retained admission and direct typed memory/LCM execution.

use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::retained_surfaces::{
    RetainedSurfaceOperation, RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOperation, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationResult, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, RequestId, RetainedSurfacePortsV1, RetainedSurfaceServiceV1,
    RetryDirective, SafeDiagnostic, now_micros,
};
use tracedecay_domain::{
    ActorId, BrainId, ManifestDigest, UserProfileId, UtcMicros, canonical_sha256,
};
use tracedecay_usecases::context::ResolvedSessionIdentity;

use super::lcm::DirectRetainedLcmPortV1;
use super::memory::DirectRetainedMemoryPortV1;
use crate::errors::TraceDecayError;

/// Exact mounted authorities for a profile-retained request.
#[derive(Clone)]
pub(crate) struct ProfileRetainedAuthoritiesV1<'a> {
    pub(crate) runtime_registry:
        Option<&'a crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
    pub(crate) session_identity: ResolvedSessionIdentity,
    pub(crate) configuration_digest: ManifestDigest,
    pub(crate) lcm_authority: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
}

const PROFILE_RETAINED_REQUEST_GRANT_REVISION_V1: u64 = 1;
const PROFILE_RETAINED_ACTOR_DOMAIN_V1: &str =
    "tracedecay.daemon.profile-retained.local-profile-actor.v1";
const PROFILE_RETAINED_REQUEST_GRANT_DOMAIN_V1: &str =
    "tracedecay.daemon.profile-retained.request-grant.v1";

/// Durable identity authority retained for one authenticated local-profile
/// connection. Grants are deliberately absent: each request is admitted for
/// one operation under its exact controls.
#[derive(Clone)]
pub(crate) struct ProfileRetainedConnectionAuthorityV1 {
    brain_id: BrainId,
    user_profile_id: UserProfileId,
    actor: ActorId,
    session_identity: ResolvedSessionIdentity,
    configuration_digest: ManifestDigest,
}

impl ProfileRetainedConnectionAuthorityV1 {
    pub(crate) fn session_identity(&self) -> &ResolvedSessionIdentity {
        &self.session_identity
    }

    pub(crate) fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    fn admit_request(
        &self,
        operation: &ApplicationOperation,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &CancellationSignal,
        observed_at: UtcMicros,
    ) -> Result<RequestContext, TraceDecayError> {
        if deadline.is_elapsed_at(observed_at) {
            return Err(TraceDecayError::Config {
                message: "profile retained request deadline elapsed before admission".to_owned(),
            });
        }
        let scope = self
            .session_identity
            .session_request_scope()
            .map_err(|error| TraceDecayError::Config {
                message: format!("profile retained request scope is invalid: {error}"),
            })?;
        let cancellation_context = cancellation.context();
        let capabilities = BTreeSet::from([operation.capability_id().clone()]);
        let use_cases = BTreeSet::from([operation.use_case_id().clone()]);
        let grant_digest = canonical_sha256(&(
            PROFILE_RETAINED_REQUEST_GRANT_DOMAIN_V1,
            &self.brain_id,
            &self.user_profile_id,
            self.session_identity.store_id(),
            self.session_identity.root_id(),
            &scope,
            &self.actor,
            &self.configuration_digest,
            operation.capability_id(),
            operation.use_case_id(),
            &request_id,
            &deadline,
            &cancellation_context,
            observed_at,
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request grant digest failed: {error}"),
        })?;
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!(
                "grant.tracedecay-daemon.profile-retained.request.{}",
                grant_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!("profile retained request grant identity is invalid: {error}"),
            })?,
            PROFILE_RETAINED_REQUEST_GRANT_REVISION_V1,
            grant_digest,
            self.actor.clone(),
            observed_at,
            deadline.expires_at,
            scope.clone(),
            capabilities,
            use_cases,
            DisclosureClass::Sensitive,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request grant is invalid: {error}"),
        })?;
        RequestContext::new(
            self.actor.clone(),
            scope,
            grant,
            request_id,
            deadline,
            cancellation_context,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request context is invalid: {error}"),
        })
    }
}

fn profile_retained_configuration_digest(
    brain_id: &BrainId,
    user_profile_id: &UserProfileId,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ManifestDigest, TraceDecayError> {
    canonical_sha256(&(
        "tracedecay.daemon.profile-retained.configuration.v1",
        brain_id,
        user_profile_id,
        session_identity.store_id(),
        session_identity.root_id(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained configuration digest failed: {error}"),
    })
}

pub(crate) fn profile_retained_connection_authority(
    identity: &crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ProfileRetainedConnectionAuthorityV1, TraceDecayError> {
    profile_retained_connection_authority_from_persisted_identity(
        identity.brain_id(),
        identity.profile_id(),
        session_identity,
    )
}

fn profile_retained_connection_authority_from_persisted_identity(
    brain_id: &BrainId,
    user_profile_id: &UserProfileId,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ProfileRetainedConnectionAuthorityV1, TraceDecayError> {
    if session_identity.project_id().is_some()
        || session_identity.profile_id().as_str() != user_profile_id.as_str()
    {
        return Err(TraceDecayError::Config {
            message: "profile retained connection requires the exact profile session identity"
                .to_owned(),
        });
    }
    session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained connection scope is invalid: {error}"),
        })?;
    let configuration_digest =
        profile_retained_configuration_digest(brain_id, user_profile_id, session_identity)?;
    let actor_digest =
        canonical_sha256(&(PROFILE_RETAINED_ACTOR_DOMAIN_V1, brain_id, user_profile_id)).map_err(
            |error| TraceDecayError::Config {
                message: format!("profile retained actor digest failed: {error}"),
            },
        )?;
    let actor = ActorId::new(format!(
        "actor.tracedecay-daemon.profile-retained.{}",
        actor_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained policy actor is invalid: {error}"),
    })?;
    Ok(ProfileRetainedConnectionAuthorityV1 {
        brain_id: brain_id.clone(),
        user_profile_id: user_profile_id.clone(),
        actor,
        session_identity: session_identity.clone(),
        configuration_digest,
    })
}

/// Execute one profile-scoped retained request through canonical admission and
/// render the typed application result only after execution has completed.
pub(crate) async fn execute_profile_retained_application(
    authorities: ProfileRetainedAuthoritiesV1<'_>,
    connection: &ProfileRetainedConnectionAuthorityV1,
    request: RetainedSurfaceRequestV1,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationSignal,
) -> Result<ApplicationResult<RetainedSurfaceResultV1>, TraceDecayError> {
    let observed_at = now_micros();
    let operation =
        tracedecay_application::retained_surface_application_operation(request.operation())
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
    let scope = authorities
        .session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    if deadline.is_elapsed_at(observed_at) {
        return Ok(Err(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        )));
    }
    if connection.user_profile_id.as_str() != authorities.session_identity.profile_id().as_str()
        || connection.session_identity != authorities.session_identity
    {
        return Ok(Err(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )));
    }
    if connection.configuration_digest != authorities.configuration_digest {
        return Ok(Err(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::stale(SafeDiagnostic {
                code: "application.retained.profile-configuration-stale".to_owned(),
                message: "The retained profile authority changed after connection admission."
                    .to_owned(),
            }),
        )));
    }
    let context = connection.admit_request(
        &operation,
        request_id.clone(),
        deadline,
        &cancellation,
        observed_at,
    )?;
    let ports = profile_retained_surface_ports(&authorities)?;
    let service = RetainedSurfaceServiceV1::new(ports);
    Ok(
        match service
            .execute(&context, &cancellation, observed_at, &request)
            .await
        {
            Ok(outcome) => Ok(ApplicationEnvelope {
                contract: operation.result_contract().clone(),
                request_id,
                scope,
                outcome,
            }),
            Err(problem) => Err(ApplicationProblemEnvelope::new(
                operation.result_contract().clone(),
                request_id,
                problem,
            )),
        },
    )
}

fn profile_retained_surface_ports<'a>(
    authorities: &'a ProfileRetainedAuthoritiesV1<'a>,
) -> Result<RetainedSurfacePortsV1<'a>, TraceDecayError> {
    authorities
        .session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained runtime identity is invalid: {error}"),
        })?;
    let mut ports = RetainedSurfacePortsV1::default();
    if let Some(runtime_registry) = authorities.runtime_registry {
        ports = ports.with_memory(Arc::new(DirectRetainedMemoryPortV1::profile(
            runtime_registry,
            authorities.configuration_digest.clone(),
        )));
        ports = ports.with_lcm(Arc::new(DirectRetainedLcmPortV1::profile(
            runtime_registry,
            authorities.session_identity.clone(),
            authorities.lcm_authority,
        )));
    }
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use tracedecay_application::retained_surfaces::{MemoryScopeV1, MemoryStatusRequestV1};
    use tracedecay_application::{ApplicationProblemKind, CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::{BrainId, ManifestDigest, UserProfileId, UtcMicros};
    use tracedecay_usecases::context::{
        ProfileId, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    };

    use super::*;

    fn identity(profile: &str) -> ResolvedSessionIdentity {
        ResolvedSessionIdentity::for_profile(
            ProfileId::new(profile).expect("profile id"),
            SessionStoreId::new(format!("store.{profile}")).expect("store id"),
            SessionRootId::new(format!("root.{profile}")).expect("root id"),
        )
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("manifest digest")
    }

    fn request() -> RetainedSurfaceRequestV1 {
        RetainedSurfaceRequestV1::MemoryStatus(MemoryStatusRequestV1 {
            memory_scope: Some(MemoryScopeV1::User),
            project_selector: None,
            project_id: None,
            project_path: None,
            format: None,
        })
    }

    fn connection(identity: &ResolvedSessionIdentity) -> ProfileRetainedConnectionAuthorityV1 {
        profile_retained_connection_authority_from_persisted_identity(
            &BrainId::new("brain.profile-retained-test").expect("brain id"),
            &UserProfileId::new(identity.profile_id().as_str()).expect("user profile id"),
            identity,
        )
        .expect("profile connection authority")
    }

    async fn denied_kind(
        authorities: ProfileRetainedAuthoritiesV1<'_>,
        connection: &ProfileRetainedConnectionAuthorityV1,
        request_id: &str,
    ) -> ApplicationProblemKind {
        let cancellation =
            CancellationSignal::active(format!("cancellation.{request_id}")).expect("cancellation");
        execute_profile_retained_application(
            authorities,
            connection,
            request(),
            RequestId::new(request_id).expect("request id"),
            Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
            cancellation,
        )
        .await
        .expect("transport result")
        .expect_err("request must be denied before port execution")
        .problem
        .kind
    }

    #[tokio::test]
    async fn connection_admission_denies_a_different_profile_scope() {
        let admitted_identity = identity("profile.retained-admitted");
        let requested_identity = identity("profile.retained-other");
        let connection = connection(&admitted_identity);
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity: requested_identity,
                configuration_digest: connection.configuration_digest().clone(),
                lcm_authority: None,
            },
            &connection,
            "request.profile-retained-scope-denial",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::NotFoundOrNotAuthorized);
    }

    #[tokio::test]
    async fn connection_admission_reports_changed_configuration_as_stale() {
        let session_identity = identity("profile.retained-stale");
        let connection = connection(&session_identity);
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity,
                configuration_digest: digest('c'),
                lcm_authority: None,
            },
            &connection,
            "request.profile-retained-stale-configuration",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::Stale);
    }

    #[test]
    fn request_admission_binds_one_operation_and_exact_request_controls() {
        let session_identity = identity("profile.retained-request-bound");
        let connection = connection(&session_identity);
        let operation = tracedecay_application::retained_surface_application_operation(
            RetainedSurfaceOperation::MemoryStatus,
        )
        .expect("retained operation");
        let observed_at = UtcMicros(100);
        let deadline = Deadline::new(UtcMicros(200)).expect("deadline");
        let cancellation = CancellationSignal::active("cancellation.profile-retained-bound")
            .expect("cancellation");
        let request_id = RequestId::new("request.profile-retained-bound").expect("request id");

        let context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                deadline.clone(),
                &cancellation,
                observed_at,
            )
            .expect("request admission");
        let other_request = connection
            .admit_request(
                &operation,
                RequestId::new("request.profile-retained-other").expect("other request id"),
                deadline.clone(),
                &cancellation,
                observed_at,
            )
            .expect("other request admission");
        let other_cancellation =
            CancellationSignal::active("cancellation.profile-retained-bound-other")
                .expect("other cancellation");
        let other_cancellation_context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                deadline.clone(),
                &other_cancellation,
                observed_at,
            )
            .expect("other cancellation admission");
        let other_deadline_context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                Deadline::new(UtcMicros(201)).expect("other deadline"),
                &cancellation,
                observed_at,
            )
            .expect("other deadline admission");

        assert_eq!(context.request_id(), &request_id);
        assert_eq!(context.deadline(), &deadline);
        assert_eq!(context.cancellation(), &cancellation.context());
        assert_eq!(context.grant().issued_at, observed_at);
        assert_eq!(context.grant().expires_at, deadline.expires_at);
        assert_eq!(
            context.grant().allowed_capabilities,
            BTreeSet::from([operation.capability_id().clone()])
        );
        assert_eq!(
            context.grant().allowed_use_cases,
            BTreeSet::from([operation.use_case_id().clone()])
        );
        assert_ne!(context.grant().grant_id, other_request.grant().grant_id);
        assert_ne!(context.grant().digest, other_request.grant().digest);
        assert_ne!(
            context.grant().digest,
            other_cancellation_context.grant().digest
        );
        assert_ne!(
            context.grant().digest,
            other_deadline_context.grant().digest
        );
    }
}
