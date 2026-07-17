//! Session-memory (holographic fact store) surface of [`TraceDecay`].

use crate::application::memory::{
    MemoryApplication, MemoryApplicationError, MemoryOperationContext, V1UpdateFactOutcome,
};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{
    AddFactOutcome, AddFactRequest, ContradictionResult, FactRecord, FactSearchResult,
    FeedbackRequest, FeedbackResult, MemoryCategory, MemoryStatus, SearchFactsRequest,
    TrustHistoryEntry, UpdateFactRequest,
};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::{FactOwnerV1, ProjectId};

use super::TraceDecay;

const MAX_FACT_HISTORY_LIMIT: usize = 1_000;

fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::database_operation("memory application", error)
}

pub(crate) fn memory_application_for_db(
    owner: FactOwnerV1,
    db: &crate::db::Database,
) -> Result<MemoryApplication<DatabaseFactStore<'_>>> {
    MemoryApplication::new(owner, DatabaseFactStore::new(db)).map_err(memory_application_error)
}

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

    fn memory_application(&self) -> Result<MemoryApplication<DatabaseFactStore<'_>>> {
        memory_application_for_db(self.project_memory_owner()?, &self.db)
    }

    fn generated_memory_operation(&self, action: &str) -> Result<MemoryOperationContext> {
        let owner = self.project_memory_owner()?;
        MemoryOperationContext::generated(&owner, action, None).map_err(memory_application_error)
    }

    fn daemon_memory_cutover_operation(&self) -> Result<MemoryOperationContext> {
        let owner = self.project_memory_owner()?;
        MemoryOperationContext::from_trusted_request_id(
            &owner,
            "daemon legacy memory cutover",
            "v1-cutover",
            None,
        )
        .map_err(memory_application_error)
    }

    /// Add a fact to the holographic memory store. The outcome carries the
    /// stored (or pre-existing) fact plus a write-time diff report
    /// (near-duplicate / possible-conflict / secret rejection).
    pub async fn add_fact(&self, request: AddFactRequest) -> Result<AddFactOutcome> {
        let context = self.generated_memory_operation("add fact")?;
        self.memory_application()?
            .add_fact_v1(request, context)
            .await
            .map_err(memory_application_error)
    }

    /// Search facts by lexical overlap, entity metadata, category, and trust.
    pub async fn search_facts(&self, request: SearchFactsRequest) -> Result<Vec<FactSearchResult>> {
        let context = self.generated_memory_operation("search facts")?;
        self.memory_application()?
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
        self.memory_application()?
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

    pub async fn related_facts(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let context = self.generated_memory_operation("related facts")?;
        self.memory_application()?
            .related_facts_v1(
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

    pub async fn reason_facts(
        &self,
        entities: &[String],
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let context = self.generated_memory_operation("reason facts")?;
        self.memory_application()?
            .reason_facts_v1(entities.to_vec(), category, min_trust, limit, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn contradict_facts(
        &self,
        category: Option<MemoryCategory>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<ContradictionResult>> {
        self.memory_application()?
            .contradict_facts_v1(category, threshold, limit)
            .await
            .map_err(memory_application_error)
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        let context = self.generated_memory_operation("update fact")?;
        match self
            .memory_application()?
            .update_fact_v1(request, context)
            .await
            .map_err(memory_application_error)?
        {
            V1UpdateFactOutcome::Updated(fact) => Ok(fact),
            V1UpdateFactOutcome::RejectedSecretLike { reason } => Err(TraceDecayError::Database {
                operation: "update_fact".to_owned(),
                message: reason,
            }),
        }
    }

    pub async fn remove_fact(&self, fact_id: i64) -> Result<bool> {
        let context = self.generated_memory_operation("remove fact")?;
        self.memory_application()?
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
        self.memory_application()?
            .list_facts_v1(category, min_trust, limit, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn get_fact(&self, fact_id: i64) -> Result<Option<FactRecord>> {
        self.memory_application()?
            .get_fact_v1(fact_id)
            .await
            .map_err(memory_application_error)
    }

    pub async fn record_fact_feedback(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        let context = self.generated_memory_operation("record fact feedback")?;
        self.memory_application()?
            .record_fact_feedback_v1(request, context)
            .await
            .map_err(memory_application_error)
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        self.memory_application()?
            .fact_trust_history_v1(fact_id, MAX_FACT_HISTORY_LIMIT)
            .await
            .map_err(memory_application_error)
    }

    pub async fn memory_status(&self) -> Result<MemoryStatus> {
        self.memory_application()?
            .memory_status_v1()
            .await
            .map_err(memory_application_error)
    }

    pub async fn project_memory_status(&self) -> Result<MemoryStatus> {
        let owner = self.project_memory_owner()?;
        let db = self.open_project_store_db().await?;
        memory_application_for_db(owner, &db)?
            .memory_status_v1()
            .await
            .map_err(memory_application_error)
    }

    /// Runs one bounded, authoritative compatibility-memory repair batch for
    /// the active project. Daemon maintenance owns scheduling and retries;
    /// callers receive the exact batch progress and must not infer completion.
    pub(crate) async fn repair_project_memory_once(
        &self,
    ) -> Result<tracedecay_store::CompatibilityMemoryRepairStatsV1> {
        let context = self.generated_memory_operation("daemon memory repair")?;
        self.memory_application()?
            .dashboard_repair_v1(context)
            .await
            .map_err(memory_application_error)
    }

    /// Advances exactly one persisted V1 raw-memory cutover batch. The stable
    /// receipt identity makes daemon restarts replay a completed cutover rather
    /// than creating a second import job.
    pub(crate) async fn advance_project_memory_cutover_once(
        &self,
    ) -> Result<tracedecay_store::CompatibilityLegacyMemoryCutoverProgressV1> {
        let context = self.daemon_memory_cutover_operation()?;
        self.memory_application()?
            .daemon_legacy_memory_cutover_v1(context)
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
