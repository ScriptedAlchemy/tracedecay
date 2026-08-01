//! Minimal project-source access composition over existing authorities.

use std::collections::BTreeSet;

use tracedecay_application::{
    ApplicationOperation, AuthorizationRequest, RequestAdmission, RequestContext, ResolvedScope,
};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CapabilityResolutionContextV1, ConfigurationRevisionId,
    ConfigurationValueV1, SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey,
    SourceKindV1, resolve_restrictive_capabilities,
};
use tracedecay_domain::{ActorId, CapabilityId as DomainCapabilityId, ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::CapabilityId;

use super::configuration::ConfigurationControlStore;

/// Non-disclosing denial returned for missing, stale, ambiguous, or denied
/// project-source access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSourceAccessDenial {
    NotFoundOrNotAuthorized,
}

/// Resolution result for one daemon-authenticated project source route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSourceAccessOutcome {
    Allowed(Box<ProjectSourceAccessSnapshot>),
    Denied(ProjectSourceAccessDenial),
}

/// Immutable, non-secret evidence for one admitted project source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSourceAccessSnapshot {
    pub scope: ResolvedScope,
    pub requester: ActorId,
    pub binding: ScopeSourceBinding,
    pub configuration_revision: ConfigurationRevisionId,
    pub configuration_digest: ManifestDigest,
    pub configuration_provenance_digest: ManifestDigest,
    pub effective_capabilities: BTreeSet<CapabilityId>,
    pub grant_expires_at: UtcMicros,
}

impl ProjectSourceAccessSnapshot {
    pub fn allows(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> bool {
        context.validate().is_ok()
            && context.scope() == &self.scope
            && context.actor() == &self.requester
            && observed_at < self.grant_expires_at
            && context.allows(operation.capability_id(), operation.use_case_id())
            && self
                .effective_capabilities
                .contains(operation.capability_id())
    }
}

/// Resolves one exact host-source binding using only the current Plan 20
/// snapshot and the daemon-authenticated request grant.
///
/// Configuration read failures, malformed snapshots, missing or ambiguous
/// bindings/rules, expired route grants, and denied capabilities all collapse
/// to the public non-disclosing denial. This adapter creates no grant, policy,
/// default, or persistence authority.
pub async fn project_source_access_snapshot_for_request(
    configuration: &dyn ConfigurationControlStore,
    request: &AuthorizationRequest<'_>,
    source_kind: SourceKindV1,
) -> ProjectSourceAccessOutcome {
    if request.context.validate().is_err()
        || request.context.admission_at(request.observed_at) != RequestAdmission::Admitted
        || !request.context.allows(
            request.operation.capability_id(),
            request.operation.use_case_id(),
        )
    {
        return denied();
    }

    let Ok(current) = configuration.current().await else {
        return denied();
    };
    if current.snapshot.validate().is_err() {
        return denied();
    }

    let Ok(bindings_key) = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY) else {
        return denied();
    };
    let Some(ConfigurationValueV1::SourceBindings(bindings)) =
        current.snapshot.effective_values.get(&bindings_key)
    else {
        return denied();
    };
    let authority = AuthorityRef::Project(request.context.scope().project_id.clone());
    let mut matching_bindings = bindings
        .iter()
        .filter(|binding| binding.source_kind == source_kind && binding.authority == authority);
    let Some(binding) = matching_bindings.next().cloned() else {
        return denied();
    };
    if matching_bindings.next().is_some() {
        return denied();
    }

    let Ok(access_rules_key) = SettingKey::new(ACCESS_RULES_SETTING_KEY) else {
        return denied();
    };
    let Some(ConfigurationValueV1::AccessRules(access_rules)) =
        current.snapshot.effective_values.get(&access_rules_key)
    else {
        return denied();
    };
    let resolution_context = CapabilityResolutionContextV1 {
        actor: request.context.actor().clone(),
        operation: None,
        source_kind,
        authority,
        evaluated_at: request.observed_at,
    };
    let Ok(granted_capabilities) = request
        .context
        .grant()
        .allowed_capabilities
        .iter()
        .map(|capability| DomainCapabilityId::new(capability.as_str().to_owned()))
        .collect::<Result<BTreeSet<_>, _>>()
    else {
        return denied();
    };
    let Ok(resolution) =
        resolve_restrictive_capabilities(granted_capabilities, access_rules, &resolution_context)
    else {
        return denied();
    };
    let Ok(requested_capability) =
        DomainCapabilityId::new(request.operation.capability_id().as_str().to_owned())
    else {
        return denied();
    };
    if !resolution.effective.contains(&requested_capability) {
        return denied();
    }
    let Ok(effective_capabilities) = resolution
        .effective
        .into_iter()
        .map(|capability| CapabilityId::new(capability.as_str().to_owned()))
        .collect::<Result<BTreeSet<_>, _>>()
    else {
        return denied();
    };

    ProjectSourceAccessOutcome::Allowed(Box::new(ProjectSourceAccessSnapshot {
        scope: request.context.scope().clone(),
        requester: request.context.actor().clone(),
        binding,
        configuration_revision: current.revision_id,
        configuration_digest: current.snapshot.effective_behavior_digest,
        configuration_provenance_digest: current.snapshot.resolution_provenance_digest,
        effective_capabilities,
        grant_expires_at: request.context.grant().expires_at,
    }))
}

fn denied() -> ProjectSourceAccessOutcome {
    ProjectSourceAccessOutcome::Denied(ProjectSourceAccessDenial::NotFoundOrNotAuthorized)
}
