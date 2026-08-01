use std::path::{Path, PathBuf};

use super::authority::{IngestAdmissionBinding, SessionIngestAuthority};
use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{self, TranscriptDiscoveryBounds, TranscriptSource};
use crate::runtime::{SessionProvider, claude_observation, codex, cursor, cursor_composer};
use tracedecay_domain::{BrainId, ObservationScopeV1, UserProfileId};
use tracedecay_store::StoreShardScopeV1;

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, ProviderRunFold,
    TranscriptCatchUpFailure, allocate_pass_byte_budgets, observation_catch_up_failure,
    scheduling_write_required,
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

pub async fn registered_project_roots_from<A: SessionIngestAuthority>(
    global: &A,
) -> Option<Vec<PathBuf>> {
    let mut roots = global.registered_project_roots().await.or_else(|| {
        tracing::warn!("project registry read failed during user-global ingest");
        None
    })?;
    roots.sort();
    roots.dedup();
    Some(roots)
}

pub async fn try_ingest_user_codex_sessions_with_db_and_admission(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    admission: &dyn HostAdmission,
) -> source::TranscriptIngestResult<TranscriptIngestStats> {
    try_ingest_user_codex_sessions_with_db_bounded(
        profile_root,
        session_id,
        registered_roots,
        admission,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .map(|outcome| outcome.stats)
}

pub(super) async fn try_ingest_user_codex_sessions_with_db_bounded(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    admission: &dyn HostAdmission,
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
        let progress =
            codex::try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
                &path,
                session_id.as_deref(),
                &registered_roots,
                admission,
                remaining,
                cancellation,
            )
            .await?;
        deferred_by_byte_cap |= progress.source_deferred;
        bytes_consumed = bytes_consumed.saturating_add(progress.bytes_consumed);
        if let Some(available) = remaining {
            remaining = Some(available.saturating_sub(progress.bytes_consumed));
        }
    }
    let stats = drain_observation_projections(
        admission,
        &ObservationScopeV1::Profile,
        "codex",
        cancellation,
    )
    .await?;
    Ok(BoundedProviderOutcome {
        stats,
        bytes_consumed,
        deferred_by_byte_cap,
    })
}

pub(super) struct BoundedProviderOutcome {
    pub(super) stats: TranscriptIngestStats,
    pub(super) bytes_consumed: u64,
    pub(super) deferred_by_byte_cap: bool,
}

pub(super) async fn try_ingest_user_cursor_sessions_with_db_bounded(
    registered_roots: Vec<PathBuf>,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> source::TranscriptIngestResult<BoundedProviderOutcome> {
    if cancellation.is_cancelled() {
        return Ok(BoundedProviderOutcome {
            stats: TranscriptIngestStats::default(),
            bytes_consumed: 0,
            deferred_by_byte_cap: false,
        });
    }
    let composer = if let Some(source) = cursor_composer::CursorComposerSource::new() {
        source
            .ingest_user_capped_with_cancellation(
                admission,
                &registered_roots,
                cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
                max_new_bytes,
                cancellation,
            )
            .await
    } else {
        cursor_composer::CursorComposerSweepOutcome::default()
    };
    if cancellation.is_cancelled() {
        return Ok(BoundedProviderOutcome {
            stats: TranscriptIngestStats {
                sessions_upserted: composer.sessions_upserted,
                messages_upserted: composer.messages_upserted,
            },
            bytes_consumed: composer.bytes_consumed,
            deferred_by_byte_cap: composer.deferred_by_byte_cap,
        });
    }
    let remaining = max_new_bytes.map(|limit| limit.saturating_sub(composer.bytes_consumed));
    let sweep = cursor::try_ingest_cursor_user_sweep_capped_with_admission(
        &registered_roots,
        admission,
        remaining,
        composer.owned_session_ids,
        cancellation,
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
    facade: &dyn HostAdmission,
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

pub(super) fn provider_selected(
    scope: Option<SessionProvider>,
    candidate: SessionProvider,
) -> bool {
    scope.is_none() || scope == Some(candidate)
}

pub async fn ingest_user_global_sources_for_provider_with_authorities<A: SessionIngestAuthority>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    registry_db: &A,
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestOutcome {
    let registry_shard = &registry_db.shard_id();
    if registry_shard.brain_id != *brain_id
        || registry_shard.profile_id != *profile_id
        || registry_shard.scope != StoreShardScopeV1::Profile
    {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_registry",
                "project_registry_authority_mismatch",
                false,
            )],
        );
    }
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
    ingest_user_global_sources_for_provider_with_roots(
        brain_id,
        profile_id,
        registered,
        profile_root,
        provider,
        roots,
    )
    .await
}

