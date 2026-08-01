use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use tracedecay_domain::{ObservationScopeV1, ProjectId};

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{TranscriptDiscoveryBounds, TranscriptSource};
use crate::runtime::{
    SessionProvider, claude, claude_observation, cline_like, codex, cursor, cursor_composer,
    hermes, kiro, vibe,
};

use super::failure::{
    ProviderRunOutcome, classify_transcript_ingest_failure, claude_catch_up_failure,
    warn_transcript_catch_up_failure,
};

pub(super) const PROJECT_CATCH_UP_PROVIDERS: &[SessionProvider] = &[
    SessionProvider::Codex,
    SessionProvider::Kiro,
    SessionProvider::Cline,
    SessionProvider::RooCode,
    SessionProvider::Kilo,
    SessionProvider::Claude,
    SessionProvider::Cursor,
    SessionProvider::Hermes,
    SessionProvider::Vibe,
];

const MAX_CODEX_SOURCE_FAILURES_PER_PASS: usize = 8;

fn codex_source_failure_saturates_pass(failure_count: usize, retryable: bool) -> bool {
    retryable || failure_count >= MAX_CODEX_SOURCE_FAILURES_PER_PASS
}

pub(super) struct ProjectProviderRun<'a> {
    pub(super) project_root: &'a Path,
    pub(super) project_id: &'a ProjectId,
    pub(super) facade: &'a dyn HostAdmission,
    pub(super) scope: &'a ObservationScopeV1,
    pub(super) candidate: SessionProvider,
    pub(super) max_new_bytes: u64,
    pub(super) cancellation: &'a ObservationCancellation,
}

