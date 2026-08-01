pub mod authority;
mod failure;
mod project;
mod project_provider;
mod scheduler;
mod startup;
mod user;
mod user_provider;

pub use authority::{IngestAdmissionBinding, SessionIngestAuthority};
pub use failure::{
    TranscriptCatchUpFailure, classify_claude_observation_failure,
    classify_transcript_ingest_failure,
};
pub use project::{
    home_dir, ingest_project_sources_for_provider,
    ingest_project_sources_for_provider_with_cancellation, with_transcript_source_home,
};
pub use startup::ingest_user_global_sources_for_startup_with_db;
pub use user::{USER_SESSIONS_DB_FILENAME, user_sessions_db_path};
pub use user::{
    ingest_user_global_sources_for_provider_with_authorities, registered_project_roots_from,
    try_ingest_user_codex_sessions_with_db_and_admission,
};

#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod test_support {
    pub use super::failure::{IngestPassBounds, IngestPassCoverage, IngestPassOutcome};
    pub use super::project::ingest_project_sources_for_provider_without_registered_authority;
    pub use super::scheduler::USER_INGEST_PROVIDER_FRONTIER_KEY;
    pub use super::startup::ingest_user_global_sources_for_startup_with_db_without_registered_authority;
    pub use super::user::{
        ingest_user_global_sources_for_provider_with_roots_bounded,
        ingest_user_global_sources_for_provider_with_roots_without_registered_authority,
    };
}

#[cfg(test)]
mod tests;
