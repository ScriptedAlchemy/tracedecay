use std::path::{Path, PathBuf};

use tracedecay_domain::ObservationScopeV1;

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::store_port::TranscriptIngestStore;
use crate::runtime::{
    SessionProvider, claude_observation, cline_like, hermes, kimi, kiro, opencode, vibe,
};

use super::failure::{
    ProviderRunOutcome, classify_transcript_ingest_failure, claude_catch_up_failure,
    warn_transcript_catch_up_failure,
};
use super::user::{
    try_ingest_user_codex_sessions_with_db_bounded, try_ingest_user_cursor_sessions_with_db_bounded,
};

pub(super) async fn run_user_provider<S: TranscriptIngestStore>(
    store: &S,
    profile_root: &Path,
    roots: &[PathBuf],
    facade: &dyn HostAdmission,
    candidate: SessionProvider,
    max_new_bytes: u64,
    cancellation: &ObservationCancellation,
) -> ProviderRunOutcome {
    UserProviderUnit {
        _store: store,
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
    _store: &'a S,
    profile_root: &'a Path,
    roots: &'a [PathBuf],
    facade: &'a dyn HostAdmission,
    candidate: SessionProvider,
    max_new_bytes: u64,
    cancellation: &'a ObservationCancellation,
}

impl<S: TranscriptIngestStore> UserProviderUnit<'_, S> {
    async fn run(self) -> ProviderRunOutcome {
        match self.candidate {
            SessionProvider::Codex => self.run_codex().await,
            SessionProvider::Cursor => self.run_cursor().await,
            SessionProvider::Hermes => self.run_hermes().await,
            SessionProvider::Claude => self.run_claude().await,
            SessionProvider::Kiro => self.run_kiro().await,
            SessionProvider::Kimi => self.run_kimi().await,
            SessionProvider::OpenCode => self.run_opencode().await,
            SessionProvider::Cline | SessionProvider::RooCode | SessionProvider::Kilo => {
                self.run_cline_like().await
            }
            SessionProvider::Vibe => self.run_vibe().await,
        }
    }

    async fn run_codex(self) -> ProviderRunOutcome {
        match try_ingest_user_codex_sessions_with_db_bounded(
            self.profile_root,
            None,
            self.roots.to_vec(),
            self.facade,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                outcome.stats,
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "codex",
                    "observation",
                    &error,
                    "Codex transcript catch-up failed",
                ),
                self.max_new_bytes,
            ),
        }
    }

    async fn run_cursor(self) -> ProviderRunOutcome {
        match try_ingest_user_cursor_sessions_with_db_bounded(
            self.roots.to_vec(),
            self.facade,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                outcome.stats,
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "cursor",
                    "observation",
                    &error,
                    "Cursor transcript catch-up failed",
                ),
                self.max_new_bytes,
            ),
        }
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

    async fn run_claude(self) -> ProviderRunOutcome {
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
            Ok(observation_stats) => ProviderRunOutcome::bounded(
                observation_stats.transcript,
                observation_stats.source_bytes_scanned,
                observation_stats.deferred_sources > 0
                    || observation_stats.source_bytes_scanned > self.max_new_bytes,
            ),
            Err(error) => {
                let failure = claude_catch_up_failure("observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Claude observation catch-up failed"
                );
                ProviderRunOutcome::failed(failure, self.max_new_bytes)
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
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "kiro",
                    "observation",
                    &error,
                    "user Kiro observation catch-up failed",
                ),
                self.max_new_bytes,
            ),
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
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "kimi",
                    "observation",
                    &error,
                    "user Kimi observation catch-up failed",
                ),
                self.max_new_bytes,
            ),
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
            Ok(outcome) => ProviderRunOutcome::bounded(
                outcome.stats,
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "opencode",
                    "observation",
                    &error,
                    "user OpenCode observation catch-up failed",
                ),
                self.max_new_bytes,
            ),
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
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "vibe",
                    "observation",
                    &error,
                    "user Vibe observation catch-up failed",
                ),
                self.max_new_bytes,
            ),
        }
    }
}
