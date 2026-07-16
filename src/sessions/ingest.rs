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
pub(crate) use project::{finalize_project_ingest, home_dir, ingest_project_sources_for_provider};
pub use project::{ingest_global_sources, ingest_global_sources_for_provider};
pub(crate) use startup::ingest_user_global_sources_for_startup_with_db;
pub use user::{
    USER_SESSIONS_DB_FILENAME, ingest_user_codex_sessions, ingest_user_cursor_sessions,
    ingest_user_global_sources, ingest_user_global_sources_for_provider, open_user_session_db,
    registered_project_roots, try_registered_project_roots, user_sessions_db_path,
};
pub(crate) use user::{
    ingest_user_global_sources_for_provider_at_with_db,
    ingest_user_global_sources_for_provider_with_authorities, registered_project_roots_from,
    try_ingest_user_codex_sessions_with_db,
};

#[cfg(test)]
mod tests;
