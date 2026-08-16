use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::{TemporalCoverageCountsV1, UtcMicros};
use tracedecay_store::{
    SessionRefreshFailureCodeV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionTemporalProjectionBatchV1,
};

use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::store::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};

#[derive(Clone, Copy, Debug)]
pub(in crate::daemon) struct SessionTemporalRefreshPolicy {
    pub(super) max_begin_requests_per_pass: usize,
    pub(super) max_operations_per_pass: usize,
    pub(super) operation_deadline: Duration,
}

impl Default for SessionTemporalRefreshPolicy {
    fn default() -> Self {
        Self {
            max_begin_requests_per_pass: 32,
            max_operations_per_pass: 16,
            operation_deadline: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub(in crate::daemon) enum SessionTemporalRefreshEffect {
    Projection {
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
    },
    Fail(SessionRefreshFailureRequestV1),
    Deferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::daemon) enum SessionTemporalRefreshProjectorErrorClass {
    Retryable,
    Terminal,
}

#[derive(Debug)]
pub(in crate::daemon) struct SessionTemporalRefreshProjectorError {
    pub(super) class: SessionTemporalRefreshProjectorErrorClass,
    pub(super) code: String,
}

impl SessionTemporalRefreshProjectorError {
    pub(super) fn retryable(code: impl Into<String>) -> Self {
        Self {
            class: SessionTemporalRefreshProjectorErrorClass::Retryable,
            code: code.into(),
        }
    }

    pub(super) fn terminal(code: impl Into<String>) -> Self {
        Self {
            class: SessionTemporalRefreshProjectorErrorClass::Terminal,
            code: code.into(),
        }
    }
}

pub(in crate::daemon) type SessionTemporalRefreshProjectionFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    SessionTemporalRefreshEffect,
                    SessionTemporalRefreshProjectorError,
                >,
            > + Send
            + 'a,
    >,
>;

pub(in crate::daemon) trait SessionTemporalRefreshProjector: Send + Sync {
    fn project<'a>(
        &'a self,
        database: &'a RegisteredGlobalDbLeaseV1,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a>;
}

#[cfg(test)]
pub(super) struct DeferredSessionTemporalProjector;

#[cfg(test)]
impl SessionTemporalRefreshProjector for DeferredSessionTemporalProjector {
    fn project<'a>(
        &'a self,
        _database: &'a RegisteredGlobalDbLeaseV1,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async { Ok(SessionTemporalRefreshEffect::Deferred) })
    }
}

pub(super) struct CanonicalSessionTemporalProjector;

impl SessionTemporalRefreshProjector for CanonicalSessionTemporalProjector {
    fn project<'a>(
        &'a self,
        database: &'a RegisteredGlobalDbLeaseV1,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async move {
            match database
                .materialize_session_temporal_refresh_batch_result(&recovery)
                .await
            {
                Ok(Some((progress, batch))) => {
                    Ok(SessionTemporalRefreshEffect::Projection { progress, batch })
                }
                // Empty remaining range is a durable no-op: terminalize with an
                // empty complete progress batch instead of deferring forever.
                Ok(None) => canonical_noop_complete_effect(&recovery),
                Err(error) if error.is_storage() => Err(
                    SessionTemporalRefreshProjectorError::retryable("source_busy"),
                ),
                Err(_) => Err(SessionTemporalRefreshProjectorError::terminal(
                    "projector_failed",
                )),
            }
        })
    }
}

fn refresh_clock_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

pub(super) fn zero_refresh_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

fn canonical_noop_complete_effect(
    recovery: &SessionRefreshRecoveryV1,
) -> std::result::Result<SessionTemporalRefreshEffect, SessionTemporalRefreshProjectorError> {
    let next_batch = match recovery.restart_state() {
        SessionRefreshRestartStateV1::BeginProjection => 0,
        SessionRefreshRestartStateV1::ResumeProjection { next_batch_ordinal } => next_batch_ordinal,
        // Ready-to-complete recoveries are finalized by the pass loop; keep
        // them deferred if a projector is invoked defensively.
        SessionRefreshRestartStateV1::ReadyToComplete => {
            return Ok(SessionTemporalRefreshEffect::Deferred);
        }
    };
    let committed = recovery.target_frontier().observed_through();
    let frontier = SessionRefreshFrontierV1::new(committed, committed)
        .map_err(|_| SessionTemporalRefreshProjectorError::terminal("projector_failed"))?;
    let coverage = recovery
        .progress()
        .map_or_else(zero_refresh_coverage, |progress| *progress.coverage());
    let committed_records = recovery
        .progress()
        .map_or(0, SessionRefreshProgressV1::committed_records);
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        frontier,
        coverage,
        next_batch.saturating_add(1),
        committed_records,
        refresh_clock_micros(),
    )
    .with_source_coverage(
        recovery
            .source_coverage(committed)
            .map_err(|_| SessionTemporalRefreshProjectorError::terminal("projector_failed"))?,
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .and_then(|batch| batch.with_checkpoint(next_batch, committed, committed))
    .map_err(|_| SessionTemporalRefreshProjectorError::terminal("projector_failed"))?;
    Ok(SessionTemporalRefreshEffect::Projection { progress, batch })
}

pub(super) fn durable_projector_failure_code(code: &str) -> String {
    match SessionRefreshFailureCodeV1::new(code) {
        Ok(code) => code.as_str().to_string(),
        Err(_) => "projector_failed".to_string(),
    }
}
