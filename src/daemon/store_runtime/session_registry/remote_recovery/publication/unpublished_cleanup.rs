use std::sync::Arc;

use tracedecay_domain::ProjectId;

use super::{RegisteredGlobalDbLeaseV1, RemoteRecoveryPublicationContextV1, Result};
use crate::daemon::store_runtime::session_registry::session_registry_error;

impl RemoteRecoveryPublicationContextV1 {
    pub(super) async fn rebind_session_sync(
        &self,
        project_id: &ProjectId,
        database: &RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        let service = self.session_sync_service("rebind project session sync")?;
        service
            .rebind_project(self.identity.profile_id(), project_id, database)
            .await
            .map(|_| ())
            .map_err(|error| session_registry_error("rebind project session sync", error))
    }

    pub(super) async fn retire_session_sync(&self, project_id: &ProjectId) -> Result<()> {
        let service = self.session_sync_service("retire project session sync")?;
        service
            .retire_project(self.identity.profile_id(), project_id)
            .await
            .map(|_| ())
            .map_err(|error| session_registry_error("retire project session sync", error))
    }

    pub(super) async fn retire_unpublished_mounted(
        &self,
        project_id: &ProjectId,
        database: &RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        let mut failures = Vec::new();
        if let Err(error) = self.retire_session_sync(project_id).await {
            failures.push(format!("session_sync={error}"));
        }
        if let Err(error) = self
            .replay
            .unregister_target(project_id, database.binding())
        {
            failures.push(format!("replay={error}"));
        }
        match database.session_relation_graph_identity() {
            Ok((binding, locator)) => {
                if let Err(error) =
                    super::super::super::code_graph::graph_attachment::close_retained(
                        &self.graph_registry,
                        binding.clone(),
                        locator.clone(),
                    )
                    .await
                {
                    failures.push(format!("relation_graph={error}"));
                }
            }
            Err(error) => failures.push(format!("relation_graph_identity={error}")),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(session_registry_error(
                "retire unpublished restored project sessions",
                failures.join("; "),
            ))
        }
    }
}
