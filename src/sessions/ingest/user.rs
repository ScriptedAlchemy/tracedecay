use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{self, TranscriptSource, try_ingest_source};
use crate::sessions::{
    SessionProvider, claude_observation, cline_like, codex, cursor, cursor_composer, hermes, kiro,
    vibe,
};

use super::failure::{
    TranscriptCatchUpFailure, classify_transcript_ingest_failure, claude_catch_up_failure,
};
use super::startup::TranscriptIngestOutcome;

pub const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

pub fn user_sessions_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}

pub async fn open_user_session_db(profile_root: &Path) -> Option<GlobalDb> {
    GlobalDb::open_at(&user_sessions_db_path(profile_root)).await
}

/// All registry paths that may identify project-owned transcript evidence.
pub async fn registered_project_roots() -> Vec<PathBuf> {
    try_registered_project_roots().await.unwrap_or_default()
}

/// Returns `None` when the registry cannot be opened. User-scope ingestion
/// must fail closed in that case: an empty root set is valid for a fresh
/// profile, while an unavailable registry cannot safely prove that evidence
/// is projectless.
pub async fn try_registered_project_roots() -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open().await?;
    registered_project_roots_from(&global).await
}

async fn try_registered_project_roots_at(profile_root: &Path) -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open_at(&profile_root.join("global.db")).await?;
    registered_project_roots_from(&global).await
}

pub(crate) async fn registered_project_roots_from(global: &GlobalDb) -> Option<Vec<PathBuf>> {
    let mut roots = global.try_list_project_paths().await.ok()?;
    roots.extend(global.try_list_code_project_paths(usize::MAX).await.ok()?);
    roots.extend(global.try_list_project_alias_paths().await.ok()?);
    roots.sort();
    roots.dedup();
    Some(roots)
}

/// Ingests Codex sessions that have no registered-project attribution into the
/// profile user session store. `session_id` bounds live hook work to one host
/// session; `None` performs historical backfill.
pub async fn ingest_user_codex_sessions(session_id: Option<String>) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    let Some(db) = open_user_session_db(&profile_root).await else {
        return TranscriptIngestStats::default();
    };
    match try_ingest_user_codex_sessions_with_db(&db, &profile_root, session_id, registered_roots)
        .await
    {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(reason_code = "transcript_store_read_failed", error = %error, "Codex transcript catch-up failed");
            TranscriptIngestStats::default()
        }
    }
}

pub(crate) async fn try_ingest_user_codex_sessions_with_db(
    db: &GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    let Some(source) = codex::CodexSource::new() else {
        return Ok(TranscriptIngestStats::default());
    };
    let source = source.for_user_scope(session_id, registered_roots);
    try_ingest_source(db, &source, profile_root, None).await
}

pub async fn ingest_user_cursor_sessions() -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    let Some(db) = open_user_session_db(&profile_root).await else {
        return TranscriptIngestStats::default();
    };
    match try_ingest_user_cursor_sessions_with_db(&db, &profile_root, registered_roots).await {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(reason_code = "transcript_store_read_failed", error = %error, "Cursor transcript catch-up failed");
            TranscriptIngestStats::default()
        }
    }
}

async fn try_ingest_user_cursor_sessions_with_db(
    db: &GlobalDb,
    profile_root: &Path,
    registered_roots: Vec<PathBuf>,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    let (composer_stats, owned) = if let Some(source) = cursor_composer::CursorComposerSource::new()
    {
        let outcome = source
            .ingest_user(
                db,
                &registered_roots,
                cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
            )
            .await;
        (
            TranscriptIngestStats {
                sessions_upserted: outcome.sessions_upserted,
                messages_upserted: outcome.messages_upserted,
            },
            outcome.owned_session_ids,
        )
    } else {
        (
            TranscriptIngestStats::default(),
            std::collections::HashSet::default(),
        )
    };
    let Some(source) = cursor::CursorSweepSource::new() else {
        return Ok(composer_stats);
    };
    let source = source
        .with_skip_session_ids(owned)
        .for_user_scope(&registered_roots);
    Ok(composer_stats.merge(try_ingest_source(db, &source, profile_root, None).await?))
}

pub async fn ingest_user_global_sources() -> TranscriptIngestStats {
    ingest_user_global_sources_for_provider(None).await
}

