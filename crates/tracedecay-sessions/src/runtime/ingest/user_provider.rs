use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tracedecay_domain::ObservationScopeV1;

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{HostProviderCoverage, persist_host_provider_coverage};
use crate::runtime::store_port::TranscriptIngestStore;
use crate::runtime::{
    SessionProvider, claude_observation, cline_like, hermes, kimi, kiro, opencode, vibe,
};

use super::failure::{
    ProviderRunOutcome, TranscriptCatchUpFailure, cancelled_claude_provider_outcome,
    cancelled_provider_outcome, classify_transcript_ingest_failure, claude_catch_up_failure,
    warn_transcript_catch_up_failure,
};
use super::scheduler::{
    USER_INGEST_CODEX_HISTORY_FRONTIER_KEY, read_ingest_frontier, write_ingest_frontier,
};
use super::user::{
    BoundedProviderFailure, BoundedProviderOutcome, try_ingest_user_codex_sessions_rotated,
    try_ingest_user_cursor_sessions_with_db_bounded,
};

fn cursor_provider_run_outcome(
    result: Result<BoundedProviderOutcome, BoundedProviderFailure>,
) -> ProviderRunOutcome {
    match result {
        Ok(outcome) => ProviderRunOutcome::bounded(
            outcome.stats,
            outcome.bytes_consumed,
            outcome.deferred_by_byte_cap,
        ),
        Err(failure) => {
            let mut outcome = ProviderRunOutcome::bounded(
                failure.outcome.stats,
                failure.outcome.bytes_consumed,
                failure.outcome.deferred_by_byte_cap,
            );
            if !failure.error.is_cancelled() {
                outcome.add_failure(warn_transcript_catch_up_failure(
                    "cursor",
                    "observation",
                    &failure.error,
                    "Cursor transcript catch-up failed",
                ));
            }
            outcome
        }
    }
}

fn claude_provider_run_outcome(
    stats: &claude_observation::ClaudeObservationIngestStats,
    error: Option<&claude_observation::ClaudeObservationIngestError>,
    max_new_bytes: u64,
) -> ProviderRunOutcome {
    let mut outcome = ProviderRunOutcome::bounded(
        stats.transcript,
        stats.source_bytes_scanned,
        stats.deferred_sources > 0 || stats.source_bytes_scanned > max_new_bytes,
    );
    if let Some(error) = error.filter(|error| !error.is_typed_cancellation()) {
        let failure = claude_catch_up_failure("observation", error);
        tracing::warn!(
            reason_code = failure.reason_code,
            retryable = failure.retryable,
            "Claude observation catch-up failed"
        );
        outcome.add_failure(failure);
    }
    outcome
}

pub(super) struct UserProviderRunResult {
    pub(super) outcome: ProviderRunOutcome,
    pub(super) claude_projected_session_ids: BTreeSet<String>,
}

impl UserProviderRunResult {
    fn provider(outcome: ProviderRunOutcome) -> Self {
        Self {
            outcome,
            claude_projected_session_ids: BTreeSet::new(),
        }
    }

    fn claude(outcome: ProviderRunOutcome, claude_projected_session_ids: BTreeSet<String>) -> Self {
        Self {
            outcome,
            claude_projected_session_ids,
        }
    }
}

pub(super) async fn run_user_provider<S: TranscriptIngestStore>(
    store: &S,
    profile_root: &Path,
    roots: &[PathBuf],
    facade: &dyn HostAdmission,
    candidate: SessionProvider,
    max_new_bytes: u64,
    cancellation: &ObservationCancellation,
) -> UserProviderRunResult {
    UserProviderUnit {
        store,
        profile_root,
        roots,
        facade,
        candidate,
        max_new_bytes,
        cancellation,
    }
    .run()
    .await
}

struct UserProviderUnit<'a, S> {
    store: &'a S,
    profile_root: &'a Path,
    roots: &'a [PathBuf],
    facade: &'a dyn HostAdmission,
    candidate: SessionProvider,
    max_new_bytes: u64,
    cancellation: &'a ObservationCancellation,
}