impl<'a> ProjectProviderRun<'a> {
    /// Provider-run chokepoint: boxes the whole per-provider ingest future so
    /// the project catch-up loop inherits a bounded debug poll frame and no
    /// longer pins each `run()` at the call site.
    pub(super) fn run(self) -> Pin<Box<dyn Future<Output = ProviderRunOutcome> + Send + 'a>> {
        Box::pin(async move {
            if self.cancellation.is_cancelled() {
                return ProviderRunOutcome::skipped();
            }
            match self.candidate {
                SessionProvider::Codex => self.run_codex().await,
                SessionProvider::Kiro => self.run_kiro().await,
                SessionProvider::Cline | SessionProvider::RooCode | SessionProvider::Kilo => {
                    self.run_cline_like().await
                }
                SessionProvider::Claude => self.run_claude().await,
                SessionProvider::Cursor => self.run_cursor().await,
                SessionProvider::Hermes => self.run_hermes().await,
                SessionProvider::Vibe => self.run_vibe().await,
            }
        })
    }

    async fn run_codex(self) -> ProviderRunOutcome {
        let Some(source) = codex::CodexSource::new() else {
            return ProviderRunOutcome::skipped();
        };
        let discovery = source.discover_transcript_paths(
            self.project_root,
            TranscriptDiscoveryBounds::default_walk(),
        );
        let mut remaining = self.max_new_bytes;
        let mut deferred = discovery.is_truncated();
        let mut outcome = ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 0, false);
        for path in discovery.paths {
            if self.cancellation.is_cancelled() {
                deferred = true;
                break;
            }
            match codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
                &path,
                self.project_root,
                self.project_id.clone(),
                self.facade,
                Some(remaining),
                self.cancellation,
            )
            .await
            {
                Ok(progress) => {
                    deferred |= progress.source_deferred || progress.bytes_consumed > remaining;
                    remaining = remaining.saturating_sub(progress.bytes_consumed);
                }
                Err(error) => {
                    let failure = warn_transcript_catch_up_failure(
                        "codex",
                        "observation",
                        &error,
                        "project Codex observation catch-up failed",
                    );
                    let stop = codex_source_failure_saturates_pass(
                        outcome.failures.len().saturating_add(1),
                        failure.retryable,
                    );
                    outcome.add_failure(failure);
                    if stop {
                        deferred = true;
                        break;
                    }
                }
            }
        }
        outcome.bytes_consumed = self.max_new_bytes.saturating_sub(remaining);
        outcome.add_deferred_units(u64::from(deferred));
        outcome
    }

    async fn run_kiro(self) -> ProviderRunOutcome {
        let Some(source) = kiro::KiroSource::new() else {
            return ProviderRunOutcome::skipped();
        };
        match kiro::capture_kiro_snapshot_observations(
            self.facade,
            &source,
            self.project_root,
            self.scope.clone(),
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap || outcome.bytes_consumed > self.max_new_bytes,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "kiro",
                    "observation",
                    &error,
                    "project Kiro observation catch-up failed",
                ),
                0,
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
            return ProviderRunOutcome::skipped();
        };
        match cline_like::capture_cline_like_snapshot_observations(
            self.facade,
            &source,
            self.project_root,
            self.scope.clone(),
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred_by_byte_cap || outcome.bytes_consumed > self.max_new_bytes,
            ),
            Err(error) => {
                let failure =
                    classify_transcript_ingest_failure(self.candidate.id(), "observation", &error);
                tracing::warn!(
                    provider = self.candidate.id(),
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "project snapshot observation catch-up failed"
                );
                ProviderRunOutcome::failed(failure, 0)
            }
        }
    }

    async fn run_vibe(self) -> ProviderRunOutcome {
        let Some(source) = vibe::VibeSource::new() else {
            return ProviderRunOutcome::skipped();
        };
        match vibe::capture_vibe_observations(
            self.facade,
            &source,
            self.project_root,
            self.scope.clone(),
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await
        {
            Ok(outcome) => ProviderRunOutcome::bounded(
                TranscriptIngestStats::default(),
                outcome.bytes_consumed,
                outcome.deferred || outcome.bytes_consumed > self.max_new_bytes,
            ),
            Err(error) => ProviderRunOutcome::failed(
                warn_transcript_catch_up_failure(
                    "vibe",
                    "observation",
                    &error,
                    "project Vibe observation catch-up failed",
                ),
                0,
            ),
        }
    }

    async fn run_claude(self) -> ProviderRunOutcome {
        match ingest_project_claude_observations(
            self.project_root,
            self.project_id.clone(),
            self.facade,
            self.max_new_bytes,
            self.cancellation,
        )
        .await
        {
            Ok(stats) => {
                let mut outcome = ProviderRunOutcome::bounded(
                    stats.transcript,
                    stats.source_bytes_scanned,
                    false,
                );
                outcome.add_deferred_units(
                    stats
                        .deferred_sources
                        .saturating_add(u64::from(stats.source_bytes_scanned > self.max_new_bytes)),
                );
                outcome
            }
            Err(error) => {
                let failure = claude_catch_up_failure("observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "project Claude observation catch-up failed"
                );
                ProviderRunOutcome::failed(failure, 0)
            }
        }
    }

    async fn run_cursor(self) -> ProviderRunOutcome {
        let composer = if let Some(source) = cursor_composer::CursorComposerSource::new() {
            source
                .ingest_capped_with_cancellation(
                    self.facade,
                    self.project_root,
                    self.project_id.clone(),
                    cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
                    Some(self.max_new_bytes),
                    self.cancellation,
                )
                .await
        } else {
            cursor_composer::CursorComposerSweepOutcome::default()
        };
        let mut outcome = ProviderRunOutcome::bounded(
            TranscriptIngestStats {
                sessions_upserted: composer.sessions_upserted,
                messages_upserted: composer.messages_upserted,
            },
            composer.bytes_consumed,
            composer.deferred_by_byte_cap,
        );
        if self.cancellation.is_cancelled() {
            return outcome;
        }
        let remaining = self.max_new_bytes.saturating_sub(composer.bytes_consumed);
        match cursor::try_ingest_cursor_project_sweep_capped_with_admission(
            self.project_root,
            self.project_id.clone(),
            self.facade,
            Some(remaining),
            composer.owned_session_ids,
            self.cancellation,
        )
        .await
        {
            Ok(stats) => {
                outcome.add_stats(TranscriptIngestStats {
                    sessions_upserted: stats.sessions_upserted,
                    messages_upserted: stats.messages_upserted,
                });
                outcome.bytes_consumed =
                    outcome.bytes_consumed.saturating_add(stats.bytes_consumed);
                outcome.add_deferred_units(u64::from(
                    stats.source_deferred || stats.bytes_consumed > remaining,
                ));
            }
            Err(error) => outcome.add_failure(warn_transcript_catch_up_failure(
                "cursor",
                "observation",
                &error,
                "project Cursor observation catch-up failed",
            )),
        }
        outcome
    }

    async fn run_hermes(self) -> ProviderRunOutcome {
        let outcome = hermes::ingest_for_project_capped_with_admission_and_cancellation(
            self.project_root,
            self.project_id.clone(),
            self.facade,
            Some(self.max_new_bytes),
            self.cancellation,
        )
        .await;
        ProviderRunOutcome::bounded(
            outcome.stats,
            outcome.bytes_consumed,
            outcome.deferred_by_byte_cap || outcome.bytes_consumed > self.max_new_bytes,
        )
    }
}

async fn ingest_project_claude_observations(
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: u64,
    cancellation: &ObservationCancellation,
) -> std::result::Result<
    claude_observation::ClaudeObservationIngestStats,
    claude_observation::ClaudeObservationIngestError,
> {
    let Some(source) = claude::ClaudeSource::new() else {
        return Ok(claude_observation::ClaudeObservationIngestStats::default());
    };
    claude_observation::ingest_source_with_observations_with_admission(
        &source,
        project_root,
        ObservationScopeV1::Project { project_id },
        admission,
        Some(max_new_bytes),
        cancellation.clone(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{MAX_CODEX_SOURCE_FAILURES_PER_PASS, codex_source_failure_saturates_pass};

    #[test]
    fn codex_source_failures_bound_each_provider_pass() {
        assert!(!codex_source_failure_saturates_pass(
            MAX_CODEX_SOURCE_FAILURES_PER_PASS - 1,
            false,
        ));
        assert!(codex_source_failure_saturates_pass(
            MAX_CODEX_SOURCE_FAILURES_PER_PASS,
            false,
        ));
        assert!(codex_source_failure_saturates_pass(1, true));
    }
}
