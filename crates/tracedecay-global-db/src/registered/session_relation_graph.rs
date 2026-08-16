use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1};

use super::{RegisteredGlobalDb, registered_error};
use crate::session_temporal::relations::{SessionRelationGraphStore, SessionRelationScope};

impl RegisteredGlobalDb {
    /// Mounts the daemon-owned native graph handle for this exact session
    /// shard. Rebinding is accepted only for the same identity and allocation.
    pub fn bind_session_relation_graph(
        &self,
        scope: SessionRelationScope,
        graph: tracedecay_graph_db::GraphDbLeaseV1,
        graph_binding: StoreRuntimeBindingV1,
        graph_verified_locator: VerifiedStoreLocatorV1,
    ) -> tracedecay_runtime_core::errors::Result<()> {
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
        if let Some((existing_scope, existing_graph, existing_binding, existing_locator)) =
            self.session_relation_graph.get()
        {
            return if existing_scope == &scope
                && std::ptr::eq(&**existing_graph, &*graph)
                && existing_binding == &graph_binding
                && existing_locator == &graph_verified_locator
            {
                Ok(())
            } else {
                Err(registered_error(
                    "bind session relation graph",
                    "registered session shard already has a different graph owner",
                ))
            };
        }
        self.session_relation_graph
            .set((scope, graph, graph_binding, graph_verified_locator))
            .map_err(|_| {
                registered_error(
                    "bind session relation graph",
                    "registered session shard graph binding raced",
                )
            })
    }

    pub(crate) fn session_relation_graph(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<(
        &SessionRelationScope,
        &tracedecay_graph_db::GraphDbLeaseV1,
        &StoreRuntimeBindingV1,
        &VerifiedStoreLocatorV1,
    )> {
        self.session_relation_graph
            .get()
            .map(|(scope, graph, binding, locator)| (scope, graph, binding, locator))
            .ok_or_else(|| {
                registered_error(
                    "resolve session relation graph",
                    "daemon-owned session relation graph is unavailable",
                )
            })
    }

    pub fn session_relation_graph_identity(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<(&StoreRuntimeBindingV1, &VerifiedStoreLocatorV1)>
    {
        let (_, _, binding, locator) = self.session_relation_graph()?;
        Ok((binding, locator))
    }

    pub(crate) fn session_relation_store(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<(&SessionRelationScope, SessionRelationGraphStore)>
    {
        let (scope, graph, _, _) = self.session_relation_graph()?;
        Ok((scope, SessionRelationGraphStore::new(graph.clone())))
    }
}
