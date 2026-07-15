use std::path::{Path, PathBuf};

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::ObservationCancellation;
use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{self, TranscriptSource, try_ingest_source};
use crate::sessions::{
    SessionProvider, claude_observation, cline_like, codex, cursor, cursor_composer, hermes, kiro,
    vibe,
};
use tracedecay_domain::ObservationScopeV1;

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
            let failure = classify_transcript_ingest_failure("codex", "observation", &error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "Codex observation catch-up failed"
            );
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
    let source = source.for_user_scope(session_id.clone(), registered_roots.clone());
    for path in source.transcript_paths(profile_root) {
        codex::try_admit_codex_jsonl_observations_for_profile(
            &path,
            db,
            session_id.as_deref(),
            &registered_roots,
            None,
        )
        .await?;
    }
    drain_observation_projections(db, "codex").await
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
    match try_ingest_user_cursor_sessions_with_db(&db, registered_roots).await {
        Ok(stats) => stats,
        Err(error) => {
            let failure = classify_transcript_ingest_failure("cursor", "observation", &error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "Cursor observation catch-up failed"
            );
            TranscriptIngestStats::default()
        }
    }
}

async fn try_ingest_user_cursor_sessions_with_db(
    db: &GlobalDb,
    registered_roots: Vec<PathBuf>,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    let owned = if let Some(source) = cursor_composer::CursorComposerSource::new() {
        source
            .ingest_user(
                db,
                &registered_roots,
                cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
            )
            .await
            .owned_session_ids
    } else {
        std::collections::HashSet::default()
    };
    let sweep =
        cursor::try_ingest_cursor_user_sweep_capped(db, &registered_roots, None, owned).await?;
    Ok(TranscriptIngestStats {
        sessions_upserted: sweep.sessions_upserted,
        messages_upserted: sweep.messages_upserted,
    })
}

async fn drain_observation_projections(
    db: &GlobalDb,
    provider: &'static str,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    let stats = claude_observation::drain_projection_queue(db, &ObservationCancellation::default())
        .await
        .map_err(|error| match error {
            claude_observation::ClaudeObservationIngestError::Transcript(error) => error,
            _ => source::TranscriptIngestError::InvalidFrameState { provider },
        })?;
    Ok(stats.transcript)
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
                let failure = classify_transcript_ingest_failure("codex", "observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Codex transcript catch-up failed"
                );
                failures.push(failure);
            }
        }
    }
    if provider_selected(provider, SessionProvider::Cursor) {
        match try_ingest_user_cursor_sessions_with_db(db, roots.clone()).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => {
                let failure = classify_transcript_ingest_failure("cursor", "observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Cursor transcript catch-up failed"
                );
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
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::new(None, Some(db)));
    if provider_selected(provider, SessionProvider::Kiro)
        && let Some(source) = kiro::KiroSource::new()
    {
        let source = source.for_user_scope(roots.clone());
        if let Err(error) = kiro::capture_kiro_snapshot_observations(
            &facade,
            &source,
            profile_root,
            ObservationScopeV1::Profile,
            None,
        )
        .await
        {
            let failure = classify_transcript_ingest_failure("kiro", "observation", &error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "user Kiro observation catch-up failed"
            );
            failures.push(failure);
        }
    }
    for (candidate, source) in [
        (SessionProvider::Cline, cline_like::ClineLikeSource::cline()),
        (
            SessionProvider::RooCode,
            cline_like::ClineLikeSource::roo_code(),
        ),
        (SessionProvider::Kilo, cline_like::ClineLikeSource::kilo()),
    ] {
        if !provider_selected(provider, candidate) {
            continue;
        }
        let Some(source) = source else {
            continue;
        };
        let source = source.for_user_scope(roots.clone());
        if let Err(error) = cline_like::capture_cline_like_snapshot_observations(
            &facade,
            &source,
            profile_root,
            ObservationScopeV1::Profile,
            None,
        )
        .await
        {
            let failure = classify_transcript_ingest_failure(candidate.id(), "observation", &error);
            tracing::warn!(
                provider = candidate.id(),
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "user snapshot observation catch-up failed"
            );
            failures.push(failure);
        }
    }

    if provider_selected(provider, SessionProvider::Vibe)
        && let Some(source) = vibe::VibeSource::new()
    {
        let source = source.for_user_scope(roots);
        match try_ingest_source(db, &source, profile_root, None).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => {
                let failure = classify_transcript_ingest_failure("vibe", "transcript", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "user Vibe transcript catch-up failed"
                );
                failures.push(failure);
            }
        }
    }

    match drain_observation_projections(db, provider.map_or("observation", SessionProvider::id))
        .await
    {
        Ok(projection_stats) => stats = stats.merge(projection_stats),
        Err(error) => {
            let failure = classify_transcript_ingest_failure("observation", "projection", &error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "user observation projection drain failed"
            );
            failures.push(failure);
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
