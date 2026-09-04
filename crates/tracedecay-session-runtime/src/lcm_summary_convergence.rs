//! Restart-safe background convergence of retained LCM summary state.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::summary_convergence::{
    LcmSummaryConvergenceCandidate, LcmSummaryConvergenceQueueState,
};
use tracedecay_lcm::{LcmCompressionRequest, LcmError, LcmSummarizerMode};

use crate::session_temporal_refresh_scheduler::registry::session_refresh_retry_delay;
use crate::session_temporal_refresh_scheduler::wake::SessionTemporalRefreshRetryClass;

pub(crate) const LCM_SUMMARY_CONVERGENCE_PAGE_LIMIT: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LcmSummaryConvergenceDisposition {
    Preparing,
    Summarized,
    Current,
    Pending { reason: String },
    Retryable { reason: String },
    Permanent { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LcmSummaryConvergenceSession {
    pub(crate) provider: String,
    pub(crate) session_id: String,
    pub(crate) summary_nodes_created: usize,
    pub(crate) disposition: LcmSummaryConvergenceDisposition,
    pub(crate) protection_rows_scanned: usize,
    pub(crate) protection_bytes_scanned: u64,
    pub(crate) compression_rows_scanned: usize,
    pub(crate) compression_bytes_scanned: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LcmSummaryConvergencePage {
    pub(crate) sessions: Vec<LcmSummaryConvergenceSession>,
    pub(crate) has_more: bool,
    pub(crate) next_retry_delay: Option<Duration>,
    pub(crate) backfill_rows_scanned: usize,
}

#[hotpath::measure(label = "daemon.lcm.summary_convergence.page", future = true)]
pub(crate) async fn run_summary_convergence_page(
    database: RegisteredGlobalDbLeaseV1,
    page_limit: usize,
) -> Result<LcmSummaryConvergencePage, LcmError> {
    // Relation effects from a prior summary transaction are their own
    // session-bounded, work-budgeted page. The scheduler calls this function
    // only while holding the daemon-wide historical-work admission.
    super::lcm_effects::DaemonLcmEffectService::new(database.clone(), None, None)
        .recover_retained_relation_projection_page()
        .await?;
    let backfill = backfill_queue(&database).await?;
    let now_unix_ms = unix_millis()?;
    let page_limit = page_limit.max(1);
    let mut sessions = Vec::with_capacity(page_limit);
    for _ in 0..page_limit {
        let Some(candidate) = load_candidate(&database, now_unix_ms).await? else {
            break;
        };
        sessions.push(process_candidate(&database, candidate, now_unix_ms).await?);
    }
    let next_retry_delay = load_next_retry(&database)
        .await?
        .map(|retry_at| {
            u64::try_from(retry_at.saturating_sub(now_unix_ms))
                .map(Duration::from_millis)
                .map_err(|error| LcmError::Db(format!("invalid retry delay: {error}")))
        })
        .transpose()?;
    let has_more = backfill.has_more || !sessions.is_empty();
    Ok(LcmSummaryConvergencePage {
        sessions,
        has_more,
        next_retry_delay,
        backfill_rows_scanned: backfill.rows_scanned,
    })
}

async fn backfill_queue(
    database: &RegisteredGlobalDbLeaseV1,
) -> Result<tracedecay_lcm::summary_convergence::LcmSummaryQueueBackfillPage, LcmError> {
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let page = tracedecay_lcm::summary_convergence::backfill_queue_page(
        &transaction,
        tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    Ok(page)
}

async fn load_candidate(
    database: &RegisteredGlobalDbLeaseV1,
    now_unix_ms: i64,
) -> Result<Option<LcmSummaryConvergenceCandidate>, LcmError> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    tracedecay_lcm::summary_convergence::next_candidate(&snapshot, now_unix_ms).await
}

async fn load_next_retry(database: &RegisteredGlobalDbLeaseV1) -> Result<Option<i64>, LcmError> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    tracedecay_lcm::summary_convergence::next_retry_at_ms(&snapshot).await
}

async fn process_candidate(
    database: &RegisteredGlobalDbLeaseV1,
    candidate: LcmSummaryConvergenceCandidate,
    now_unix_ms: i64,
) -> Result<LcmSummaryConvergenceSession, LcmError> {
    let protection = match database
        .lcm_protect_session_raw_messages_page(
            &candidate.provider,
            &candidate.session_id,
            candidate.protection_frontier_store_id,
            tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize,
            tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => return record_candidate_error(database, candidate, error, now_unix_ms).await,
    };
    persist_protection(database, &candidate, protection.frontier_store_id).await?;
    if protection.has_more {
        return Ok(session_result(
            candidate,
            LcmSummaryConvergenceDisposition::Preparing,
            0,
            protection.rows_scanned,
            protection.bytes_scanned,
            0,
            0,
        ));
    }

    let bounded =
        match super::lcm_effects::DaemonLcmEffectService::new(database.clone(), None, None)
            .compress_retained_page(compression_request(&candidate), &candidate)
            .await
        {
            Ok(bounded) => bounded,
            Err(error) => {
                return record_candidate_error(database, candidate, error, now_unix_ms).await;
            }
        };
    let response = &bounded.response;
    let (state, disposition, failure_code) =
        if response.retry_status.as_deref() == Some("needs_authoritative_summary") {
            (
                LcmSummaryConvergenceQueueState::Unavailable,
                LcmSummaryConvergenceDisposition::Pending {
                    reason: response.reason.clone(),
                },
                Some(response.reason.as_str()),
            )
        } else if bounded.has_more {
            (
                LcmSummaryConvergenceQueueState::Pending,
                LcmSummaryConvergenceDisposition::Summarized,
                None,
            )
        } else if response.summary_nodes_created > 0 {
            (
                LcmSummaryConvergenceQueueState::Current,
                LcmSummaryConvergenceDisposition::Summarized,
                None,
            )
        } else {
            (
                LcmSummaryConvergenceQueueState::Current,
                LcmSummaryConvergenceDisposition::Current,
                None,
            )
        };
    if state == LcmSummaryConvergenceQueueState::Unavailable {
        persist_outcome(database, &candidate, state, failure_code, 0, 0).await?;
    }
    observe_session_outcome(&disposition, response.summary_nodes_created);
    Ok(session_result(
        candidate,
        disposition,
        response.summary_nodes_created,
        protection.rows_scanned,
        protection.bytes_scanned,
        bounded.rows_scanned,
        bounded.bytes_scanned,
    ))
}

async fn record_candidate_error(
    database: &RegisteredGlobalDbLeaseV1,
    candidate: LcmSummaryConvergenceCandidate,
    error: LcmError,
    now_unix_ms: i64,
) -> Result<LcmSummaryConvergenceSession, LcmError> {
    if matches!(
        error,
        LcmError::Cancelled | LcmError::ProfileResetRequired { .. }
    ) {
        return Err(error);
    }
    let reason = error_code(&error);
    let (state, disposition, failure_count, next_attempt_at_ms) = if is_retryable(&error) {
        let failure_count = candidate.failure_count.saturating_add(1);
        let class = match error {
            LcmError::DeadlineExceeded => SessionTemporalRefreshRetryClass::Deadline,
            _ => SessionTemporalRefreshRetryClass::Storage,
        };
        let delay = session_refresh_retry_delay(class, failure_count);
        (
            LcmSummaryConvergenceQueueState::Retryable,
            LcmSummaryConvergenceDisposition::Retryable {
                reason: reason.to_string(),
            },
            failure_count,
            now_unix_ms.saturating_add(duration_millis(delay)?),
        )
    } else {
        (
            LcmSummaryConvergenceQueueState::Permanent,
            LcmSummaryConvergenceDisposition::Permanent {
                reason: reason.to_string(),
            },
            candidate.failure_count.saturating_add(1),
            0,
        )
    };
    persist_outcome(
        database,
        &candidate,
        state,
        Some(reason),
        failure_count,
        next_attempt_at_ms,
    )
    .await?;
    Ok(session_result(candidate, disposition, 0, 0, 0, 0, 0))
}

fn is_retryable(error: &LcmError) -> bool {
    matches!(
        error,
        LcmError::Db(_)
            | LcmError::Io(_)
            | LcmError::DeadlineExceeded
            | LcmError::StaleSummaryGeneration { .. }
            | LcmError::LifecycleStateNotFound
    )
}

fn error_code(error: &LcmError) -> &'static str {
    match error {
        LcmError::ProfileResetRequired { .. } => "profile_reset_required",
        LcmError::InvalidPayloadRef => "invalid_payload_ref",
        LcmError::PayloadNotFound => "payload_not_found",
        LcmError::PayloadNotOwnedBySession => "payload_not_owned_by_session",
        LcmError::PayloadMissing => "payload_missing",
        LcmError::PayloadGcd => "payload_gcd",
        LcmError::PayloadLocked => "payload_locked",
        LcmError::PayloadIntegrityMismatch => "payload_integrity_mismatch",
        LcmError::StillReferenced => "still_referenced",
        LcmError::SummaryNodeNotFound => "summary_node_not_found",
        LcmError::SummarySourceNotOwnedBySession => "summary_source_not_owned_by_session",
        LcmError::ImmutableSummaryConflict { .. } => "immutable_summary_conflict",
        LcmError::ImmutablePayloadConflict { .. } => "immutable_payload_conflict",
        LcmError::SummaryPredecessorRequired { .. } => "summary_predecessor_required",
        LcmError::InvalidSummarySuccessor { .. } => "invalid_summary_successor",
        LcmError::SummaryCycle { .. } => "summary_cycle",
        LcmError::SummarySourceUnavailable { .. } => "summary_source_unavailable",
        LcmError::StaleSummaryGeneration { .. } => "stale_summary_generation",
        LcmError::LifecycleStateNotFound => "lifecycle_state_not_found",
        LcmError::Cancelled => "cancelled",
        LcmError::DeadlineExceeded => "deadline_exceeded",
        LcmError::BudgetExhausted => "budget_exhausted",
        LcmError::SanitizationRefused { .. } => "sanitization_refused",
        LcmError::Db(_) => "storage_unavailable",
        LcmError::Io(_) => "io_unavailable",
    }
}

async fn persist_protection(
    database: &RegisteredGlobalDbLeaseV1,
    candidate: &LcmSummaryConvergenceCandidate,
    frontier_store_id: i64,
) -> Result<(), LcmError> {
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    tracedecay_lcm::summary_convergence::record_protection_progress(
        &transaction,
        candidate,
        frontier_store_id,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))
}

