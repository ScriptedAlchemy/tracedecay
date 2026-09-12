use std::sync::Arc;

use tracedecay_domain::{BrainId, ObservationScopeV1, ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::{HostAdmissionOutcome, HostAdmissionScope};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_store::StoreShardScopeV1;

#[derive(Clone, Default)]
pub struct HostAdmissionAuthorities<'a> {
    pub(crate) project_id: Option<ProjectId>,
    project_registered: Option<&'a RegisteredGlobalDb>,
    brain_id: Option<BrainId>,
    profile_id: Option<UserProfileId>,
    profile_registered: Option<&'a RegisteredGlobalDb>,
    pub(crate) repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
    /// The process background CPU authority observation-capture preparation
    /// is admitted through. The composition root injects the one authority its
    /// worker plan installed; capture without it is refused as
    /// `background_cpu_unavailable`.
    pub(crate) background_cpu: Option<Arc<ProcessBackgroundCpuV1>>,
}

impl<'a> HostAdmissionAuthorities<'a> {
    pub fn registered_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: Some(registered),
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
            background_cpu: None,
        }
    }

    pub(crate) fn registered_for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: Some(registered),
            repository_provenance: None,
            background_cpu: None,
        }
    }

    /// Mounts the process background CPU authority that observation-capture
    /// preparation runs under.
    #[must_use]
    pub fn with_background_cpu(mut self, background_cpu: Arc<ProcessBackgroundCpuV1>) -> Self {
        self.background_cpu = Some(background_cpu);
        self
    }

    pub fn for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_project(brain_id, profile_id, project_id, registered)
    }

    pub fn for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_profile(brain_id, profile_id, registered)
    }

    /// Adds the registered profile-session authority to project admission.
    #[must_use]
    pub fn with_profile_registered(
        mut self,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        self.profile_id = Some(profile_id);
        self.profile_registered = Some(registered);
        self
    }

    /// Admission bound to a project identity with **no** registered database
    /// and no resolved profile identity behind it.
    ///
    /// Standalone callers (a CLI invocation with no daemon-owned registry
    /// mount) still need an admission handle to walk a transcript and count
    /// what it *would* admit. Every capture fails closed; only scope
    /// validation against `project_id` is authoritative.
    pub fn unregistered_for_project(project_id: ProjectId) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: None,
            brain_id: None,
            profile_id: None,
            profile_registered: None,
            repository_provenance: None,
            background_cpu: None,
        }
    }

    /// Profile-scoped counterpart of [`Self::unregistered_for_project`].
    #[must_use]
    pub const fn unregistered_for_profile() -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: None,
            profile_id: None,
            profile_registered: None,
            repository_provenance: None,
            background_cpu: None,
        }
    }

    pub fn unavailable_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
            background_cpu: None,
        }
    }

    pub fn unavailable_for_profile(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
            background_cpu: None,
        }
    }

    #[must_use]
    pub fn with_repository_provenance(
        mut self,
        repository_provenance: RepositoryProvenanceAdmissionContext,
    ) -> Self {
        self.repository_provenance = Some(repository_provenance);
        self
    }

    pub(crate) fn registered_database(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<Option<&'a RegisteredGlobalDb>, HostAdmissionOutcome> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_registered,
            HostAdmissionScope::Profile => self.profile_registered,
        };
        let Some(database) = database else {
            return Ok(None);
        };
        let shard = &database.binding().shard_id;
        let profile_matches = self.brain_id.as_ref() == Some(&shard.brain_id)
            && self.profile_id.as_ref() == Some(&shard.profile_id);
        let valid = profile_matches
            && match (scope, &shard.scope) {
                (
                    HostAdmissionScope::Project,
                    StoreShardScopeV1::ProjectSessions { project_id },
                ) => self.project_id.as_ref() == Some(project_id),
                (HostAdmissionScope::Profile, StoreShardScopeV1::ProfileSessions) => true,
                _ => false,
            };
        if valid {
            Ok(Some(database))
        } else {
            Err(HostAdmissionOutcome::project_authority_mismatch())
        }
    }

    pub(crate) fn validate_scope(&self, scope: &ObservationScopeV1) -> Result<(), HostAdmissionOutcome> {
        let ObservationScopeV1::Project { project_id } = scope else {
            return Ok(());
        };
        match self.project_id.as_ref() {
            Some(expected) if expected == project_id => Ok(()),
            Some(_) => Err(HostAdmissionOutcome::project_authority_mismatch()),
            None => Err(HostAdmissionOutcome::project_authority_unbound()),
        }
    }
}
