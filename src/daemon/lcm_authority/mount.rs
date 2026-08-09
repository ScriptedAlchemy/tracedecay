use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, RequestId,
};
use tracedecay_domain::{ActorId, UtcMicros};
use tracedecay_store::{StoreShardIdV1, StoreShardScopeV1};
use tracedecay_tool_catalog::CapabilityId;
use tracedecay_usecases::context::{
    CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets,
    ResolvedSessionIdentity, application_observed_at, session_application_grant_digest,
};
use tracedecay_usecases::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_usecases::session::SessionRequestBinding;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityInvocation, LcmAuthorityPort, LcmAuthorityRequest, LcmAuthorityResponse,
    LcmAuthorityTarget, lcm_authority_operation_identity,
};

use super::DaemonLcmAuthority;
use crate::global_db::RegisteredGlobalDb;

const LCM_ACTOR_ID: &str = "actor.daemon.lcm";
const LCM_GRANT_ID: &str = "grant.daemon.lcm";
const LCM_REQUEST_TIMEOUT: Duration = Duration::from_secs(115);
const LCM_MAX_RESULTS: u64 = 4_096;
const LCM_MAX_BYTES: u64 = 64 * 1024 * 1024;
const LCM_MAX_WORK_UNITS: u64 = 1_000_000;

pub(crate) type MountedLcmFuture<'a> =
    Pin<Box<dyn Future<Output = Option<LcmAuthorityResponse>> + Send + 'a>>;

/// Daemon-minted invocation boundary. Transport and host adapters can select
/// an operation but cannot supply scope, grants, deadlines, cancellation
/// identity, or a database handle.
pub(crate) trait MountedLcmAuthorityPort: Send + Sync {
    fn execute(&self, request: LcmAuthorityRequest) -> MountedLcmFuture<'_>;

    fn execute_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        request: LcmAuthorityRequest,
    ) -> MountedLcmFuture<'a>;
}

struct MountedLcmAuthority {
    authority: DaemonLcmAuthority,
    identity: ResolvedSessionIdentity,
}

impl MountedLcmAuthority {
    fn invocation(&self, request: LcmAuthorityRequest) -> Option<LcmAuthorityInvocation> {
        let operation = request.operation();
        let target = request.authority_target();
        let (capability, use_case) = lcm_authority_operation_identity(operation).ok()?;
        let request_id = mint_global_request_id(GlobalRequestSurface::LcmDaemon).ok()?;
        let request_id = RequestId::new(request_id.as_str()).ok()?;
        let actor = ActorId::new(LCM_ACTOR_ID).ok()?;
        let scope = self.identity.session_request_scope().ok()?;
        let (capability_digest, policy_digest, configuration_digest) =
            lcm_binding_digests(&self.identity, &capability, &target)?;
        let cancellation = CancellationToken::for_application_request(request_id.as_str());
        let budgets =
            RequestBudgets::new(LCM_MAX_RESULTS, LCM_MAX_BYTES, LCM_MAX_WORK_UNITS).ok()?;
        let observed_at = application_observed_at();
        let timeout_micros = i64::try_from(LCM_REQUEST_TIMEOUT.as_micros()).ok()?;
        let expires_at = UtcMicros(observed_at.0.checked_add(timeout_micros)?);
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(LCM_GRANT_ID).ok()?,
            1,
            session_application_grant_digest(
                capability_digest,
                policy_digest,
                configuration_digest,
                &cancellation,
                budgets,
            )
            .ok()?,
            actor.clone(),
            observed_at,
            expires_at,
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .ok()?;
        let context = RequestContext::new(
            actor,
            scope,
            grant,
            request_id,
            Deadline::new(expires_at).ok()?,
            CancellationContext::active(cancellation.application_token_id()?).ok()?,
        )
        .ok()?;
        let binding = SessionRequestBinding::new(
            self.identity.clone(),
            capability_digest,
            policy_digest,
            configuration_digest,
            cancellation.clone(),
            budgets,
        );
        Some(LcmAuthorityInvocation {
            context,
            binding,
            target,
            cancellation,
            request,
        })
    }
}

