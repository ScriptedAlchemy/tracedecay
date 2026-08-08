use std::sync::Arc;

use tracedecay_graph_db::GraphDb;

use super::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Binds the exact registered Grafeo runtime paired with this memory
    /// shard. A second binding is rejected so path-derived or sibling-project
    /// handles cannot silently replace the mounted authority.
    pub fn bind_memory_relation_graph(&self, graph: Arc<GraphDb>) -> Result<()> {
        if self
            .inner
            .memory_relation_graph
            .get()
            .is_some_and(|mounted| Arc::ptr_eq(mounted, &graph))
        {
            return Ok(());
        }
        self.inner
            .memory_relation_graph
            .set(graph)
            .map_err(|_| TraceDecayError::Database {
                operation: "bind project memory relation graph".to_owned(),
                message: "memory relation graph is already bound".to_owned(),
            })
    }

    pub(crate) fn memory_relation_graph(&self) -> Option<Arc<GraphDb>> {
        self.inner.memory_relation_graph.get().cloned()
    }
}
