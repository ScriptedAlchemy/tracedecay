use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::CodeGenerationId;

use super::{
    GraphVectorGenerationStoreV1, ProjectSemanticVectorCodeScopeLiveness,
    ProjectSemanticVectorPublishedDependency, ProjectSemanticVectorRetentionStep,
    ProjectSemanticVectorSourceLiveness, ProjectVectorReadableSources,
    RetainedSemanticVectorGraphV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProjectVectorRetentionFailure {
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

impl ProjectVectorRetentionFailure {
    pub(super) fn from_configuration(
        error: crate::application::semantic_runtime::SemanticConfigurationBackendErrorV1,
    ) -> Self {
        use crate::application::semantic_runtime::SemanticConfigurationBackendErrorV1;
        match error {
            SemanticConfigurationBackendErrorV1::Conflict => Self::ResetRequired(
                "semantic configuration inventory changed during retention".to_owned(),
            ),
            SemanticConfigurationBackendErrorV1::Rejected => Self::Corrupt(
                "semantic configuration inventory was rejected by its authority".to_owned(),
            ),
            SemanticConfigurationBackendErrorV1::Unavailable => Self::Unavailable(
                "semantic configuration inventory authority is unavailable".to_owned(),
            ),
        }
    }

    pub(super) fn retention_step(self) -> ProjectSemanticVectorRetentionStep {
        match self {
            Self::ResetRequired(message) => {
                ProjectSemanticVectorRetentionStep::ResetRequired(message)
            }
            Self::Corrupt(message) => ProjectSemanticVectorRetentionStep::Corrupt(message),
            Self::Unavailable(message) => ProjectSemanticVectorRetentionStep::Unavailable(message),
            Self::Denied(message) => ProjectSemanticVectorRetentionStep::Denied(message),
        }
    }

    pub(super) fn readable_sources(self) -> ProjectVectorReadableSources {
        match self {
            Self::ResetRequired(message) => ProjectVectorReadableSources::ResetRequired(message),
            Self::Corrupt(message) => ProjectVectorReadableSources::Corrupt(message),
            Self::Unavailable(message) => ProjectVectorReadableSources::Unavailable(message),
            Self::Denied(message) => ProjectVectorReadableSources::Denied(message),
        }
    }

    pub(super) fn source_liveness(self) -> ProjectSemanticVectorSourceLiveness {
        match self {
            Self::ResetRequired(message) => {
                ProjectSemanticVectorSourceLiveness::ResetRequired(message)
            }
            Self::Corrupt(message) => ProjectSemanticVectorSourceLiveness::Corrupt(message),
            Self::Unavailable(message) => ProjectSemanticVectorSourceLiveness::Unavailable(message),
            Self::Denied(message) => ProjectSemanticVectorSourceLiveness::Denied(message),
        }
    }

    pub(super) fn code_scope_liveness(self) -> ProjectSemanticVectorCodeScopeLiveness {
        match self {
            Self::ResetRequired(message) => {
                ProjectSemanticVectorCodeScopeLiveness::ResetRequired(message)
            }
            Self::Corrupt(message) => ProjectSemanticVectorCodeScopeLiveness::Corrupt(message),
            Self::Unavailable(message) => {
                ProjectSemanticVectorCodeScopeLiveness::Unavailable(message)
            }
            Self::Denied(message) => ProjectSemanticVectorCodeScopeLiveness::Denied(message),
        }
    }

    pub(super) fn published_dependency(self) -> ProjectSemanticVectorPublishedDependency {
        match self {
            Self::ResetRequired(message) => {
                ProjectSemanticVectorPublishedDependency::ResetRequired(message)
            }
            Self::Corrupt(message) => ProjectSemanticVectorPublishedDependency::Corrupt(message),
            Self::Unavailable(message) => {
                ProjectSemanticVectorPublishedDependency::Unavailable(message)
            }
            Self::Denied(message) => ProjectSemanticVectorPublishedDependency::Denied(message),
        }
    }
}

impl From<crate::store::vector_generations::VectorGenerationStoreErrorV1>
    for ProjectVectorRetentionFailure
{
    fn from(error: crate::store::vector_generations::VectorGenerationStoreErrorV1) -> Self {
        use crate::store::vector_generations::VectorGenerationStoreErrorV1;
        match error {
            VectorGenerationStoreErrorV1::ResetRequired(message) => Self::ResetRequired(message),
            VectorGenerationStoreErrorV1::Corrupt(message) => Self::Corrupt(message),
            VectorGenerationStoreErrorV1::InvalidPlan(message) => Self::Denied(message),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

pub(super) async fn complete_configuration_inventory(
    configuration: &crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
) -> Result<
    crate::application::semantic_runtime::SemanticConfigurationInventoryReceiptV1,
    ProjectVectorRetentionFailure,
> {
    use crate::application::semantic_runtime::{
        MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
        SemanticConfigurationInventoryPageRequestV1,
    };
    let mut request = SemanticConfigurationInventoryPageRequestV1::first(
        MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
    )
    .map_err(|error| ProjectVectorRetentionFailure::Denied(error.to_string()))?;
    loop {
        let page = configuration
            .configuration_inventory_page(&request)
            .await
            .map_err(ProjectVectorRetentionFailure::from_configuration)?;
        match (page.continuation, page.complete_receipt) {
            (Some(cursor), None) => {
                request = SemanticConfigurationInventoryPageRequestV1::after(
                    cursor,
                    MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
                )
                .map_err(|error| ProjectVectorRetentionFailure::Denied(error.to_string()))?;
            }
            (None, Some(receipt)) => return Ok(receipt),
            _ => {
                return Err(ProjectVectorRetentionFailure::Corrupt(
                    "semantic configuration inventory coverage is incomplete".to_owned(),
                ));
            }
        }
    }
}

pub(super) async fn validate_configured_vector_roots(
    configuration: &crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    store: &GraphVectorGenerationStoreV1,
    retained: &RetainedSemanticVectorGraphV1,
    stage_revision: tracedecay_store::SemanticVectorStageCensusRevision,
    inventory: crate::application::semantic_runtime::SemanticConfigurationInventoryReceiptV1,
) -> Result<
    (
        crate::application::semantic_runtime::SemanticConfiguredVectorRootReceiptV1,
        BTreeSet<CodeGenerationId>,
    ),
    ProjectVectorRetentionFailure,
> {
    use crate::application::semantic_runtime::{
        MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
        SemanticConfiguredVectorRootPageRequestV1,
    };
    let mut request = SemanticConfiguredVectorRootPageRequestV1::first(
        inventory,
        MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
    )
    .map_err(|error| ProjectVectorRetentionFailure::Denied(error.to_string()))?;
    let mut sources = BTreeSet::new();
    loop {
        let page = configuration
            .configured_vector_roots_page(&request)
            .await
            .map_err(ProjectVectorRetentionFailure::from_configuration)?;
        for root in &page.roots {
            let dependency = store
                .published_generation_dependency(
                    root,
                    stage_revision,
                    Arc::clone(retained.cancellation()),
                )
                .map_err(ProjectVectorRetentionFailure::from)?;
            let tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup::Published(
                dependency,
            ) = dependency
            else {
                return Err(ProjectVectorRetentionFailure::ResetRequired(
                    "configured semantic vector root has no exact published dependency".to_owned(),
                ));
            };
            sources.insert(
                CodeGenerationId::new(dependency.source_generation.as_str())
                    .map_err(|error| ProjectVectorRetentionFailure::Corrupt(error.to_string()))?,
            );
        }
        match (page.continuation, page.complete_receipt) {
            (Some(cursor), None) => {
                request = SemanticConfiguredVectorRootPageRequestV1::after(
                    cursor,
                    MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE,
                )
                .map_err(|error| ProjectVectorRetentionFailure::Denied(error.to_string()))?;
            }
            (None, Some(receipt)) => {
                store
                    .validate_project_census_revision(
                        stage_revision,
                        Arc::clone(retained.cancellation()),
                    )
                    .map_err(ProjectVectorRetentionFailure::from)?;
                return Ok((receipt, sources));
            }
            _ => {
                return Err(ProjectVectorRetentionFailure::Corrupt(
                    "configured semantic vector root coverage is incomplete".to_owned(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectSemanticVectorCodeScopeLiveness, ProjectSemanticVectorPublishedDependency,
        ProjectSemanticVectorSourceLiveness, ProjectVectorRetentionFailure,
    };
    use crate::store::vector_generations::VectorGenerationStoreErrorV1;

    #[test]
    fn vector_store_reset_and_corruption_remain_typed() {
        let reset = ProjectVectorRetentionFailure::from(
            VectorGenerationStoreErrorV1::ResetRequired("missing root".to_owned()),
        );
        let corrupt = ProjectVectorRetentionFailure::from(VectorGenerationStoreErrorV1::Corrupt(
            "invalid dependency".to_owned(),
        ));
        let unavailable = ProjectVectorRetentionFailure::from(
            VectorGenerationStoreErrorV1::Unavailable("graph is closed".to_owned()),
        );

        assert_eq!(
            reset.clone(),
            ProjectVectorRetentionFailure::ResetRequired("missing root".to_owned())
        );
        assert_eq!(
            corrupt.clone(),
            ProjectVectorRetentionFailure::Corrupt("invalid dependency".to_owned())
        );
        assert_eq!(
            unavailable.clone(),
            ProjectVectorRetentionFailure::Unavailable("graph is closed".to_owned())
        );
        assert!(matches!(
            reset.clone().source_liveness(),
            ProjectSemanticVectorSourceLiveness::ResetRequired(message)
                if message == "missing root"
        ));
        assert!(matches!(
            corrupt.clone().source_liveness(),
            ProjectSemanticVectorSourceLiveness::Corrupt(message)
                if message == "invalid dependency"
        ));
        assert!(matches!(
            unavailable.clone().source_liveness(),
            ProjectSemanticVectorSourceLiveness::Unavailable(message)
                if message == "graph is closed"
        ));
        assert!(matches!(
            reset.clone().code_scope_liveness(),
            ProjectSemanticVectorCodeScopeLiveness::ResetRequired(message)
                if message == "missing root"
        ));
        assert!(matches!(
            corrupt.clone().code_scope_liveness(),
            ProjectSemanticVectorCodeScopeLiveness::Corrupt(message)
                if message == "invalid dependency"
        ));
        assert!(matches!(
            unavailable.clone().code_scope_liveness(),
            ProjectSemanticVectorCodeScopeLiveness::Unavailable(message)
                if message == "graph is closed"
        ));
        assert!(matches!(
            reset.published_dependency(),
            ProjectSemanticVectorPublishedDependency::ResetRequired(message)
                if message == "missing root"
        ));
        assert!(matches!(
            corrupt.published_dependency(),
            ProjectSemanticVectorPublishedDependency::Corrupt(message)
                if message == "invalid dependency"
        ));
        assert!(matches!(
            unavailable.published_dependency(),
            ProjectSemanticVectorPublishedDependency::Unavailable(message)
                if message == "graph is closed"
        ));
    }
}
