use std::time::Duration;

use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_temporal_query::ports::ExecutionControl;

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::{LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmSummarizerMode};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_lcm::{LcmSessionBoundaryRequest, LcmSessionBoundaryResponse};

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
}

#[derive(Clone)]
struct LcmEffectControl {
    cancellation: Option<CancellationSignal>,
    expires_at: tokio::time::Instant,
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
            expires_at: tokio::time::Instant::now() + budget,
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
        if tokio::time::Instant::now() >= self.expires_at {
            return Err(LcmError::DeadlineExceeded);
        }
        Ok(())
    }

    #[hotpath::skip]
    async fn execute<T>(
        &self,
        execution: &ExecutionControl,
        mutation: impl std::future::Future<Output = Result<T, LcmError>>,
    ) -> Result<T, LcmError> {
        self.checkpoint()?;
        let result = mutation.await;
        if result.is_err() {
            execution.cancel();
        }
        result
    }

    fn execution_control(&self) -> ExecutionControl {
        let control = ExecutionControl::new(Some(self.expires_at.into_std()))
            .with_work_limit(LCM_EFFECT_WORK_LIMIT);
        if self.checkpoint().is_err() {
            control.cancel();
        }
        control
    }

    fn remaining(&self) -> Result<Duration, LcmError> {
        self.checkpoint()?;
        Ok(self
            .expires_at
            .saturating_duration_since(tokio::time::Instant::now()))
    }
}

impl DaemonLcmEffectService {
    pub(super) fn new(
        db: RegisteredGlobalDbLeaseV1,
        deadline: Option<&Deadline>,
        cancellation: Option<&CancellationSignal>,
    ) -> Self {
        Self {
            db,
            control: LcmEffectControl::new(deadline, cancellation),
        }
    }

    #[hotpath::skip]
    pub(super) async fn compress(
        &self,
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let result = self.compress_phases(request).await;
        observe_compression_outcome(result.as_ref());
        result
    }

    #[hotpath::skip]
    pub(super) async fn compress_retained_page(
        &self,
        request: LcmCompressionRequest,
        convergence_candidate: &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        let result = self
            .compress_retained_phases(request, convergence_candidate)
            .await;
        observe_compression_outcome(result.as_ref().map(|bounded| &bounded.response));
        result
    }

    #[hotpath::skip]
    async fn compress_phases(
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
                hotpath::future!(
                    self.db
                        .lcm_protect_session_raw_messages(&request.provider, &request.session_id),
                    label = "daemon.lcm.hydrate"
                ),
            )
            .await?;
        if matches!(
            &request.summarizer,
            LcmSummarizerMode::Provided { summary_text, .. } if !summary_text.trim().is_empty()
        ) || matches!(&request.summarizer, LcmSummarizerMode::Fake { .. })
        {
            return self.commit_compression(&request).await;
        }

