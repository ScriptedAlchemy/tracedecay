use std::path::Path;

use tracedecay_domain::{ObservationScopeV1, ProjectId};

use crate::application::host_admission::HostAdmissionFacade;
use crate::application::observation::ObservationCancellation;
use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{TranscriptDiscoveryBounds, TranscriptSource};
use crate::sessions::{
    SessionProvider, claude, claude_observation, cline_like, codex, cursor, cursor_composer,
    hermes, kiro,
};

use super::failure::{
    ProviderRunOutcome, classify_transcript_ingest_failure, claude_catch_up_failure,
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
];

pub(super) struct ProjectProviderRun<'a> {
    pub(super) db: &'a GlobalDb,
    pub(super) project_root: &'a Path,
    pub(super) project_id: &'a ProjectId,
    pub(super) facade: &'a HostAdmissionFacade<'a>,
    pub(super) scope: &'a ObservationScopeV1,
    pub(super) candidate: SessionProvider,
    pub(super) max_new_bytes: u64,
    pub(super) cancellation: &'a ObservationCancellation,
}

impl ProjectProviderRun<'_> {
    pub(super) async fn run(self) -> ProviderRunOutcome {
        match self.candidate {
            SessionProvider::Codex => self.run_codex().await,
            SessionProvider::Kiro => self.run_kiro().await,
            SessionProvider::Cline | SessionProvider::RooCode | SessionProvider::Kilo => {
                self.run_cline_like().await
            }
            SessionProvider::Claude => self.run_claude().await,
            SessionProvider::Cursor => self.run_cursor().await,
            SessionProvider::Hermes => self.run_hermes().await,
            SessionProvider::Vibe => ProviderRunOutcome::skipped(),
        }
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
            match codex::try_admit_codex_jsonl_observations_for_project(
                &path,
                self.db,
                self.project_root,
                self.project_id.clone(),
                Some(remaining),
            )
            .await
            {
                Ok(progress) => {
                    deferred |= progress.source_deferred || progress.bytes_consumed > remaining;
                    remaining = remaining.saturating_sub(progress.bytes_consumed);
                }
                Err(error) => {
                    let failure =
                        classify_transcript_ingest_failure("codex", "observation", &error);
                    tracing::warn!(
                        reason_code = failure.reason_code,
                        retryable = failure.retryable,
                        "project Codex observation catch-up failed"
                    );
                    outcome.add_failure(failure);
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
            Err(error) => {
                let failure = classify_transcript_ingest_failure("kiro", "observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "project Kiro observation catch-up failed"
                );
                ProviderRunOutcome::failed(failure, 0)
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

    async fn run_claude(self) -> ProviderRunOutcome {
        match ingest_project_claude_observations(
            self.db,
            self.project_root,
            self.project_id.clone(),
            self.max_new_bytes,
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
                .ingest_capped(
                    self.db,
                    self.project_root,
                    self.project_id.clone(),
                    cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
                    Some(self.max_new_bytes),
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
        let remaining = self.max_new_bytes.saturating_sub(composer.bytes_consumed);
        match cursor::try_ingest_cursor_project_sweep_capped(
            self.project_root,
            self.db,
            self.project_id.clone(),
            Some(remaining),
            composer.owned_session_ids,
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
            Err(error) => {
                let failure = classify_transcript_ingest_failure("cursor", "observation", &error);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "project Cursor observation catch-up failed"
                );
                outcome.add_failure(failure);
            }
        }
        outcome
    }

    async fn run_hermes(self) -> ProviderRunOutcome {
        let outcome = hermes::ingest_for_project_capped(
            self.db,
            self.project_root,
            self.project_id.clone(),
            Some(self.max_new_bytes),
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
    db: &GlobalDb,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: u64,
) -> std::result::Result<
    claude_observation::ClaudeObservationIngestStats,
    claude_observation::ClaudeObservationIngestError,
> {
    let Some(source) = claude::ClaudeSource::new() else {
        return Ok(claude_observation::ClaudeObservationIngestStats::default());
    };
    claude_observation::ingest_source_with_observations(
        db,
        &source,
        project_root,
        ObservationScopeV1::Project { project_id },
        Some(max_new_bytes),
        ObservationCancellation::default(),
    )
    .await
}
