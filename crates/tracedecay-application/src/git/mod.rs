//! Git index transaction application boundary.

mod catalog;
#[cfg(feature = "native-git")]
mod historical_blob;
mod native_integration;
mod read;
mod surface_catalog;
mod transactions;

pub use catalog::{git_index_catalog_contribution, git_index_handler_descriptors};
#[cfg(feature = "native-git")]
pub use historical_blob::NativeHistoricalBlobReaderV1;
pub use native_integration::{
    NativeIntegrationApplyRequestV1, NativeIntegrationCancelDispositionV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationContractError,
    NativeIntegrationEvidenceRevisionsV1, NativeIntegrationPort, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationRecoveryRequestV1, NativeIntegrationSelectionBindingV1,
    NativeIntegrationService, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStackResolutionRequestV1,
    NativeIntegrationStatusRequestV1,
};
pub use read::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest,
    GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest,
    GitIntelligenceError, GitReadPort, is_canonical_repository_relative_path,
};
pub use surface_catalog::{git_surface_catalog_contribution, git_surface_handler_descriptors};
pub use transactions::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, git_index_effect_class,
};

#[cfg(test)]
mod tests;
