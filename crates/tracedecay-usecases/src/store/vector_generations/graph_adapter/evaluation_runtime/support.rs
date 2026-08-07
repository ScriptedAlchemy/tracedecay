use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
use tracedecay_domain::{BrainId, ProjectId, RepositoryId, UserProfileId, WorktreeId};
use tracedecay_graph_db::GraphDbError;
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
        SemanticVectorStagingStoreError::Infrastructure | SemanticVectorStagingStoreError::Busy => {
            GraphDbError::unavailable("semantic evaluation staging is unavailable")
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
        CodeGraphProjectionError::BudgetExhausted => GraphDbError::BudgetExhausted,
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