pub(super) async fn ingest_user_global_sources_for_provider_with_roots<
    A: SessionIngestAuthority,
>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
) -> TranscriptIngestOutcome {
    ingest_user_global_sources_for_provider_with_roots_and_cancellation(
        brain_id,
        profile_id,
        registered,
        profile_root,
        provider,
        roots,
        &ObservationCancellation::default(),
    )
    .await
}

pub(super) async fn ingest_user_global_sources_for_provider_with_roots_and_cancellation<
    A: SessionIngestAuthority,
>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestOutcome {
    ingest_user_global_sources_for_provider_with_roots_bounded(
        (brain_id, profile_id, registered),
        profile_root,
        provider,
        roots,
        default_ingest_pass_bounds(),
        cancellation,
    )
    .await
    .into_transcript_outcome()
}

#[cfg(any(test, feature = "test-helpers"))]
pub async fn ingest_user_global_sources_for_provider_with_roots_without_registered_authority<
    A: SessionIngestAuthority,
>(
    _db: &A,
    _profile_root: &Path,
    provider: Option<SessionProvider>,
    _roots: Vec<PathBuf>,
) -> TranscriptIngestOutcome {
    TranscriptIngestOutcome::new(
        TranscriptIngestStats::default(),
        vec![TranscriptCatchUpFailure::registered_authority_unavailable(
            provider.map_or("all", SessionProvider::id),
        )],
    )
}

/// Bounded fair multi-provider user catch-up with typed coverage outcomes.
pub async fn ingest_user_global_sources_for_provider_with_roots_bounded<
    A: SessionIngestAuthority,
>(
    registered: (&BrainId, &UserProfileId, &A),
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
    bounds: IngestPassBounds,
    cancellation: &ObservationCancellation,
) -> IngestPassOutcome {
    let (brain_id, profile_id, registered) = registered;
    let shard = &registered.shard_id();
    if shard.brain_id != *brain_id
        || shard.profile_id != *profile_id
        || shard.scope != StoreShardScopeV1::ProfileSessions
    {
        return IngestPassOutcome::failed(TranscriptCatchUpFailure::new(
            provider.map_or("all", SessionProvider::id),
            "profile_sessions_authority",
            "profile_sessions_authority_mismatch",
            false,
        ));
    }
    let selected: Vec<SessionProvider> = USER_CATCH_UP_PROVIDERS
        .iter()
        .copied()
        .filter(|candidate| provider_selected(provider, *candidate))
        .collect();
    let transcript_store = registered.transcript_store();
    let Some(frontier) =
        read_ingest_frontier(&transcript_store, USER_INGEST_PROVIDER_FRONTIER_KEY).await
    else {
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
    let facade = registered.admission(IngestAdmissionBinding::Profile {
        brain_id,
        profile_id,
    });
    let facade = facade.as_ref();

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
                &transcript_store,
                profile_root,
                &roots,
                facade,
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
            facade,
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
    if cancelled {
        let deferred = u64::try_from(selected.len().saturating_sub(attempted))
            .unwrap_or(u64::MAX)
            .max(1);
        coverage = match coverage {
            IngestPassCoverage::Backpressured { rejected_units, .. } => {
                IngestPassCoverage::Backpressured {
                    admitted_units: u64::try_from(attempted).unwrap_or(u64::MAX),
                    rejected_units: rejected_units.max(deferred),
                }
            }
            IngestPassCoverage::Complete | IngestPassCoverage::Partial { .. } => {
                IngestPassCoverage::Partial {
                    deferred_units: deferred,
                }
            }
        };
    } else {
        coverage = finish_user_provider_coverage(
            coverage,
            selected.len(),
            attempted,
            usize::try_from(provider_runs.deferred_units).unwrap_or(usize::MAX),
        );
    }
    if provider_runs.stats.messages_upserted > 0 {
        crate::host_ports::session_review::schedule(
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
        write_ingest_frontier(
            &transcript_store,
            USER_INGEST_PROVIDER_FRONTIER_KEY,
            frontier,
            attempted,
        )
        .await
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
