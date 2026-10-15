pub mod authority;
mod failure;
mod project;
pub(in crate::runtime) mod project_provider;
pub(crate) mod quarantine;
mod scheduler;
mod startup;
mod user;
mod user_provider;

pub use authority::{IngestAdmissionBinding, SessionIngestAuthority};
pub use failure::{
    IngestPassCoverage, TranscriptCatchUpFailure, TranscriptIngestDisposition,
    classify_claude_observation_failure, classify_transcript_ingest_disposition,
    classify_transcript_ingest_failure,
};
pub use project::{
    home_dir, ingest_project_sources_for_provider,
    ingest_project_sources_for_provider_with_cancellation_and_codex_state,
    transcript_source_profile, with_transcript_source_profile,
};
pub use startup::{
    TranscriptIngestOutcome, ingest_user_global_sources_for_startup_with_db_and_codex_state,
    subscribe_history_capacity_release,
};
pub use user::{
    CodexHookIngestOutcome,
    ingest_user_global_sources_for_provider_with_authorities_and_cancellation,
    registered_project_roots_from, try_ingest_user_codex_sessions_with_db_and_admission,
    user_sessions_db_path,
};

#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod test_support {
    pub use super::failure::{IngestPassBounds, IngestPassOutcome};
    pub use super::project::ingest_project_sources_for_provider_bounded;
    pub use super::scheduler::{
        PROJECT_INGEST_PROVIDER_FRONTIER_KEY, USER_INGEST_PROVIDER_FRONTIER_KEY,
        default_ingest_pass_bounds,
    };
    pub use super::user::ingest_user_global_sources_for_provider_with_roots_bounded;
}

#[cfg(test)]
mod quarantine_codex_tests;
#[cfg(test)]
mod quarantine_contract;
#[cfg(test)]
mod quarantine_hermes_tests;
#[cfg(test)]
mod tests;