        // The message corpus is owned once by `request`; only `summarizer`
        // changes between the planning pass and the final commit.
        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let pending = self.commit_compression(&request).await?;
        if pending.status != "needs_summary" {
            return Ok(pending);
        }
        let Some(summary_request) = pending.summary_request.as_ref() else {
            return Ok(pending);
        };
        let summary = match super::lcm_summarization::resolve_authoritative_summary(
            &self.db,
            &request.provider,
            &request.session_id,
            summary_request,
            self.control.remaining()?,
            None,
        )
        .await
        {
            Ok(summary) => summary,
            Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                return Err(error);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                self.control.checkpoint()?;
                return Ok(summary_unavailable(pending, reason));
            }
        };
        self.control.checkpoint()?;
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary.text,
            route: Some(summary.route),
        };
        self.commit_compression(&request).await
    }

    async fn compress_retained_phases(
        &self,
        mut request: LcmCompressionRequest,
        convergence_candidate: &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        if matches!(
            &request.summarizer,
            LcmSummarizerMode::Provided { summary_text, .. } if !summary_text.trim().is_empty()
        ) || matches!(&request.summarizer, LcmSummarizerMode::Fake { .. })
        {
            return self
                .commit_retained_compression(&request, Some(convergence_candidate), None)
                .await;
        }

        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let pending = self
            .commit_retained_compression(&request, Some(convergence_candidate), None)
            .await?;
        if pending.response.status != "needs_summary" {
            return Ok(pending);
        }
        let Some(summary_request) = pending.response.summary_request.as_ref() else {
            return Ok(pending);
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
                pending.response = summary_unavailable(
                    pending.response,
                    summary
                        .source_range
                        .absent_reason()
                        .unwrap_or("authoritative_summary_source_mismatch"),
                );
                return Ok(pending);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                return Err(error);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                self.control.checkpoint()?;
                let mut pending = pending;
                pending.response = summary_unavailable(pending.response, reason);
                return Ok(pending);
            }
        };
        self.control.checkpoint()?;
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary.text,
            route: Some(summary.route),
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

    #[hotpath::skip]
    async fn commit_compression(
        &self,
        request: &LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_compress_guarded(request, &execution, move || {
                        before_commit.checkpoint()
                    }),
                    label = "daemon.lcm.commit"
                ),
            )
            .await
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
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_compress_retained_page_guarded(
                        request,
                        &execution,
                        move || before_commit.checkpoint(),
                        tracedecay_lcm::compression::RetainedCompressionGuard {
                            row_limit: RETAINED_PHASE_ROWS,
                            byte_limit: RETAINED_PHASE_BYTES,
                            expected_summary_source_range: expected_summary_source_range.cloned(),
                        },
                        convergence_candidate,
                    ),
                    label = "daemon.lcm.retained_commit"
                ),
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
            .execute(&execution, async {
                self.db
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
            })
            .await?;
        self.control.checkpoint()?;
        Ok(recovered)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub(super) async fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_session_boundary_guarded(request, move || {
                        before_commit.checkpoint()
                    }),
                    label = "daemon.lcm.boundary"
                ),
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
    DaemonLcmEffectService::new(db, None, None)
        .compress(request)
        .await
}

#[cfg(feature = "test-helpers")]
#[doc(hidden)]
pub async fn lcm_session_boundary_for_test(
    db: RegisteredGlobalDbLeaseV1,
    request: LcmSessionBoundaryRequest,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    DaemonLcmEffectService::new(db, None, None)
        .session_boundary(request)
        .await
}

/// Terminal compression outcomes for profiling, including deferrals and
/// failures: a lane that only counts commits hides exactly the retried and
/// cancelled work a compaction investigation needs to see. Borrows the
/// outcome so classifying a retained page never copies its response payload.
fn observe_compression_outcome(result: Result<&LcmCompressionResponse, &LcmError>) {
    match result {
        Ok(response) if response.retry_status.is_some() => {
            hotpath::gauge!("daemon.lcm.compress.deferred").inc(1.0);
        }
        Ok(response) if response.status == "needs_summary" => {
            hotpath::gauge!("daemon.lcm.compress.needs_summary").inc(1.0);
        }
        Ok(response) if response.summary_nodes_created > 0 => {
            hotpath::gauge!("daemon.lcm.compress.committed").inc(1.0);
        }
        Ok(_) => {
            hotpath::gauge!("daemon.lcm.compress.noop").inc(1.0);
        }
        Err(LcmError::Cancelled) => {
            hotpath::gauge!("daemon.lcm.compress.cancelled").inc(1.0);
        }
        Err(LcmError::DeadlineExceeded) => {
            hotpath::gauge!("daemon.lcm.compress.deadline").inc(1.0);
        }
        Err(_) => {
            hotpath::gauge!("daemon.lcm.compress.failed").inc(1.0);
        }
    }
}

fn summary_unavailable(
    mut response: LcmCompressionResponse,
    reason: &'static str,
) -> LcmCompressionResponse {
    response.reason = reason.to_string();
    response.retry_status = Some("needs_authoritative_summary".to_string());
    response
}

#[cfg(test)]
mod tests;
