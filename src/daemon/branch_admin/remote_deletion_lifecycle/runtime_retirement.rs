use crate::errors::Result;
use std::path::Path;

use super::super::{StoreAdministration, remote_recovery_lifecycle};

impl StoreAdministration {
    pub(super) async fn remote_deleted_project_roots(
        &self,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<std::collections::BTreeSet<std::path::PathBuf>> {
        remote_recovery_lifecycle::project_roots(
            database,
            &self.project_servers,
            profile_root,
            project_id,
        )
        .await
    }

    pub(super) async fn retire_remote_deleted_project_work(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<()> {
        remote_recovery_lifecycle::retire_runtime_work(
            &self.project_servers,
            &self.session_temporal_refresh_schedulers,
            #[cfg(unix)]
            &self.automation_schedulers,
            &self.project_server_retirements,
            profile_root,
            project_id,
            None,
        )
        .await
    }
}