async fn persist_outcome(
    database: &RegisteredGlobalDbLeaseV1,
    candidate: &LcmSummaryConvergenceCandidate,
    state: LcmSummaryConvergenceQueueState,
    failure_code: Option<&str>,
    failure_count: u32,
    next_attempt_at_ms: i64,
) -> Result<(), LcmError> {
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    tracedecay_lcm::summary_convergence::record_outcome(
        &transaction,
        candidate,
        state,
        failure_code,
        failure_count,
        next_attempt_at_ms,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))
}

fn compression_request(candidate: &LcmSummaryConvergenceCandidate) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: candidate.provider.clone(),
        session_id: candidate.session_id.clone(),
        messages: Vec::new(),
        current_tokens: None,
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        summarizer: LcmSummarizerMode::HermesAuxiliary,
    }
}

fn session_result(
    candidate: LcmSummaryConvergenceCandidate,
    disposition: LcmSummaryConvergenceDisposition,
    summary_nodes_created: usize,
    protection_rows_scanned: usize,
    protection_bytes_scanned: u64,
    compression_rows_scanned: usize,
    compression_bytes_scanned: u64,
) -> LcmSummaryConvergenceSession {
    LcmSummaryConvergenceSession {
        provider: candidate.provider,
        session_id: candidate.session_id,
        summary_nodes_created,
        disposition,
        protection_rows_scanned,
        protection_bytes_scanned,
        compression_rows_scanned,
        compression_bytes_scanned,
    }
}

