//! Session-memory (holographic fact store) surface of [`TraceDecay`].

use crate::application::memory::{MemoryApplication, MemoryOperationContext, V1UpdateFactOutcome};
// The shared resolvers live in `tracedecay_usecases::memory` (the crate that
// owns `MemoryApplication`/`MemoryApplicationError`) rather than in
// `tracedecay-runtime-core` — that crate is a *dependency* of
// `tracedecay-usecases`, so hosting these there would require a circular
// crate dependency. Both this module and
// `tracedecay-dashboard-api::tracedecay::facts` delegate to the same
// functions instead of keeping independent copies.
pub(crate) use crate::application::memory::{memory_application_error, memory_application_for_db};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{
    AddFactOutcome, AddFactRequest, FactRecord, FactSearchResult, FeedbackRequest, FeedbackResult,
    MemoryCategory, MemoryStatus, SearchFactsRequest, TrustHistoryEntry, UpdateFactRequest,
};
use crate::store::memory::{ProjectFactStore, ProjectMemoryDbHandle};
use tracedecay_domain::{FactOwnerV1, ProjectId};

use super::TraceDecay;

const MAX_FACT_HISTORY_LIMIT: usize = 1_000;

fn project_memory_owner_from_layout_id(project_id: Option<&str>) -> Result<FactOwnerV1> {
    let project_id = project_id.ok_or_else(|| TraceDecayError::Config {
        message: "active project has no authoritative project_id for memory".to_string(),
    })?;
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid authoritative project_id for memory: {error}"),
        })?;
    Ok(FactOwnerV1::Project { project_id })
}

impl TraceDecay {
    /// Returns the only project-memory owner accepted by core routes.
    ///
    /// The ID is supplied by the resolved store layout, never reconstructed
    /// from a filesystem path or a caller-provided display label.
    pub(crate) fn project_memory_owner(&self) -> Result<FactOwnerV1> {
        project_memory_owner_from_layout_id(self.store_layout.identity.project_id.as_deref())
    }