pub(super) fn lcm_binding_digests(
    identity: &ResolvedSessionIdentity,
    capability: &CapabilityId,
    target: &LcmAuthorityTarget,
) -> Option<(CapabilityDigest, PolicyDigest, ConfigurationDigest)> {
    let target_digest = tracedecay_domain::canonical_sha256(target).ok()?;
    Some((
        CapabilityDigest::new(lcm_binding_digest(
            b"tracedecay.daemon.lcm.capability.v1\0",
            identity,
            Some(capability.as_str()),
            &target_digest,
        )),
        PolicyDigest::new(lcm_binding_digest(
            b"tracedecay.daemon.lcm.policy.v1\0",
            identity,
            None,
            &target_digest,
        )),
        ConfigurationDigest::new(lcm_binding_digest(
            b"tracedecay.daemon.lcm.configuration.v1\0",
            identity,
            None,
            &target_digest,
        )),
    ))
}

pub(super) fn binding_matches_target(
    binding: &SessionRequestBinding,
    capability: &CapabilityId,
    target: &LcmAuthorityTarget,
) -> bool {
    lcm_binding_digests(binding.identity(), capability, target).is_some_and(
        |(capability_digest, policy_digest, configuration_digest)| {
            binding.capability_digest() == capability_digest
                && binding.policy_digest() == policy_digest
                && binding.configuration_digest() == configuration_digest
        },
    )
}

fn lcm_binding_digest(
    domain: &[u8],
    identity: &ResolvedSessionIdentity,
    operation: Option<&str>,
    target_digest: &tracedecay_domain::ManifestDigest,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(identity.profile_id().as_str().as_bytes());
    digest.update([0]);
    if let Some(project_id) = identity.project_id() {
        digest.update(project_id.as_str().as_bytes());
    }
    digest.update([0]);
    digest.update(identity.store_id().as_str().as_bytes());
    digest.update([0]);
    digest.update(identity.root_id().as_str().as_bytes());
    if let Some(route) = identity.git_route() {
        digest.update([0]);
        digest.update(route.repository_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.worktree_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.branch_id().as_str().as_bytes());
    }
    if let Some(operation) = operation {
        digest.update([0]);
        digest.update(operation.as_bytes());
    }
    digest.update([0]);
    digest.update(target_digest.as_str().as_bytes());
    digest.finalize().into()
}

impl MountedLcmAuthorityPort for MountedLcmAuthority {
    fn execute(&self, request: LcmAuthorityRequest) -> MountedLcmFuture<'_> {
        Box::pin(async move {
            let invocation = self.invocation(request)?;
            Some(self.authority.execute(invocation).await)
        })
    }

    fn execute_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        request: LcmAuthorityRequest,
    ) -> MountedLcmFuture<'a> {
        Box::pin(
            self.authority
                .execute_retained_read(context, cancellation, request),
        )
    }
}

fn identity_matches_shard(identity: &ResolvedSessionIdentity, shard: &StoreShardIdV1) -> bool {
    if identity.profile_id().as_str() != shard.profile_id.as_str() {
        return false;
    }
    match (&shard.scope, identity.project_id()) {
        (StoreShardScopeV1::ProfileSessions, None) => true,
        (StoreShardScopeV1::ProjectSessions { project_id }, Some(identity_project)) => {
            project_id.as_str() == identity_project.as_str()
        }
        _ => false,
    }
}

pub(crate) fn mount_registered_lcm_authority(
    database: Arc<RegisteredGlobalDb>,
    identity: ResolvedSessionIdentity,
    expected_shard: &StoreShardIdV1,
) -> Option<Arc<dyn MountedLcmAuthorityPort>> {
    if &database.binding().shard_id != expected_shard
        || !identity_matches_shard(&identity, expected_shard)
    {
        return None;
    }
    Some(Arc::new(MountedLcmAuthority {
        authority: DaemonLcmAuthority::registered(database),
        identity,
    }))
}
