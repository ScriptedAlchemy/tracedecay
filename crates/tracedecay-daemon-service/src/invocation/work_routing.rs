//! Pinned configuration authority for Work proposal routes.

use tracedecay_configuration::config::PinnedRuntimeConfiguration;
use tracedecay_configuration::config::work_executable_binding::{
    PinnedWorkExecutableBindingResolver, WorkExecutableBindingResolver,
};
use tracedecay_contracts::{
    CapabilityGrantSnapshot, RequestContext, ResolvedScope, WORK_APPLICATION_OPERATION_IDS_V1,
    WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1,
};
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
};
use tracedecay_domain::{ManifestDigest, TaskId, WorkRouteCandidateV1};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

/// The project-open-pinned authority for one Work proposal's routing state.
///
/// Routes are explicit configuration facts. Mount verifies the exact pinned
/// executable for every declared route before any request can observe it.
#[derive(Clone, Debug)]
pub struct DaemonWorkProposalRoutingAuthorityV1 {
    scope: ResolvedScope,
    configuration_revision: ConfigurationRevisionId,
    configuration_snapshot: ConfigurationSnapshotId,
    configuration_digest: ManifestDigest,
    grant_digest: ManifestDigest,
    generate_proposal_capability: CapabilityId,
    generate_proposal_use_case: UseCaseId,
    eligible_routes: Vec<WorkRouteCandidateV1>,
}

impl DaemonWorkProposalRoutingAuthorityV1 {
    pub fn mount(
        scope: ResolvedScope,
        configuration: &PinnedRuntimeConfiguration,
        expected_configuration_digest: &ManifestDigest,
        grant: &CapabilityGrantSnapshot,
    ) -> Result<Self, WorkRoutingSnapshotErrorV1> {
        let configuration_snapshot = configuration.snapshot();
        if configuration_snapshot.validate().is_err()
            || &configuration_snapshot.effective_behavior_digest != expected_configuration_digest
            || grant.validate().is_err()
            || grant.scope != scope
        {
            return Err(WorkRoutingSnapshotErrorV1::Unavailable);
        }
        let (_, capability, use_case) = WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .find(|(operation, _, _)| *operation == "generate_proposal")
            .ok_or(WorkRoutingSnapshotErrorV1::Unavailable)?;
        let generate_proposal_capability =
            CapabilityId::new(*capability).map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
        let generate_proposal_use_case =
            UseCaseId::new(*use_case).map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
        if !grant
            .allowed_capabilities
            .contains(&generate_proposal_capability)
            || !grant
                .allowed_use_cases
                .contains(&generate_proposal_use_case)
        {
            return Err(WorkRoutingSnapshotErrorV1::Unavailable);
        }
        let binding_key = SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY)
            .map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
        let Some(ConfigurationValueV1::WorkExecutableBindings(bindings)) =
            configuration_snapshot.effective_values.get(&binding_key)
        else {
            return Err(WorkRoutingSnapshotErrorV1::Unavailable);
        };
        let resolver = PinnedWorkExecutableBindingResolver::from_configuration(configuration)
            .map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
        let mut eligible_routes = Vec::new();
        for binding in bindings {
            for route in binding.routes() {
                let capability = binding
                    .capabilities()
                    .iter()
                    .copied()
                    .find(|candidate| {
                        candidate.provider_id().as_str() == route.provider_capability_id
                    })
                    .ok_or(WorkRoutingSnapshotErrorV1::Unavailable)?;
                resolver
                    .resolve(
                        binding.executable(),
                        capability.backend(),
                        capability.protocol(),
                    )
                    .map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
                eligible_routes.push(route.clone());
            }
        }
        Ok(Self {
            scope,
            configuration_revision: configuration.revision_id().clone(),
            configuration_snapshot: configuration_snapshot.snapshot_id.clone(),
            configuration_digest: expected_configuration_digest.clone(),
            grant_digest: grant.digest.clone(),
            generate_proposal_capability,
            generate_proposal_use_case,
            eligible_routes,
        })
    }

    pub(super) fn same_configuration_as(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.configuration_revision == other.configuration_revision
            && self.configuration_snapshot == other.configuration_snapshot
            && self.configuration_digest == other.configuration_digest
            && self.grant_digest == other.grant_digest
            && self.eligible_routes == other.eligible_routes
    }

    pub(super) fn matches_scope(&self, scope: &ResolvedScope) -> bool {
        &self.scope == scope
    }

    pub(super) fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub(super) fn configuration_revision(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision
    }
}

impl WorkRoutingSnapshotPortV1 for DaemonWorkProposalRoutingAuthorityV1 {
    fn routing_snapshot(
        &self,
        context: &RequestContext,
        _task_id: &TaskId,
    ) -> Result<WorkRoutingSnapshotV1, WorkRoutingSnapshotErrorV1> {
        if context.validate().is_err()
            || context.scope() != &self.scope
            || &context.grant().digest != &self.grant_digest
            || !context.allows(
                &self.generate_proposal_capability,
                &self.generate_proposal_use_case,
            )
        {
            return Err(WorkRoutingSnapshotErrorV1::NotFoundOrNotAuthorized);
        }
        Ok(WorkRoutingSnapshotV1 {
            configuration_revision: Some(self.configuration_revision.clone()),
            eligible_routes: self.eligible_routes.clone(),
            budget: None,
            content_location: None,
            prior_outcomes: Vec::new(),
            human_override: None,
        })
    }
}