    /// Opens the project-wide memory store. Project facts are project-wide by
    /// contract; when this instance serves a branch-sharded database, memory
    /// reads and writes must still target the shared project store, or
    /// branch shards accumulate diverging fact stores and the daemon repairs
    /// the wrong file. This is the single resolver for that routing
    /// decision — the MCP memory handlers' no-selector arm calls this method
    /// directly instead of re-deriving the predicate.
    pub(crate) async fn project_memory_db(&self) -> Result<ProjectMemoryDbHandle<'_>> {
        if self.db_path() == self.store_layout.graph_db_path {
            Ok(ProjectMemoryDbHandle::Active(&self.db))
        } else {
            let database = if self.read_only {
                self.open_project_store_db_read_only().await?
            } else {
                self.open_project_store_db().await?
            };
            Ok(ProjectMemoryDbHandle::Owned(Box::new(database)))
        }
    }

    fn generated_memory_operation(&self, action: &str) -> Result<MemoryOperationContext> {
        let owner = self.project_memory_owner()?;
        MemoryOperationContext::generated(&owner, action, None).map_err(memory_application_error)
    }

    /// Resolves the project-memory owner and database into one owner-bound
    /// application over a fact store that owns its resolved handle. Every
    /// project-memory route builds its application through this accessor.
    async fn project_memory_application(&self) -> Result<MemoryApplication<ProjectFactStore<'_>>> {
        let owner = self.project_memory_owner()?;
        let store = self.project_memory_db().await?.into_fact_store();
        MemoryApplication::new(owner, store).map_err(memory_application_error)
    }

    /// Add a fact to the holographic memory store. The outcome carries the
    /// stored (or pre-existing) fact plus a write-time diff report
    /// (near-duplicate / possible-conflict / secret rejection).
    pub async fn add_fact(&self, request: AddFactRequest) -> Result<AddFactOutcome> {
        let context = self.generated_memory_operation("add fact")?;
        self.project_memory_application()
            .await?
            .add_fact_v1(request, context)
            .await
            .map_err(memory_application_error)
    }

    /// Search facts by lexical overlap, entity metadata, category, and trust.
    pub async fn search_facts(&self, request: SearchFactsRequest) -> Result<Vec<FactSearchResult>> {
        let context = self.generated_memory_operation("search facts")?;
        self.project_memory_application()
            .await?
            .search_facts_v1(request, context)
            .await
            .map_err(memory_application_error)
    }

    /// Search facts without updating recall/access counters. This is for
    /// background enrichment surfaces such as `tracedecay_context`, where a
    /// memory match is supporting context rather than an explicit recall.
    pub async fn search_facts_untracked(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>> {
        let owner = self.project_memory_owner()?;
        let db = self.open_project_store_db_read_only().await?;
        memory_application_for_db(owner, &db)?
            .search_facts_untracked_v1(request)
            .await
            .map_err(memory_application_error)
    }

    pub async fn probe_entity(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let context = self.generated_memory_operation("probe facts")?;
        self.project_memory_application()
            .await?
            .probe_facts_v1(
                SearchFactsRequest {
                    query: entity.to_owned(),
                    category,
                    limit: Some(limit),
                    min_trust,
                    include_why: true,
                },
                context,
            )
            .await
            .map_err(memory_application_error)
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        let context = self.generated_memory_operation("update fact")?;
        match self
            .project_memory_application()
            .await?
            .update_fact_v1(request, context)
            .await
            .map_err(memory_application_error)?
        {
            V1UpdateFactOutcome::Updated(fact) => Ok(*fact),
            V1UpdateFactOutcome::RejectedSecretLike { reason } => Err(TraceDecayError::Database {
                operation: "update_fact".to_owned(),
                message: reason,
            }),
        }
    }

    pub async fn remove_fact(&self, fact_id: i64) -> Result<bool> {
        let context = self.generated_memory_operation("remove fact")?;
        self.project_memory_application()
            .await?
            .remove_fact_v1(fact_id, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn list_facts(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactRecord>> {
        let context = self.generated_memory_operation("list facts")?;
        self.project_memory_application()
            .await?
            .list_facts_v1(category, min_trust, limit, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn get_fact(&self, fact_id: i64) -> Result<Option<FactRecord>> {
        self.project_memory_application()
            .await?
            .get_fact_v1(fact_id)
            .await
            .map_err(memory_application_error)
    }

    pub async fn record_fact_feedback(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        let context = self.generated_memory_operation("record fact feedback")?;
        self.project_memory_application()
            .await?
            .record_fact_feedback_v1(request, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        self.project_memory_application()
            .await?
            .fact_trust_history_v1(fact_id, MAX_FACT_HISTORY_LIMIT)
            .await
            .map_err(memory_application_error)
    }

    pub async fn memory_status(&self) -> Result<MemoryStatus> {
        self.project_memory_application()
            .await?
            .memory_status_v1()
            .await
            .map_err(memory_application_error)
    }

    /// Runs one bounded, authoritative compatibility-memory repair batch for
    /// the active project. Daemon maintenance owns scheduling and retries;
    /// callers receive the exact batch progress and must not infer completion.
    ///
    /// Public because status reads are pure (they report backlog without
    /// repairing); this is the explicit repair entry point that owns the
    /// side effect.
    pub async fn repair_project_memory_once(
        &self,
    ) -> Result<tracedecay_store::CompatibilityMemoryRepairStatsV1> {
        let context = self.generated_memory_operation("daemon memory repair")?;
        self.project_memory_application()
            .await?
            .dashboard_repair_v1(context)
            .await
            .map_err(memory_application_error)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn project_memory_owner_requires_a_valid_authoritative_layout_id() {
        assert!(project_memory_owner_from_layout_id(None).is_err());
        assert!(project_memory_owner_from_layout_id(Some("")).is_err());
    }
}
