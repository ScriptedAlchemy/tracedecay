use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1};

use super::{RegisteredGlobalDb, registered_error};
use tracedecay_session_temporal_store::relations::{
    SessionRelationGraphStore, SessionRelationScope,
};

impl RegisteredGlobalDb {
    /// Mounts the daemon-owned native graph handle for this exact session
    /// shard. Rebinding is accepted only for the same identity and allocation.
    pub fn bind_session_relation_graph(
        &self,
        scope: SessionRelationScope,
        graph: tracedecay_graph_db::GraphDbLeaseV1,
        graph_binding: StoreRuntimeBindingV1,
        graph_verified_locator: VerifiedStoreLocatorV1,
    ) -> tracedecay_domain::errors::Result<()> {
        let shard = &self.binding().shard_id;
        let exact = match (&shard.scope, &scope) {
            (
                StoreShardScopeV1::ProjectSessions {
                    project_id: expected,
                },
                SessionRelationScope::ProjectSessions { project_id: actual },
            ) => expected == actual,
            (
                StoreShardScopeV1::ProfileSessions,
                SessionRelationScope::ProfileSessions { profile_id },
            ) => &shard.profile_id == profile_id,
            _ => false,
        };
        if !exact
            || &graph_binding != self.binding()
            || graph_verified_locator.shard_id != graph_binding.shard_id
            || graph_verified_locator.incarnation != graph_binding.incarnation
        {
            return Err(registered_error(
                "bind session relation graph",
                "graph scope or exact graph authority does not match the registered session shard",
            ));
        }
        let mut mounted = self
            .session_relation_graph
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((existing_scope, _, existing_binding, existing_locator)) = mounted.as_ref()
            && (existing_scope != &scope
                || existing_binding != &graph_binding
                || existing_locator != &graph_verified_locator)
        {
            return Err(registered_error(
                "bind session relation graph",
                "registered session shard already has a different graph owner",
            ));
        }
        *mounted = Some((scope, graph, graph_binding, graph_verified_locator));
        Ok(())
    }

    pub(crate) fn session_relation_graph(
        &self,
    ) -> tracedecay_domain::errors::Result<(
        SessionRelationScope,
        tracedecay_graph_db::GraphDbLeaseV1,
        StoreRuntimeBindingV1,
        VerifiedStoreLocatorV1,
    )> {
        self.session_relation_graph
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                registered_error(
                    "resolve session relation graph",
                    "daemon-owned session relation graph is unavailable",
                )
            })
    }

    pub fn session_relation_graph_identity(
        &self,
    ) -> tracedecay_domain::errors::Result<(StoreRuntimeBindingV1, VerifiedStoreLocatorV1)> {
        let (_, _, binding, locator) = self.session_relation_graph()?;
        Ok((binding, locator))
    }

    pub fn session_relation_store(
        &self,
    ) -> tracedecay_domain::errors::Result<(SessionRelationScope, SessionRelationGraphStore)> {
        let (scope, graph, _, _) = self.session_relation_graph()?;
        Ok((scope, SessionRelationGraphStore::new(graph)))
    }

    /// Shared-crate graph lease so dependents (including this crate's
    /// `cfg(test)` unit-test identity) can reconstruct a same-crate
    /// [`SessionRelationGraphStore`] without crossing crate versions.
    pub fn session_relation_graph_lease(
        &self,
    ) -> tracedecay_domain::errors::Result<tracedecay_graph_db::GraphDbLeaseV1> {
        let (_, graph, _, _) = self.session_relation_graph()?;
        Ok(graph)
    }
}
