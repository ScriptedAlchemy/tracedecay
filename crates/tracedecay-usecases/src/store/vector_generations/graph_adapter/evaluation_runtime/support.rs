use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
use tracedecay_domain::{
    BrainId, CodeGenerationId, ProjectId, RepositoryId, UserProfileId, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationId, GraphGenerationManifest,
    GraphProjectionIdentity, GraphWatermark, SourceGeneration,
};
use tracedecay_store::{
    CodeShardScopeV1, GraphPublicationStoreErrorV1, RuntimeInterruptionV1,
    SemanticVectorStagingStoreError, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};

pub(super) fn evaluation_binding() -> Result<StoreRuntimeBindingV1, GraphDbError> {
    Ok(StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::new("brain.semantic-evaluation")
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            UserProfileId::new("profile.semantic-evaluation")
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            ProjectId::new("project.semantic-evaluation")
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        ),
        StoreIncarnationV1::new(1).map_err(|error| GraphDbError::invalid(error.to_string()))?,
        StoreAuthorityEpochV1::new(1).map_err(|error| GraphDbError::invalid(error.to_string()))?,
    ))
}

pub(super) fn evaluation_source_namespace(
    generation: &CodeGenerationId,
) -> Result<tracedecay_graph_db::GraphNamespace, GraphDbError> {
    tracedecay_graph_db::GraphNamespace::new(format!("semantic-evaluation-code:{generation}"))
}

/// Isolated measurement publishes a source-generation identity receipt, not a
/// second copy of the production code graph. Vector staging needs a live
/// parent replay; chunks come from `CodeIndexPublishedGenerationV1`.
pub(super) fn evaluation_source_receipt_manifest(
    projection: GraphProjectionIdentity,
    graph_generation: GraphGenerationId,
    source_generation_id: &CodeGenerationId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, GraphDbError> {
    let receipt_digest = canonical_sha256(&(
        "tracedecay.semantic-evaluation-source-receipt.v1",
        source_generation_id,
        &graph_generation,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphGenerationManifest::new_checked(
        projection,
        graph_generation,
        SourceGeneration::new(format!("source:{}", receipt_digest.as_str()))?,
        GraphWatermark::new(format!("watermark:{}", receipt_digest.as_str()))?,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        check,
    )
}

pub(super) fn evaluation_source_scope(
    binding: &StoreRuntimeBindingV1,
    repository: &RepositoryId,
    worktree: &WorktreeId,
) -> Result<StoreShardIdV1, GraphDbError> {
    let StoreShardIdV1 {
        brain_id,
        profile_id,
        scope: tracedecay_store::StoreShardScopeV1::Project { project_id },
    } = &binding.shard_id
    else {
        return Err(GraphDbError::invalid(
            "semantic evaluation binding is not a project shard",
        ));
    };
    Ok(StoreShardIdV1::code(
        brain_id.clone(),
        profile_id.clone(),
        project_id.clone(),
        repository.clone(),
        CodeShardScopeV1::Worktree {
            worktree_id: worktree.clone(),
        },
    ))
}

pub(super) fn map_publication_error(error: GraphPublicationStoreErrorV1) -> GraphDbError {
    match error {
        GraphPublicationStoreErrorV1::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::Cancelled) => {
            GraphDbError::Cancelled
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::DeadlineExceeded) => {
            GraphDbError::DeadlineExceeded
        }
        GraphPublicationStoreErrorV1::Infrastructure => {
            GraphDbError::unavailable("semantic evaluation metadata is unavailable")
        }
        GraphPublicationStoreErrorV1::Corrupt(message) => GraphDbError::Corrupt { message },
    }
}

pub(super) fn map_staging_error(error: SemanticVectorStagingStoreError) -> GraphDbError {
    match error {
        SemanticVectorStagingStoreError::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        SemanticVectorStagingStoreError::Interrupted(RuntimeInterruptionV1::Cancelled) => {
            GraphDbError::Cancelled
        }
        SemanticVectorStagingStoreError::Interrupted(RuntimeInterruptionV1::DeadlineExceeded) => {
            GraphDbError::DeadlineExceeded
        }
        SemanticVectorStagingStoreError::Infrastructure => {
            GraphDbError::unavailable("semantic evaluation staging persistence is unavailable")
        }
        SemanticVectorStagingStoreError::Busy => {
            GraphDbError::unavailable("semantic evaluation staging authority is busy")
        }
        SemanticVectorStagingStoreError::CensusRevisionChanged { expected, actual } => {
            GraphDbError::ResetRequired {
                message: format!(
                    "semantic evaluation census changed from {} to {}",
                    expected.get(),
                    actual.get()
                ),
            }
        }
        SemanticVectorStagingStoreError::AuthorityLost
        | SemanticVectorStagingStoreError::ReusedOperationContext => GraphDbError::Conflict,
        SemanticVectorStagingStoreError::Corrupt(message) => GraphDbError::Corrupt { message },
    }
}

pub(super) fn map_code_graph_error(error: CodeGraphProjectionError) -> GraphDbError {
    match error {
        CodeGraphProjectionError::Cancelled => GraphDbError::Cancelled,
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::Conflict | CodeGraphProjectionError::GenerationMismatch => {
            GraphDbError::Conflict
        }
        CodeGraphProjectionError::BudgetExhausted { budget, limit } => {
            // Preserve budget identity; unrecognized names are
            // projection-local budgets reported under the read class.
            let kind = GraphBudgetKind::from_name(&budget).unwrap_or(GraphBudgetKind::Read);
            GraphDbError::budget_exhausted(kind, limit)
        }
        CodeGraphProjectionError::ProjectionMismatch {
            namespace,
            projection,
            message,
        } => GraphDbError::ProjectionMismatch {
            namespace,
            projection,
            message,
        },
        CodeGraphProjectionError::RecoveredGenerationMismatch {
            namespace,
            projection,
            generation,
            message,
        } => GraphDbError::GenerationMismatch {
            namespace,
            projection,
            generation,
            message,
        },
        CodeGraphProjectionError::ResetRequired(message) => GraphDbError::ResetRequired { message },
        CodeGraphProjectionError::Corrupt(message) => GraphDbError::Corrupt { message },
        CodeGraphProjectionError::Unavailable(message) => GraphDbError::Unavailable { message },
        CodeGraphProjectionError::DurabilityUncertain(message) => {
            GraphDbError::DurabilityUncertain { message }
        }
        CodeGraphProjectionError::Closed => GraphDbError::Closed,
        CodeGraphProjectionError::Contract(message) => GraphDbError::InvalidRequest { message },
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_store::SemanticVectorStagingStoreError;

    use super::map_staging_error;

    #[test]
    fn map_staging_error_names_busy_and_infrastructure_separately() {
        assert_eq!(
            map_staging_error(SemanticVectorStagingStoreError::Infrastructure).to_string(),
            "graph database unavailable: semantic evaluation staging persistence is unavailable",
        );
        assert_eq!(
            map_staging_error(SemanticVectorStagingStoreError::Busy).to_string(),
            "graph database unavailable: semantic evaluation staging authority is busy",
        );
    }
}
