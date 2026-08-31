//! Canonical retrieval telemetry for explicit retained-memory search.

use tracedecay_application::{RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1};
use tracedecay_domain::{FactOwnerV1, ManifestDigest, ProvenanceId};
use tracedecay_runtime_core::store::memory::DatabaseFactStore;
use tracedecay_store::{
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactRetrievalReceiptV1, ProjectMemoryFactSearchPageV1,
};

use crate::memory::MemoryApplication;
use crate::memory_mapping;
use crate::memory_mutation::{
    MemoryMutationSettlement, bounded_memory_operation, fact_write_control,
    memory_mutation_settlement,
};

#[derive(Default)]
pub struct TrackedExplicitSearch {
    pub projections: Vec<ProjectMemoryFactProjectionV1>,
    pub receipt: Option<ProjectMemoryFactRetrievalReceiptV1>,
    pub authority_result_invalid: bool,
    pub settled_after_expiry: bool,
}

impl TrackedExplicitSearch {
    pub fn committed_state(&self) -> Option<&ManifestDigest> {
        self.receipt
            .as_ref()
            .map(ProjectMemoryFactRetrievalReceiptV1::committed_state_digest)
    }
}

#[hotpath::measure(label = "daemon.retained.memory.track", future = true)]
pub async fn track_explicit_search(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    owner: &FactOwnerV1,
    operation_id: ProvenanceId,
    page: &ProjectMemoryFactSearchPageV1,
) -> Result<TrackedExplicitSearch, RetainedSurfaceExecutionErrorV1> {
    if page.hits().is_empty() {
        return Ok(TrackedExplicitSearch::default());
    }
    let targets = page
        .hits()
        .iter()
        .map(|hit| ProjectMemoryFactIdV1::new(owner.clone(), hit.fact().fact_id().clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(memory_mapping::map_store_error)?;
    let command =
        ProjectMemoryFactRetrievalCommandV1::new(owner.clone(), operation_id, targets, true)
            .map_err(memory_mapping::map_store_error)?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) = bounded_memory_operation(context, async {
        Ok(hotpath::future!(
            memory.record_project_memory_fact_retrieval(command, &write_control),
            label = "daemon.retained.memory.track.commit"
        )
        .await)
    })
    .await?;
    let (outcome, authority_result_invalid) = match memory_mutation_settlement(outcome)? {
        MemoryMutationSettlement::Validated(outcome) => (outcome, false),
        MemoryMutationSettlement::InvalidAuthority(outcome) => (outcome, true),
    };
    Ok(TrackedExplicitSearch {
        projections: outcome.projections().to_vec(),
        receipt: Some(outcome.receipt().clone()),
        authority_result_invalid,
        settled_after_expiry,
    })
}
