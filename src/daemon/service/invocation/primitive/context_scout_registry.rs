use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{ProjectId, UserProfileId};

use super::super::{DaemonInvocationService, InvocationProjectRuntimeIdentityV1};
use crate::agents::context_scout_ports::ProjectContextScoutAddressRegistryV1;
use crate::db::Database;

#[derive(Debug, Error)]
pub(crate) enum DaemonContextScoutRuntimeRegistrationError {
    #[error("a Context Scout address registry is already mounted for this project identity")]
    AlreadyRegistered,
    #[error("the Context Scout address registry could not be opened")]
    InvalidProjectIdentity,
}

#[derive(Clone)]
pub(crate) struct DaemonContextScoutRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonContextScoutRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        profile_id: UserProfileId,
        project_id: ProjectId,
        project_root: PathBuf,
    ) -> Result<Arc<ProjectContextScoutAddressRegistryV1>, DaemonContextScoutRuntimeRegistrationError>
    {
        let Some(registry) =
            ProjectContextScoutAddressRegistryV1::new(database, project_id.clone())
        else {
            return Err(DaemonContextScoutRuntimeRegistrationError::InvalidProjectIdentity);
        };
        let key = InvocationProjectRuntimeIdentityV1::new(profile_id, project_id, project_root);
        let mut registries = self.service.context_scout_registries.lock().await;
        if registries.contains_key(&key) {
            return Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered);
        }
        registries.insert(key, Arc::clone(&registry));
        Ok(registry)
    }

    pub(crate) async fn get(
        &self,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        project_root: &Path,
    ) -> Option<Arc<ProjectContextScoutAddressRegistryV1>> {
        let key = InvocationProjectRuntimeIdentityV1::new(
            profile_id.clone(),
            project_id.clone(),
            project_root.to_path_buf(),
        );
        self.service
            .context_scout_registries
            .lock()
            .await
            .get(&key)
            .cloned()
    }
}
