use tracedecay_store::{
    FactReadControl, ProjectMemoryGraphPageV1, ProjectMemoryGraphQueryV1, ProjectMemoryGraphStore,
};

use super::{MemoryApplication, MemoryApplicationError};

impl<A: ProjectMemoryGraphStore> MemoryApplication<A> {
    /// Reads the rebuildable Grafeo topology, then returns facts hydrated by
    /// the canonical owner-bound fact authority. Graph nodes never carry fact
    /// content and cannot act as a raw-row fallback.
    pub async fn project_memory_graph(
        &self,
        query: ProjectMemoryGraphQueryV1,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryGraphPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let max_relations = query.max_relations();
        let page = self
            .authority
            .project_memory_graph(query, read_control)
            .await?;
        if page.owner() != &self.owner
            || page.relations().len() > max_relations
            || page.facts().iter().any(|fact| fact.owner() != &self.owner)
            || page.relations().iter().any(|relation| {
                relation.source().owner() != &self.owner || relation.target().owner() != &self.owner
            })
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project memory graph owner and bounds",
            });
        }
        Ok(page)
    }
}
