//! Git index transaction application boundary.

mod catalog;
mod transactions;

pub use catalog::{git_index_catalog_contribution, git_index_handler_descriptors};
pub use transactions::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, git_index_effect_class,
};

#[cfg(test)]
mod tests;
