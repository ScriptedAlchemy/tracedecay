use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_temporal_query::execution::ExecutionControl;

use tracedecay_domain::SessionId;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::{
    LcmCompressionOutcome, LcmCompressionRequest, LcmCompressionResponse, LcmError,
    LcmSummarizerMode,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_lcm::{LcmSessionBoundaryRequest, LcmSessionBoundaryResponse};
use tracedecay_session_temporal_store::SessionTemporalAccess;
use tracedecay_sessions::serving::SessionRefreshWorkerPort;

pub(super) const LCM_EFFECT_CEILING: Duration =
    tracedecay_daemon_protocol::DEFAULT_DAEMON_OPERATION_DEADLINE;
const LCM_EFFECT_WORK_LIMIT: usize = 4_096;

/// Daemon-owned execution boundary for retained LCM mutations.
///
/// The database authority remains lower-level storage. Host and MCP adapters
/// call this service so a disconnect or deadline can still roll back the open
/// transaction before its commit checkpoint.
#[derive(Clone)]
pub(super) struct DaemonLcmEffectService {
    db: RegisteredGlobalDbLeaseV1,
    control: LcmEffectControl,
    projection: Option<Arc<dyn SessionRefreshWorkerPort>>,
}

#[derive(Clone)]
struct LcmEffectControl {
    cancellation: Option<CancellationSignal>,
    expires_at: std::time::Instant,
}

impl LcmEffectControl {
    fn new(deadline: Option<&Deadline>, cancellation: Option<&CancellationSignal>) -> Self {
        let budget = deadline
            .and_then(tracedecay_daemon_protocol::deadline_remaining)
            .map_or(LCM_EFFECT_CEILING, |remaining| {
                remaining.min(LCM_EFFECT_CEILING)
            });
        Self {
            cancellation: cancellation.cloned(),
            expires_at: std::time::Instant::now() + budget,
        }
    }

    fn checkpoint(&self) -> Result<(), LcmError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationSignal::is_cancelled)
        {
            return Err(LcmError::Cancelled);
        }
        if std::time::Instant::now() >= self.expires_at {
            return Err(LcmError::DeadlineExceeded);
        }
        Ok(())
    }

    async fn execute<T: Send>(
        &self,
        execution: &ExecutionControl,
        mutation: Pin<Box<dyn Future<Output = Result<T, LcmError>> + Send + '_>>,
    ) -> Result<T, LcmError> {
        self.checkpoint()?;
        let result = mutation.await;
        if result.is_err() {
            execution.cancel();
        }
        result
    }

    fn execution_control(&self) -> ExecutionControl {
        let control =
            ExecutionControl::new(Some(self.expires_at)).with_work_limit(LCM_EFFECT_WORK_LIMIT);
        if self.checkpoint().is_err() {
            control.cancel();
        }
        control
    }

    fn remaining(&self) -> Result<Duration, LcmError> {
        self.checkpoint()?;
        Ok(self
            .expires_at
            .saturating_duration_since(std::time::Instant::now()))
    }
}

impl DaemonLcmEffectService {
    pub(super) fn new(
        db: RegisteredGlobalDbLeaseV1,
        deadline: Option<&Deadline>,
        cancellation: Option<&CancellationSignal>,
        projection: Option<Arc<dyn SessionRefreshWorkerPort>>,
    ) -> Self {
        Self {
            db,
            control: LcmEffectControl::new(deadline, cancellation),
            projection,
        }
    }