pub(super) fn provider_selected(
    scope: Option<SessionProvider>,
    candidate: SessionProvider,
) -> bool {
    scope.is_none() || scope == Some(candidate)
}

/// Keeps the profile-level session store current without touching providers
/// outside an explicitly requested message-search scope.
pub async fn ingest_user_global_sources_for_provider(
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    let Some(db) = open_user_session_db(&profile_root).await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(&db, &profile_root, provider, roots)
        .await
        .stats
}

pub(crate) async fn ingest_user_global_sources_for_provider_at(
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Some(roots) = try_registered_project_roots_at(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(&db, profile_root, provider, roots)
        .await
        .stats
}

pub(crate) async fn ingest_user_global_sources_for_provider_at_with_db(
    db: &GlobalDb,
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestOutcome {
    let Some(roots) = try_registered_project_roots_at(profile_root).await else {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_registry",
                "project_registry_unavailable",
                true,
            )],
        );
    };
    ingest_user_global_sources_for_provider_with_roots(db, profile_root, provider, roots).await
}

pub(crate) async fn ingest_user_global_sources_for_provider_with_authorities(
    db: &GlobalDb,
    registry_db: &GlobalDb,
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestOutcome {
    let Some(roots) = registered_project_roots_from(registry_db).await else {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_registry",
                "project_registry_unavailable",
                true,
            )],
        );
    };
    ingest_user_global_sources_for_provider_with_roots(db, profile_root, provider, roots).await
}

pub(super) async fn ingest_user_global_sources_for_provider_with_roots(
    db: &GlobalDb,
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
) -> TranscriptIngestOutcome {
    let mut stats = TranscriptIngestStats::default();
    let mut failures = Vec::new();
    if provider_selected(provider, SessionProvider::Codex) {
        match try_ingest_user_codex_sessions_with_db(db, profile_root, None, roots.clone()).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => {
                let failure = classify_transcript_ingest_failure("codex", "transcript", &error);
                tracing::warn!(reason_code = failure.reason_code, retryable = failure.retryable, error = %error, "Codex transcript catch-up failed");
                failures.push(failure);
            }
        }
    }
    if provider_selected(provider, SessionProvider::Cursor) {
        match try_ingest_user_cursor_sessions_with_db(db, profile_root, roots.clone()).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => {
                let failure = classify_transcript_ingest_failure("cursor", "transcript", &error);
                tracing::warn!(reason_code = failure.reason_code, retryable = failure.retryable, error = %error, "Cursor transcript catch-up failed");
                failures.push(failure);
            }
        }
    }
    if provider_selected(provider, SessionProvider::Hermes) {
        stats = stats.merge(hermes::ingest_user_sessions(db, &roots).await);
    }
    if provider_selected(provider, SessionProvider::Claude) {
        match claude_observation::ingest_user_sessions(
            db,
            profile_root,
            None,
            roots.clone(),
            None,
            crate::application::observation::ObservationCancellation::default(),
        )
        .await
        {
            Ok(observation_stats) => {
                stats = stats.merge(observation_stats.transcript);
            }
            Err(error) => {
                let failure = claude_catch_up_failure("observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Claude observation catch-up failed"
                );
                failures.push(failure);
            }
        }
    }
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    if provider_selected(provider, SessionProvider::Vibe)
        && let Some(source) = vibe::VibeSource::new()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Cline)
        && let Some(source) = cline_like::ClineLikeSource::cline()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::RooCode)
        && let Some(source) = cline_like::ClineLikeSource::roo_code()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Kilo)
        && let Some(source) = cline_like::ClineLikeSource::kilo()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Kiro)
        && let Some(source) = kiro::KiroSource::new()
    {
        sources.push(Box::new(source.for_user_scope(roots)));
    }
    for source in sources {
        let provider = source.provider();
        match try_ingest_source(db, source.as_ref(), profile_root, None).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => {
                let failure = classify_transcript_ingest_failure(provider, "transcript", &error);
                tracing::warn!(provider, reason_code = failure.reason_code, retryable = failure.retryable, error = %error, "user transcript catch-up failed");
                failures.push(failure);
            }
        }
    }
    if stats.messages_upserted > 0 {
        crate::hooks::schedule_user_session_review(
            provider.map_or("all", SessionProvider::id),
            None,
        );
    }
    TranscriptIngestOutcome::new(stats, failures)
}
