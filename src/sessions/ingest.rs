mod failure;
mod project;
mod project_provider;
mod scheduler;
mod startup;
mod user;
mod user_provider;

pub(crate) use failure::{
    TranscriptCatchUpFailure, classify_claude_observation_failure,
    classify_transcript_ingest_failure,
};
pub(crate) use project::{
    home_dir, ingest_project_sources_for_provider,
    ingest_project_sources_for_provider_with_cancellation, with_transcript_source_home,
};
pub(crate) use startup::ingest_user_global_sources_for_startup_with_db;
pub use user::{USER_SESSIONS_DB_FILENAME, user_sessions_db_path};
pub(crate) use user::{
    ingest_user_global_sources_for_provider_with_authorities, registered_project_roots_from,
    try_ingest_user_codex_sessions_with_db_and_admission,
};

#[cfg(test)]
mod tests;