    pub(super) async fn compress(
        &self,
        mut request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        // Observation-projected sessions land raw rows without ingest
        // protection; the verified raw loads inside compression reject them.
        // Hydrate the canonical sanitization receipts first so the daemon
        // journey consumes the same protected shape as transcript ingest.
        let execution = self.control.execution_control();
        self.control
            .execute(
                &execution,
                Box::pin(tracing::Instrument::instrument(
                    self.db
                        .lcm_protect_session_raw_messages(&request.provider, &request.session_id),
                    tracing::trace_span!("daemon.lcm.hydrate"),
                )),
            )
            .await?;
        if request.summarizer.has_nonempty_provided_summary() {
            return self.commit_compression(&request, None).await;
        }

        // The message corpus is owned once by `request`; only `summarizer`
        // changes between the planning pass and the final commit.
        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let mut pending = self.commit_compression(&request, None).await?;
        let mut sources_changed = false;
        loop {
            let summary_request = match pending.outcome()? {
                LcmCompressionOutcome::NeedsSummary(summary_request) => summary_request,
                LcmCompressionOutcome::Complete | LcmCompressionOutcome::Deferred { .. } => {
                    return Ok(pending);
                }
            };
            let resolved = if sources_changed {
                // Native compaction evidence predates this revision. Only a
                // provider run over the new request can summarize its new bytes.
                super::lcm_summarization::generate_provider_summary(
                    &self.db,
                    &request.provider,
                    summary_request,
                    self.control.remaining()?,
                )
                .await
            } else {
                super::lcm_summarization::resolve_authoritative_summary(
                    &self.db,
                    &request.provider,
                    &request.session_id,
                    summary_request,
                    self.control.remaining()?,
                    None,
                )
                .await
            };
            let summary = match resolved {
                Ok(summary) => summary,
                Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                    return Err(error);
                }
                Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                    self.control.checkpoint()?;
                    return pending.defer_for_authoritative_summary(reason);
                }
            };
            self.control.checkpoint()?;
            request.summarizer = LcmSummarizerMode::Provided {
                summary_text: summary.text,
                route: Some(summary.route),
                extraction_result: None,
            };
            // Publication reuses the text only while these exact sources still
            // match. A changed selection returns the canonical NeedsSummary
            // outcome and regenerates within this call's original deadline.
            pending = self
                .commit_compression(&request, Some(&summary_request.source_messages))
                .await?;
            sources_changed = true;
        }
    }

    pub(super) async fn compress_retained_page(
        &self,
        mut request: LcmCompressionRequest,
        convergence_candidate: &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        if request.summarizer.has_nonempty_provided_summary() {
            return self
                .commit_retained_compression(&request, Some(convergence_candidate), None)
                .await;
        }

        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let pending = self
            .commit_retained_compression(&request, Some(convergence_candidate), None)
            .await?;
        let summary_request = match pending.response.outcome()? {
            LcmCompressionOutcome::NeedsSummary(summary_request) => summary_request,
            LcmCompressionOutcome::Complete | LcmCompressionOutcome::Deferred { .. } => {
                return Ok(pending);
            }
        };
        // Host-native compaction text is usable for retained convergence only
        // when its evidence binds the exact raw source range selected by this
        // page. Otherwise the provider authority summarizes source_messages.
        let required_native_source_range = summary_request.source_range.clone();
        let summary = match super::lcm_summarization::resolve_authoritative_summary(
            &self.db,
            &request.provider,
            &request.session_id,
            summary_request,
            self.control.remaining()?,
            Some(&required_native_source_range),
        )
        .await
        {
            Ok(summary)
                if summary.source_range.interval() == Some(&required_native_source_range) =>
            {
                summary
            }
            Ok(summary) => {
                self.control.checkpoint()?;
                let mut pending = pending;
                // An absent interval is its own typed state; only a present
                // interval that binds a different range is a mismatch.
                pending.response = pending.response.defer_for_authoritative_summary(
                    summary
                        .source_range
                        .absent_reason()
                        .unwrap_or("authoritative_summary_source_mismatch"),
                )?;
                return Ok(pending);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                return Err(error);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                self.control.checkpoint()?;
                let mut pending = pending;
                pending.response = pending.response.defer_for_authoritative_summary(reason)?;
                return Ok(pending);
            }
        };
        self.control.checkpoint()?;
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary.text,
            route: Some(summary.route),
            extraction_result: None,
        };
        let mut committed = self
            .commit_retained_compression(
                &request,
                Some(convergence_candidate),
                Some(&required_native_source_range),
            )
            .await?;
        committed.rows_scanned = committed.rows_scanned.saturating_add(pending.rows_scanned);
        committed.bytes_scanned = committed
            .bytes_scanned
            .saturating_add(pending.bytes_scanned);
        committed.has_more |= pending.has_more;
        Ok(committed)
    }

    async fn commit_compression(
        &self,
        request: &LcmCompressionRequest,
        expected_sources: Option<&[tracedecay_lcm::LcmSummarySourceMessage]>,
    ) -> Result<LcmCompressionResponse, LcmError> {
        loop {
            let execution = self.control.execution_control();
            let before_commit = self.control.clone();
            let result = self
                .control
                .execute(
                    &execution,
                    Box::pin(tracing::Instrument::instrument(
                        self.db.lcm_compress_guarded(
                            request,
                            expected_sources,
                            &execution,
                            move || before_commit.checkpoint(),
                        ),
                        tracing::trace_span!("daemon.lcm.commit"),
                    )),
                )
                .await;
            match result {
                Err(error @ LcmError::StaleSummaryGeneration { expected, actual })
                    if actual > expected =>
                {
                    let Some(projection) = &self.projection else {
                        return Err(error);
                    };
                    let session_id = SessionId::new(request.session_id.clone())
                        .map_err(|error| LcmError::Db(error.to_string()))?;
                    let settled = projection
                        .wake_and_wait_for_session_projection(
                            &session_id,
                            self.control.remaining()?,
                        )
                        .await;
                    self.control.checkpoint()?;
                    if !settled {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
    }

    async fn commit_retained_compression(
        &self,
        request: &LcmCompressionRequest,
        convergence_candidate: Option<
            &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
        >,
        expected_summary_source_range: Option<&tracedecay_lcm::LcmSummarySourceRange>,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        // A retained pass can perform one planning scan and one commit scan.
        // Split the existing page budgets between those phases so the whole
        // service call, rather than each internal phase, remains bounded.
        const RETAINED_PHASE_ROWS: usize = tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize / 2;
        const RETAINED_PHASE_BYTES: u64 = tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64 / 2;
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        let compressed: Pin<
            Box<
                dyn Future<
                        Output = Result<
                            tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse,
                            LcmError,
                        >,
                    > + Send,
            >,
        > = Box::pin(self.db.lcm_compress_retained_page_guarded(
            request,
            &execution,
            move || before_commit.checkpoint(),
            tracedecay_lcm::compression::RetainedCompressionGuard {
                row_limit: RETAINED_PHASE_ROWS,
                byte_limit: RETAINED_PHASE_BYTES,
                expected_summary_source_range: expected_summary_source_range.cloned(),
            },
            convergence_candidate,
        ));
        self.control
            .execute(
                &execution,
                Box::pin(tracing::Instrument::instrument(
                    compressed,
                    tracing::trace_span!("daemon.lcm.retained_commit"),
                )),
            )
            .await
    }

    pub(super) async fn recover_retained_relation_projection_page(
        &self,
    ) -> Result<tracedecay_session_temporal_store::SessionRelationRecoveryPage, LcmError> {
        const RELATION_PAGE_LIMIT: usize = 1;
        let execution = self.control.execution_control();
        let recovered = self
            .control
            .execute(&execution, Box::pin(async {
                SessionTemporalAccess::new(&*self.db)
                    .recover_pending_session_relation_projection_page(
                        RELATION_PAGE_LIMIT,
                        tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                            &execution,
                        ),
                    )
                    .await
                    .map_err(|error| match error {
                        tracedecay_store::SessionStoreError::Cancelled => LcmError::Cancelled,
                        tracedecay_store::SessionStoreError::DeadlineExceeded => {
                            LcmError::DeadlineExceeded
                        }
                        error => LcmError::Db(format!(
                            "recover bounded retained LCM relation projection: {error}"
                        )),
                    })
            }))
            .await?;
        self.control.checkpoint()?;
        Ok(recovered)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub(super) async fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                Box::pin(tracing::Instrument::instrument(
                    self.db
                        .lcm_session_boundary_guarded(request, move || before_commit.checkpoint()),
                    tracing::trace_span!("daemon.lcm.boundary"),
                )),
            )
            .await
    }
}

#[cfg(feature = "test-helpers")]
#[doc(hidden)]
pub async fn lcm_compress_for_test(
    db: RegisteredGlobalDbLeaseV1,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    DaemonLcmEffectService::new(db, None, None, None)
        .compress(request)
        .await
}

#[cfg(feature = "test-helpers")]
#[doc(hidden)]
pub async fn lcm_session_boundary_for_test(
    db: RegisteredGlobalDbLeaseV1,
    request: LcmSessionBoundaryRequest,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    DaemonLcmEffectService::new(db, None, None, None)
        .session_boundary(request)
        .await
}

#[cfg(test)]
mod tests;
