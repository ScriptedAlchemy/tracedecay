//! [`ProjectStoreRuntimeV1`] implementor for the daemon session registry.

use std::path::{Path, PathBuf};

use tracedecay_domain::{ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use tracedecay_usecases::tracedecay::{ProjectStoreRuntimeV1, RuntimeFuture};

use super::DaemonSessionRuntimeRegistryV1;

impl ProjectStoreRuntimeV1 for DaemonSessionRuntimeRegistryV1 {
    fn profile_id(&self) -> &UserProfileId {
        DaemonSessionRuntimeRegistryV1::profile_id(self)
    }

    fn profile_database(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1> {
        Box::pin(DaemonSessionRuntimeRegistryV1::profile_database(self))
    }

    fn project_sessions(
        &self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1> {
        Box::pin(DaemonSessionRuntimeRegistryV1::project_sessions(
            self, project_id, roots,
        ))
    }

    fn project_graph(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        db_path: PathBuf,
        authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> RuntimeFuture<'_, Database> {
        let project_root = project_root.to_path_buf();
        Box::pin(async move {
            DaemonSessionRuntimeRegistryV1::project_graph(
                self,
                &project_root,
                project_id,
                db_path,
                authority,
                access,
            )
            .await
        })
    }

    fn project_graph_registered(
        &self,
        project_id: ProjectId,
        db_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> RuntimeFuture<'_, Database> {
        Box::pin(DaemonSessionRuntimeRegistryV1::project_graph_registered(
            self, project_id, db_path, access,
        ))
    }
}
