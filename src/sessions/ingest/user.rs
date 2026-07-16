use std::path::{Path, PathBuf};

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::ObservationCancellation;
use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{self, TranscriptDiscoveryBounds, TranscriptSource};
use crate::sessions::{SessionProvider, claude_observation, codex, cursor, cursor_composer};
use tracedecay_domain::ObservationScopeV1;

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, ProviderRunFold,
    TranscriptCatchUpFailure, allocate_pass_byte_budgets, classify_transcript_ingest_failure,
    observation_catch_up_failure, scheduling_write_required,
};
use super::scheduler::{
    USER_CATCH_UP_PROVIDERS, USER_INGEST_PROVIDER_FRONTIER_KEY, default_ingest_pass_bounds,
    finish_user_provider_coverage, plan_user_provider_admission, read_ingest_frontier,
    write_ingest_frontier,
};
use super::startup::TranscriptIngestOutcome;
use super::user_provider::run_user_provider;

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
    try_ingest_user_codex_sessions_with_db_bounded(
        db,
        profile_root,
        session_id,
        registered_roots,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .map(|outcome| outcome.stats)
}

pub(super) async fn try_ingest_user_codex_sessions_with_db_bounded(
    db: &GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    max_total_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> source::TranscriptIngestResult<BoundedProviderOutcome> {
    let Some(source) = codex::CodexSource::new() else {
        return Ok(BoundedProviderOutcome {
            stats: TranscriptIngestStats::default(),
            bytes_consumed: 0,
            deferred_by_byte_cap: false,
        });
    };
    let source = source.for_user_scope(session_id.clone(), registered_roots.clone());
    let discovery =
        source.discover_transcript_paths(profile_root, TranscriptDiscoveryBounds::default_walk());
    let mut remaining = max_total_new_bytes;
    let mut bytes_consumed = 0u64;
    let mut deferred_by_byte_cap = discovery.is_truncated();
    let paths = discovery.paths;
    for path in paths {
        if cancellation.is_cancelled() {
            break;
        }
        let progress = codex::try_admit_codex_jsonl_observations_for_profile(
            &path,
            db,
            session_id.as_deref(),
            &registered_roots,
            remaining,
        )
        .await?;
        deferred_by_byte_cap |= progress.source_deferred;
        bytes_consumed = bytes_consumed.saturating_add(progress.bytes_consumed);
        if let Some(available) = remaining {
            remaining = Some(available.saturating_sub(progress.bytes_consumed));
        }
    }
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(db));
    let stats =
        drain_observation_projections(&facade, &ObservationScopeV1::Profile, "codex", cancellation)
            .await?;
    Ok(BoundedProviderOutcome {
        stats,
        bytes_consumed,
        deferred_by_byte_cap,
    })
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
    try_ingest_user_cursor_sessions_with_db_bounded(db, registered_roots, None)
        .await
        .map(|outcome| outcome.stats)
}

pub(super) struct BoundedProviderOutcome {
    pub(super) stats: TranscriptIngestStats,
    pub(super) bytes_consumed: u64,
    pub(super) deferred_by_byte_cap: bool,
}

pub(super) async fn try_ingest_user_cursor_sessions_with_db_bounded(
    db: &GlobalDb,
    registered_roots: Vec<PathBuf>,
    max_new_bytes: Option<u64>,
) -> source::TranscriptIngestResult<BoundedProviderOutcome> {
    let composer = if let Some(source) = cursor_composer::CursorComposerSource::new() {
        source
            .ingest_user_capped(
                db,
                &registered_roots,
                cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
                max_new_bytes,
            )
            .await
    } else {
        cursor_composer::CursorComposerSweepOutcome::default()
    };
    let remaining = max_new_bytes.map(|limit| limit.saturating_sub(composer.bytes_consumed));
    let sweep = cursor::try_ingest_cursor_user_sweep_capped(
        db,
        &registered_roots,
        remaining,
        composer.owned_session_ids,
    )
    .await?;
    Ok(BoundedProviderOutcome {
        stats: TranscriptIngestStats {
            sessions_upserted: composer
                .sessions_upserted
                .saturating_add(sweep.sessions_upserted),
            messages_upserted: composer
                .messages_upserted
                .saturating_add(sweep.messages_upserted),
        },
        bytes_consumed: composer.bytes_consumed.saturating_add(sweep.bytes_consumed),
        deferred_by_byte_cap: composer.deferred_by_byte_cap || sweep.source_deferred,
    })
}

async fn drain_observation_projections(
    facade: &HostAdmissionFacade<'_>,
    scope: &ObservationScopeV1,
    provider: &'static str,
    cancellation: &ObservationCancellation,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    let stats = claude_observation::drain_projection_queue(facade, scope, cancellation)
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
    ingest_user_global_sources_for_provider_with_roots_bounded(
        db,
        profile_root,
        provider,
        roots,
        default_ingest_pass_bounds(),
        &ObservationCancellation::default(),
    )
    .await
    .into_transcript_outcome()
}

