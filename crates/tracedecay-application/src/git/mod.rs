//! Git index transaction application boundary.

mod catalog;
#[cfg(feature = "native-git")]
mod historical_blob;
mod public_wire;
mod read;
mod surface_catalog;
mod transactions;

pub use catalog::{git_index_catalog_contribution, git_index_handler_descriptors};
#[cfg(feature = "native-git")]
pub use historical_blob::NativeHistoricalBlobReaderV1;
pub use public_wire::{
    GitApplySurfaceRequest, GitBlameSurfaceRequest, GitDiffSurfaceRequest,
    GitHistorySurfaceRequest, GitHunksSurfaceRequest, GitPreviewSurfaceRequest, GitQueryEnvelopeV1,
    GitReadRequestV1, GitReadResultV1, GitReadSurfaceRequest, GitStatusSummaryV1,
    GitStatusSurfaceRequest, GitSurfaceDiffScopeV1,
};
pub use read::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest,
    GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest,
    GitIntelligenceError, GitReadPort, is_canonical_repository_relative_path,
};
pub use surface_catalog::{
    git_sdk_executable_binding_registry, git_surface_catalog_contribution,
    git_surface_handler_descriptors,
};
pub use transactions::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, git_index_effect_class,
};

#[cfg(test)]
mod tests;
