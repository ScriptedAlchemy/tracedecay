//! Pinned configuration authority for Work proposal routes.

use tracedecay_application::{
    RequestContext, ResolvedScope, WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1,
    WorkRoutingSnapshotV1,
};
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkProposalRouteProfileV1,
};
use tracedecay_domain::{ManifestDigest, TaskId, WorkExecutableReference};
use tracedecay_tool_catalog::CapabilityId;

use crate::config::work_executable_binding::{
    PinnedWorkExecutableBindingResolver, WorkExecutableBindingError, WorkExecutableBindingResolver,
};

#[derive(Clone, Debug)]
struct DeclaredProposalRouteV1 {
    executable: WorkExecutableReference,
    profile: WorkProposalRouteProfileV1,
}

/// The project-open-pinned authority that turns configured route declarations
/// into the immutable input for one Work proposal.
///
/// It never discovers executables. Every candidate originates in the exact
/// `work.executable_bindings.v1` configuration value, must remain authorized
/// by the request grant, and has its declared executable verified before it is
/// exposed to policy.
#[derive(Clone, Debug)]
pub(in crate::daemon) struct DaemonWorkProposalRoutingAuthorityV1 {
    scope: ResolvedScope,
    configuration_revision: ConfigurationRevisionId,
    configuration_snapshot: ConfigurationSnapshotId,
    configuration_digest: ManifestDigest,
    executable_resolver: PinnedWorkExecutableBindingResolver,
    routes: Vec<DeclaredProposalRouteV1>,
}

impl DaemonWorkProposalRoutingAuthorityV1 {
    pub(in crate::daemon) fn mount(
        scope: ResolvedScope,
        configuration_revision: ConfigurationRevisionId,
        configuration_snapshot: &tracedecay_domain::configuration::ConfigurationSnapshotV1,
        expected_configuration_digest: &ManifestDigest,
    ) -> Result<Self, WorkRoutingSnapshotErrorV1> {
        if configuration_snapshot.validate().is_err()
            || &configuration_snapshot.effective_behavior_digest != expected_configuration_digest
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
        let executable_resolver = PinnedWorkExecutableBindingResolver::from_snapshot(
            &configuration_revision,
            configuration_snapshot,
        )
        .map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
        let routes = bindings
            .iter()
            .flat_map(|binding| {
                binding
                    .routing_profiles()
                    .iter()
                    .cloned()
                    .map(move |profile| DeclaredProposalRouteV1 {
                        executable: binding.executable().clone(),
                        profile,
                    })
            })
            .collect();
        Ok(Self {
            scope,
            configuration_revision,
            configuration_snapshot: configuration_snapshot.snapshot_id.clone(),
            configuration_digest: expected_configuration_digest.clone(),
            executable_resolver,
            routes,
        })
    }

    pub(super) fn same_configuration_as(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.configuration_revision == other.configuration_revision
            && self.configuration_snapshot == other.configuration_snapshot
            && self.configuration_digest == other.configuration_digest
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
        if context.validate().is_err() || context.scope() != &self.scope {
            return Err(WorkRoutingSnapshotErrorV1::NotFoundOrNotAuthorized);
        }
        let mut authorized_route_exists = false;
        let mut eligible_routes = Vec::with_capacity(self.routes.len());
        let mut verified_executable_capabilities = Vec::new();
        for declared in &self.routes {
            let capability = CapabilityId::new(
                declared
                    .profile
                    .executable_capability()
                    .provider_capability_id(),
            )
            .map_err(|_| WorkRoutingSnapshotErrorV1::Unavailable)?;
            if !context.grant().allowed_capabilities.contains(&capability) {
                continue;
            }
            authorized_route_exists = true;
            let executable_capability = declared.profile.executable_capability();
            if !verified_executable_capabilities.iter().any(
                |(verified_executable, verified_capability)| {
                    verified_executable == &declared.executable
                        && *verified_capability == executable_capability
                },
            ) {
                self.executable_resolver
                    .resolve(
                        &declared.executable,
                        executable_capability.backend(),
                        executable_capability.protocol(),
                    )
                    .map_err(route_availability_problem)?;
                verified_executable_capabilities
                    .push((declared.executable.clone(), executable_capability));
            }
            eligible_routes.push(declared.profile.candidate());
        }
        if !self.routes.is_empty() && !authorized_route_exists {
            return Err(WorkRoutingSnapshotErrorV1::NotFoundOrNotAuthorized);
        }
        Ok(WorkRoutingSnapshotV1 {
            configuration_revision: Some(self.configuration_revision.clone()),
            eligible_routes,
            budget: None,
            content_location: None,
            prior_outcomes: Vec::new(),
            human_override: None,
        })
    }
}

fn route_availability_problem(_error: WorkExecutableBindingError) -> WorkRoutingSnapshotErrorV1 {
    WorkRoutingSnapshotErrorV1::Unavailable
}