/// Bounded fair multi-provider user catch-up with typed coverage outcomes.
pub(super) async fn ingest_user_global_sources_for_provider_with_roots_bounded(
    db: &GlobalDb,
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
    bounds: IngestPassBounds,
    cancellation: &ObservationCancellation,
) -> IngestPassOutcome {
    let selected: Vec<SessionProvider> = USER_CATCH_UP_PROVIDERS
        .iter()
        .copied()
        .filter(|candidate| provider_selected(provider, *candidate))
        .collect();
    let Some(frontier) = read_ingest_frontier(db, USER_INGEST_PROVIDER_FRONTIER_KEY).await else {
        return IngestPassOutcome::failed(TranscriptCatchUpFailure::pass_frontier_unavailable());
    };
    let plan = plan_user_provider_admission(selected.len(), frontier, bounds);
    let mut coverage = plan.coverage;

    let mut provider_runs = ProviderRunFold::default();
    let mut attempted = 0usize;
    let mut cancelled = false;
    let budget_slots = plan
        .admitted_indices
        .len()
        .saturating_mul(bounds.retries.saturating_add(1));
    let initial_budgets = allocate_pass_byte_budgets(budget_slots, bounds);
    let mut remaining_bytes = initial_budgets
        .iter()
        .copied()
        .fold(0_u64, u64::saturating_add);
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(db));

    'providers: for &index in &plan.admitted_indices {
        if cancellation.is_cancelled() {
            cancelled = true;
            provider_runs
                .failures
                .push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
        let Some(candidate) = selected.get(index).copied() else {
            continue;
        };
        if remaining_bytes == 0 || bounds.bytes_per_unit == 0 {
            break;
        }
        attempted = attempted.saturating_add(1);
        let mut retries = 0usize;
        loop {
            let grant = remaining_bytes.min(bounds.bytes_per_unit);
            if grant == 0 {
                break 'providers;
            }
            let mut unit_result = run_user_provider(
                db,
                profile_root,
                &roots,
                &facade,
                candidate,
                grant,
                cancellation,
            )
            .await;
            let within_byte_grant = unit_result.bytes_consumed <= grant;
            unit_result.byte_bounds_enforced &= within_byte_grant;
            remaining_bytes = remaining_bytes.saturating_sub(unit_result.bytes_consumed.min(grant));
            if unit_result.succeeded() {
                provider_runs.record(unit_result);
                break;
            }
            if unit_result.retryable()
                && retries < bounds.retries
                && remaining_bytes > 0
                && !cancellation.is_cancelled()
            {
                provider_runs.record_retry(&unit_result);
                retries = retries.saturating_add(1);
                continue;
            }
            provider_runs.record(unit_result);
            break;
        }
        if cancellation.is_cancelled() {
            cancelled = true;
            provider_runs
                .failures
                .push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
    }

    if !cancelled {
        match claude_observation::drain_projection_queue(
            &facade,
            &ObservationScopeV1::Profile,
            cancellation,
        )
        .await
        {
            Ok(projection_stats) => {
                provider_runs.stats = provider_runs.stats.merge(projection_stats.transcript);
            }
            Err(error) => {
                let failure = observation_catch_up_failure("observation", "projection", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "user observation projection drain failed"
                );
                provider_runs.failures.push(failure);
            }
        }
    }
    if !cancelled {
        coverage = finish_user_provider_coverage(
            coverage,
            selected.len(),
            attempted,
            usize::try_from(provider_runs.deferred_units).unwrap_or(usize::MAX),
        );
    }
    if provider_runs.stats.messages_upserted > 0 {
        crate::hooks::schedule_user_session_review(
            provider.map_or("all", SessionProvider::id),
            None,
        );
    }

    if matches!(coverage, IngestPassCoverage::Backpressured { .. })
        && !provider_runs
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_backpressured")
    {
        provider_runs
            .failures
            .push(TranscriptCatchUpFailure::pass_backpressured());
    }

    let write = scheduling_write_required(coverage, attempted, cancelled);
    let scheduling_state_written = if write {
        write_ingest_frontier(db, USER_INGEST_PROVIDER_FRONTIER_KEY, frontier, attempted).await
    } else {
        false
    };
    if write && !scheduling_state_written {
        provider_runs
            .failures
            .push(TranscriptCatchUpFailure::pass_frontier_unavailable());
    }

    IngestPassOutcome {
        stats: provider_runs.stats,
        failures: provider_runs.failures,
        coverage,
        scheduling_state_written,
        units_admitted: u64::try_from(attempted).unwrap_or(u64::MAX),
        units_completed: provider_runs.units_completed,
        units_failed: provider_runs.units_failed,
        byte_bounds_enforced: provider_runs.byte_bounds_enforced,
    }
}
