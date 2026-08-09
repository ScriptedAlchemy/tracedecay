//! Profile-scoped retained admission and direct typed memory/LCM execution.

use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::retained_surfaces::{
    RetainedSurfaceOperation, RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, RetainedSurfacePortsV1, RetainedSurfaceServiceV1, RetryDirective,
    SafeDiagnostic, now_micros,
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

const PROFILE_RETAINED_POLICY_REVISION_V1: u64 = 1;
const PROFILE_RETAINED_ACTOR_DOMAIN_V1: &str =
    "tracedecay.daemon.profile-retained.authenticated-actor.v1";
const PROFILE_RETAINED_OPERATIONS_V1: [RetainedSurfaceOperation; 20] = [
    RetainedSurfaceOperation::FactStoreAdd,
    RetainedSurfaceOperation::FactStoreSearch,
    RetainedSurfaceOperation::FactStoreProbe,
    RetainedSurfaceOperation::FactStoreRelated,
    RetainedSurfaceOperation::FactStoreReason,
    RetainedSurfaceOperation::FactStoreContradict,
    RetainedSurfaceOperation::FactStoreGet,
    RetainedSurfaceOperation::FactStoreUpdate,
    RetainedSurfaceOperation::FactStoreRemove,
    RetainedSurfaceOperation::FactStoreList,
    RetainedSurfaceOperation::FactFeedback,
    RetainedSurfaceOperation::MemoryStatus,
    RetainedSurfaceOperation::MessageSearch,
    RetainedSurfaceOperation::LcmStatus,
    RetainedSurfaceOperation::LcmDoctor,
    RetainedSurfaceOperation::LcmLoadSession,
    RetainedSurfaceOperation::LcmGrep,
    RetainedSurfaceOperation::LcmDescribe,
    RetainedSurfaceOperation::LcmExpand,
    RetainedSurfaceOperation::LcmExpandQuery,
];

/// Policy admission retained for the lifetime of a mounted profile connection.
/// Request handlers may narrow its deadline but never issue or widen this grant.
#[derive(Clone)]
pub(crate) struct ProfileRetainedAdmissionV1 {
    pub(crate) user_profile_id: UserProfileId,
    pub(crate) actor: ActorId,
    pub(crate) grant: CapabilityGrantSnapshot,
    session_identity: ResolvedSessionIdentity,
    configuration_digest: ManifestDigest,
}

impl ProfileRetainedAdmissionV1 {
    pub(crate) fn session_identity(&self) -> &ResolvedSessionIdentity {
        &self.session_identity
    }

    pub(crate) fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }
}

pub(crate) fn profile_retained_configuration_digest(
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

pub(crate) fn issue_profile_retained_policy_admission(
    brain_id: &BrainId,
    user_profile_id: UserProfileId,
    session_identity: &ResolvedSessionIdentity,
    configuration_digest: &ManifestDigest,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
) -> Result<ProfileRetainedAdmissionV1, TraceDecayError> {
    if session_identity.project_id().is_some()
        || session_identity.profile_id().as_str() != user_profile_id.as_str()
    {
        return Err(TraceDecayError::Config {
            message:
                "profile retained policy admission requires the exact profile session identity"
                    .to_owned(),
        });
    }
    let scope =
        session_identity
            .session_request_scope()
            .map_err(|error| TraceDecayError::Config {
                message: format!("profile retained policy scope is invalid: {error}"),
            })?;
    let operations = PROFILE_RETAINED_OPERATIONS_V1
        .into_iter()
        .map(tracedecay_application::retained_surface_application_operation)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained policy catalog is invalid: {error}"),
        })?;
    let capabilities = operations
        .iter()
        .map(|operation| operation.capability_id().clone())
        .collect::<BTreeSet<_>>();
    let use_cases = operations
        .iter()
        .map(|operation| operation.use_case_id().clone())
        .collect::<BTreeSet<_>>();
    let actor_digest =
        canonical_sha256(&(PROFILE_RETAINED_ACTOR_DOMAIN_V1, brain_id, &user_profile_id)).map_err(
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
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.profile-retained.policy-grant.v1",
        brain_id,
        &user_profile_id,
        &scope,
        &actor,
        configuration_digest,
        &capabilities,
        &use_cases,
        issued_at,
        expires_at,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained policy grant digest failed: {error}"),
    })?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.profile-open.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained policy grant identity is invalid: {error}"),
        })?,
        PROFILE_RETAINED_POLICY_REVISION_V1,
        grant_digest,
        actor.clone(),
        issued_at,
        expires_at,
        scope,
        capabilities,
        use_cases,
        DisclosureClass::Sensitive,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained policy grant is invalid: {error}"),
    })?;
    Ok(ProfileRetainedAdmissionV1 {
        user_profile_id,
        actor,
        grant,
        session_identity: session_identity.clone(),
        configuration_digest: configuration_digest.clone(),
    })
}

/// Execute one profile-scoped retained request through canonical admission and
/// render the typed application result only after execution has completed.
pub(crate) async fn execute_profile_retained_application(
    authorities: ProfileRetainedAuthoritiesV1<'_>,
    admission: &ProfileRetainedAdmissionV1,
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
    let deadline = Deadline {
        expires_at: UtcMicros(deadline.expires_at.0.min(admission.grant.expires_at.0)),
    };
    if deadline.is_elapsed_at(observed_at) {
        return Ok(Err(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        )));
    }
    if admission.user_profile_id.as_str() != authorities.session_identity.profile_id().as_str()
        || admission.grant.scope != scope
        || admission.grant.issuer != admission.actor
        || !admission
            .grant
            .allowed_capabilities
            .contains(operation.capability_id())
        || !admission
            .grant
            .allowed_use_cases
            .contains(operation.use_case_id())
    {
        return Ok(Err(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )));
    }
    if admission.configuration_digest != authorities.configuration_digest {
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
    let context = RequestContext::new(
        admission.actor.clone(),
        scope.clone(),
        admission.grant.clone(),
        request_id.clone(),
        deadline,
        cancellation.context(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;
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

    fn admission(
        identity: &ResolvedSessionIdentity,
        configuration_digest: &ManifestDigest,
    ) -> ProfileRetainedAdmissionV1 {
        issue_profile_retained_policy_admission(
            &BrainId::new("brain.profile-retained-test").expect("brain id"),
            UserProfileId::new(identity.profile_id().as_str()).expect("user profile id"),
            identity,
            configuration_digest,
            UtcMicros(1),
            UtcMicros(i64::MAX),
        )
        .expect("profile admission")
    }

    async fn denied_kind(
        authorities: ProfileRetainedAuthoritiesV1<'_>,
        admission: &ProfileRetainedAdmissionV1,
        request_id: &str,
    ) -> ApplicationProblemKind {
        let cancellation =
            CancellationSignal::active(format!("cancellation.{request_id}")).expect("cancellation");
        execute_profile_retained_application(
            authorities,
            admission,
            request(),
            RequestId::new(request_id).expect("request id"),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
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
        let configuration_digest = digest('a');
        let admission = admission(&admitted_identity, &configuration_digest);
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity: requested_identity,
                configuration_digest,
                lcm_authority: None,
            },
            &admission,
            "request.profile-retained-scope-denial",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::NotFoundOrNotAuthorized);
    }

    #[tokio::test]
    async fn connection_admission_reports_changed_configuration_as_stale() {
        let session_identity = identity("profile.retained-stale");
        let admission = admission(&session_identity, &digest('b'));
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity,
                configuration_digest: digest('c'),
                lcm_authority: None,
            },
            &admission,
            "request.profile-retained-stale-configuration",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::Stale);
    }
}
