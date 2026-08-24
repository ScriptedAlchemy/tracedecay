//! Pinned configuration authority for Work proposal routes.

use tracedecay_application::{
    RequestContext, ResolvedScope, WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1,
    WorkRoutingSnapshotV1,
};
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
};
use tracedecay_domain::{ManifestDigest, TaskId};

/// The project-open-pinned authority for one Work proposal's routing state.
///
/// Executable bindings authorize exact provider artifacts but do not declare
/// policy route candidates. This authority therefore publishes an empty route
/// set and lets policy record `NoEligibleRoutes`; deriving a candidate from an
/// executable capability would fabricate model, budget, and fitness evidence.
#[derive(Clone, Debug)]
pub(in crate::daemon) struct DaemonWorkProposalRoutingAuthorityV1 {
    scope: ResolvedScope,
    configuration_revision: ConfigurationRevisionId,
    configuration_snapshot: ConfigurationSnapshotId,
    configuration_digest: ManifestDigest,
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
        let Some(ConfigurationValueV1::WorkExecutableBindings(_)) =
            configuration_snapshot.effective_values.get(&binding_key)
        else {
            return Err(WorkRoutingSnapshotErrorV1::Unavailable);
        };
        Ok(Self {
            scope,
            configuration_revision,
            configuration_snapshot: configuration_snapshot.snapshot_id.clone(),
            configuration_digest: expected_configuration_digest.clone(),
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
        Ok(WorkRoutingSnapshotV1 {
            configuration_revision: Some(self.configuration_revision.clone()),
            eligible_routes: Vec::new(),
            budget: None,
            content_location: None,
            prior_outcomes: Vec::new(),
            human_override: None,
        })
    }
}