impl<S: TranscriptIngestStore> UserProviderUnit<'_, S> {
    async fn run(self) -> UserProviderRunResult {
        if self.cancellation.is_cancelled() {
            return UserProviderRunResult::provider(ProviderRunOutcome::skipped());
        }
        match self.candidate {
            SessionProvider::Codex => UserProviderRunResult::provider(self.run_codex().await),
            SessionProvider::Cursor => UserProviderRunResult::provider(self.run_cursor().await),
            SessionProvider::Hermes => UserProviderRunResult::provider(self.run_hermes().await),
            SessionProvider::Claude => self.run_claude().await,
            SessionProvider::Kiro => UserProviderRunResult::provider(self.run_kiro().await),
            SessionProvider::Kimi => UserProviderRunResult::provider(self.run_kimi().await),
            SessionProvider::OpenCode => UserProviderRunResult::provider(self.run_opencode().await),
            SessionProvider::Cline | SessionProvider::RooCode | SessionProvider::Kilo => {
                UserProviderRunResult::provider(self.run_cline_like().await)
            }
            SessionProvider::Vibe => UserProviderRunResult::provider(self.run_vibe().await),
        }
    }

    async fn run_codex(self) -> ProviderRunOutcome {
        // Recent-first with a durable historical rotation: the frontier picks
        // which older discovery buckets this pass covers. A missing frontier
        // (fresh store) truthfully starts the rotation at zero.
        let rotation = read_ingest_frontier(self.store, USER_INGEST_CODEX_HISTORY_FRONTIER_KEY)
            .await
            .unwrap_or(0);
        match try_ingest_user_codex_sessions_rotated(
            self.profile_root,
            None,
            self.roots.to_vec(),
            self.facade,
            Some(self.max_new_bytes),
            self.cancellation,
            rotation,
        )
        .await
        {
            Ok((outcome, next_rotation)) => {
                // A failed advance repeats the same historical window next
                // pass — at-least-once coverage, never a skipped range.
                let advance = next_rotation.saturating_sub(rotation);
                let _ = write_ingest_frontier(
                    self.store,
                    USER_INGEST_CODEX_HISTORY_FRONTIER_KEY,
                    rotation,
                    usize::try_from(advance).unwrap_or(usize::MAX),
                )
                .await;
                ProviderRunOutcome::bounded(
                    outcome.stats,
                    outcome.bytes_consumed,
                    outcome.deferred_by_byte_cap,
                )
            }
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                ProviderRunOutcome::failed(
                    warn_transcript_catch_up_failure(
                        "codex",
                        "observation",
                        &error,
                        "Codex transcript catch-up failed",
                    ),
                    self.max_new_bytes,
                )
            }
        }
    }

    async fn run_cursor(self) -> ProviderRunOutcome {
        cursor_provider_run_outcome(
            try_ingest_user_cursor_sessions_with_db_bounded(
                self.roots.to_vec(),
                self.facade,
                Some(self.max_new_bytes),
                self.cancellation,
            )
            .await,
        )
    }

    async fn run_hermes(self) -> ProviderRunOutcome {
        let outcome = hermes::ingest_user_sessions_capped_with_admission(
            self.facade,
            self.roots,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await;
        ProviderRunOutcome::bounded(
            outcome.stats,
            outcome.bytes_consumed,
            outcome.deferred_by_byte_cap,
        )
    }

    async fn run_claude(self) -> UserProviderRunResult {
        match claude_observation::ingest_user_sessions_with_admission(
            self.profile_root,
            None,
            self.roots.to_vec(),
            self.facade,
            Some(self.max_new_bytes),
            self.cancellation.clone(),
        )
        .await
        {
            Ok(observation_stats) => UserProviderRunResult::claude(
                claude_provider_run_outcome(&observation_stats, None, self.max_new_bytes),
                observation_stats.projected_session_ids().clone(),
            ),
            Err(error) => {
                if let Some(stats) = error.accumulated_stats() {
                    return UserProviderRunResult::claude(
                        claude_provider_run_outcome(stats, Some(&error), self.max_new_bytes),
                        stats.projected_session_ids().clone(),
                    );
                }
                if let Some(cancelled) = cancelled_claude_provider_outcome(&error) {
                    return UserProviderRunResult::provider(cancelled);
                }
                let failure = claude_catch_up_failure("observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Claude observation catch-up failed"
                );
                UserProviderRunResult::provider(ProviderRunOutcome::failed(
                    failure,
                    self.max_new_bytes,
                ))
            }
        }
    }

    async fn run_kiro(self) -> ProviderRunOutcome {
        let Some(source) = kiro::KiroSource::new() else {
            return ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        };
        let source = source.for_user_scope(self.roots.to_vec());
        match kiro::capture_kiro_snapshot_observations(
            self.facade,
            &source,
            self.profile_root,
            ObservationScopeV1::Profile,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap,
            ),
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                ProviderRunOutcome::failed(
                    warn_transcript_catch_up_failure(
                        "kiro",
                        "observation",
                        &error,
                        "user Kiro observation catch-up failed",
                    ),
                    self.max_new_bytes,
                )
            }
        }
    }

    async fn run_kimi(self) -> ProviderRunOutcome {
        let Some(source) = kimi::KimiSource::new() else {
            return ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        };
        let source = source.for_user_scope(self.roots.to_vec());
        match kimi::capture_kimi_observations(
            self.facade,
            &source,
            self.profile_root,
            ObservationScopeV1::Profile,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => {
                let mut run = ProviderRunOutcome::bounded(
                    TranscriptIngestStats::default(),
                    outcome.bytes_consumed,
                    outcome.deferred,
                );
                if outcome.discovery_failures > 0 {
                    run.add_failure(TranscriptCatchUpFailure::source_discovery_partial("kimi"));
                }
                run
            }
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                let mut run = ProviderRunOutcome::failed(
                    warn_transcript_catch_up_failure(
                        "kimi",
                        "observation",
                        &error,
                        "user Kimi observation catch-up failed",
                    ),
                    self.max_new_bytes,
                );
                if let Err(coverage_error) = persist_host_provider_coverage(
                    self.facade,
                    &ObservationScopeV1::Profile,
                    "kimi",
                    HostProviderCoverage::Unavailable,
                    1,
                )
                .await
                {
                    run.add_failure(warn_transcript_catch_up_failure(
                        "kimi",
                        "coverage",
                        &coverage_error,
                        "user Kimi coverage persistence failed",
                    ));
                }
                run
            }
        }
    }

    async fn run_opencode(self) -> ProviderRunOutcome {
        let Some(source) = opencode::OpenCodeSource::new_for_user(self.roots.to_vec()) else {
            return ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        };
        match opencode::capture_opencode_observations(
            self.facade,
            &source,
            ObservationScopeV1::Profile,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => {
                let mut run = ProviderRunOutcome::bounded(
                    outcome.stats,
                    outcome.bytes_consumed,
                    outcome.deferred_by_byte_cap
                        || outcome.scan_cancelled
                        || outcome.scan_input_bound_reached,
                );
                if outcome.scan_non_durable_units > 0 || outcome.scan_unavailable_units > 0 {
                    run.add_failure(TranscriptCatchUpFailure::source_scan_partial(
                        "opencode",
                        outcome.scan_unavailable_units > 0,
                    ));
                }
                run
            }
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                let mut run = ProviderRunOutcome::failed(
                    warn_transcript_catch_up_failure(
                        "opencode",
                        "observation",
                        &error,
                        "user OpenCode observation catch-up failed",
                    ),
                    self.max_new_bytes,
                );
                if let Err(coverage_error) = persist_host_provider_coverage(
                    self.facade,
                    &ObservationScopeV1::Profile,
                    "opencode",
                    HostProviderCoverage::Unavailable,
                    1,
                )
                .await
                {
                    run.add_failure(warn_transcript_catch_up_failure(
                        "opencode",
                        "coverage",
                        &coverage_error,
                        "user OpenCode coverage persistence failed",
                    ));
                }
                run
            }
        }
    }

    async fn run_cline_like(self) -> ProviderRunOutcome {
        let source = match self.candidate {
            SessionProvider::Cline => cline_like::ClineLikeSource::cline(),
            SessionProvider::RooCode => cline_like::ClineLikeSource::roo_code(),
            SessionProvider::Kilo => cline_like::ClineLikeSource::kilo(),
            _ => None,
        };
        let Some(source) = source else {
            return ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        };
        let source = source.for_user_scope(self.roots.to_vec());
        match cline_like::capture_cline_like_snapshot_observations(
            self.facade,
            &source,
            self.profile_root,
            ObservationScopeV1::Profile,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap,
            ),
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                let failure =
                    classify_transcript_ingest_failure(self.candidate.id(), "observation", &error);
                tracing::warn!(
                    provider = self.candidate.id(),
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "user snapshot observation catch-up failed"
                );
                ProviderRunOutcome::failed(failure, self.max_new_bytes)
            }
        }
    }

    async fn run_vibe(self) -> ProviderRunOutcome {
        let Some(source) = vibe::VibeSource::new() else {
            return ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        };
        let source = source.for_user_scope(self.roots.to_vec());
        match vibe::capture_vibe_observations(
            self.facade,
            &source,
            self.profile_root,
            ObservationScopeV1::Profile,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred,
            ),
            Err(error) => {
                if let Some(cancelled) = cancelled_provider_outcome(&error) {
                    return cancelled;
                }
                ProviderRunOutcome::failed(
                    warn_transcript_catch_up_failure(
                        "vibe",
                        "observation",
                        &error,
                        "user Vibe observation catch-up failed",
                    ),
                    self.max_new_bytes,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::claude_observation::{
        ClaudeObservationIngestError, ClaudeObservationIngestStats,
    };
    use crate::runtime::shared::TranscriptIngestStats;
    use crate::runtime::source::TranscriptIngestError;

    use super::super::user::{BoundedProviderFailure, BoundedProviderOutcome};
    use super::{claude_provider_run_outcome, cursor_provider_run_outcome};

    fn committed_outcome() -> BoundedProviderOutcome {
        BoundedProviderOutcome {
            stats: TranscriptIngestStats {
                sessions_upserted: 1,
                messages_upserted: 257,
            },
            bytes_consumed: 42,
            deferred_by_byte_cap: true,
        }
    }

    #[test]
    fn cancelled_composer_run_keeps_committed_user_stats() {
        let outcome = cursor_provider_run_outcome(Err(BoundedProviderFailure {
            outcome: committed_outcome(),
            error: TranscriptIngestError::Cancelled { provider: "cursor" },
        }));

        assert_eq!(outcome.stats.sessions_upserted, 1);
        assert_eq!(outcome.stats.messages_upserted, 257);
        assert_eq!(outcome.bytes_consumed, 42);
        assert_eq!(outcome.deferred_units, 1);
        assert!(outcome.failures.is_empty());
    }

    #[test]
    fn failed_composer_run_keeps_committed_user_stats_and_failure() {
        let outcome = cursor_provider_run_outcome(Err(BoundedProviderFailure {
            outcome: committed_outcome(),
            error: TranscriptIngestError::InvalidFrameState { provider: "cursor" },
        }));

        assert_eq!(outcome.stats.sessions_upserted, 1);
        assert_eq!(outcome.stats.messages_upserted, 257);
        assert_eq!(outcome.failures.len(), 1);
        assert!(!outcome.succeeded());
    }

    #[test]
    fn failed_claude_projection_termination_keeps_committed_user_stats() {
        let mut stats = ClaudeObservationIngestStats::default();
        stats.transcript = TranscriptIngestStats {
            sessions_upserted: 1,
            messages_upserted: 256,
        };
        stats.observations_committed = 256;
        stats.source_bytes_scanned = 42;
        let error = ClaudeObservationIngestError::Terminated {
            stats: Box::new(stats),
            error: Box::new(ClaudeObservationIngestError::Transcript(
                TranscriptIngestError::NonDurableRecord {
                    provider: "claude",
                    offset: 0,
                    end_offset: 0,
                    reason: "registered_authority_unavailable",
                },
            )),
        };

        let outcome = claude_provider_run_outcome(
            error.accumulated_stats().expect("projection stats"),
            Some(&error),
            64,
        );

        assert_eq!(outcome.stats.sessions_upserted, 1);
        assert_eq!(outcome.stats.messages_upserted, 256);
        assert_eq!(outcome.bytes_consumed, 42);
        assert_eq!(outcome.failures.len(), 1);
    }
}