fn unix_millis() -> Result<i64, LcmError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LcmError::Db(format!("system clock before unix epoch: {error}")))?;
    duration_millis(elapsed)
}

fn duration_millis(duration: Duration) -> Result<i64, LcmError> {
    i64::try_from(duration.as_millis())
        .map_err(|_| LcmError::Db("LCM convergence retry timestamp overflow".to_string()))
}

fn observe_session_outcome(
    disposition: &LcmSummaryConvergenceDisposition,
    summary_nodes_created: usize,
) {
    hotpath::gauge!("daemon.lcm.summary_convergence.sessions").inc(1.0);
    if summary_nodes_created > 0 {
        hotpath::gauge!("daemon.lcm.summary_convergence.summary_nodes")
            .inc(summary_nodes_created.min(u32::MAX as usize) as f64);
    }
    match disposition {
        LcmSummaryConvergenceDisposition::Preparing
        | LcmSummaryConvergenceDisposition::Summarized => {}
        LcmSummaryConvergenceDisposition::Current => {
            hotpath::gauge!("daemon.lcm.summary_convergence.current").inc(1.0);
        }
        LcmSummaryConvergenceDisposition::Pending { .. } => {
            hotpath::gauge!("daemon.lcm.summary_convergence.pending").inc(1.0);
        }
        LcmSummaryConvergenceDisposition::Retryable { .. } => {
            hotpath::gauge!("daemon.lcm.summary_convergence.retryable").inc(1.0);
        }
        LcmSummaryConvergenceDisposition::Permanent { .. } => {
            hotpath::gauge!("daemon.lcm.summary_convergence.permanent").inc(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_retryable;
    use tracedecay_lcm::LcmError;

    #[test]
    fn convergence_error_classification_preserves_retry_truth() {
        assert!(is_retryable(&LcmError::DeadlineExceeded));
        assert!(is_retryable(&LcmError::Db("busy".to_string())));
        assert!(!is_retryable(&LcmError::PayloadIntegrityMismatch));
        assert!(!is_retryable(&LcmError::BudgetExhausted));
        assert!(!is_retryable(&LcmError::SummarySourceUnavailable {
            source_id: "1".to_string(),
            reason: "canonical_session_message_missing".to_string(),
        }));
    }
}
