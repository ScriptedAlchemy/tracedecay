//! Shared profile code-index worker configuration journey.
//!
//! Dashboard and daemon-service both commit the same store-open → mutation →
//! grant → persist path. The grant is issued by the caller; this module owns
//! the store-backed mutation, commit, and typed error mapping so those steps
//! cannot diverge.

use tracedecay_domain::configuration::{
    CodeIndexWorkerSelectionV1, ConfigurationRevisionId, UserProfileId,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::configuration::{
    ProfileCodeIndexWorkerCommitV1, ProfileCodeIndexWorkerConfigurationStore,
};

use super::{ConfigurationError, ConfigurationMutationAuthority, DirectConfigurationMutation};

/// Open the exact registered profile-sessions store and build the worker mutation.
pub fn profile_code_index_worker_mutation(
    database: &RegisteredGlobalDb,
    profile_id: &UserProfileId,
    selection: CodeIndexWorkerSelectionV1,
) -> Result<DirectConfigurationMutation, ConfigurationError> {
    ProfileCodeIndexWorkerConfigurationStore::new_registered(database, profile_id)
        .and_then(|store| store.mutation(selection))
}

/// Open the exact registered profile-sessions store and commit the worker selection.
#[hotpath::measure(label = "daemon.config.profile_workers.commit", future = true)]
pub async fn commit_profile_code_index_worker_selection(
    database: &RegisteredGlobalDb,
    profile_id: &UserProfileId,
    authority: &ConfigurationMutationAuthority,
    selection: CodeIndexWorkerSelectionV1,
    expected_revision: &ConfigurationRevisionId,
) -> Result<ProfileCodeIndexWorkerCommitV1, ConfigurationError> {
    let store = ProfileCodeIndexWorkerConfigurationStore::new_registered(database, profile_id)?;
    store
        .commit_selection(authority, selection, expected_revision)
        .await
}

/// Map store/control-plane errors onto the typed daemon configuration failure.
///
/// `ResetRequired` stays a reset; every other store failure is a configuration
/// authority miss. Callers must not add a catch-all that collapses those.
pub fn map_profile_worker_configuration_error(error: ConfigurationError) -> TraceDecayError {
    match error {
        ConfigurationError::ResetRequired { reason } => {
            TraceDecayError::reset_required("configuration", reason)
        }
        error => TraceDecayError::Config {
            message: format!("configuration authority unavailable: {error}"),
        },
    }
}
