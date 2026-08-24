//! Durable wall-clock spans and observed execution order for Work attempts.
//!
//! Attempt state remains owned by the canonical attempt store. Start instants
//! come from its existing effect-dispatch holder and terminal instants come
//! from sealed terminal evidence. This projection only joins those owner facts;
//! it does not infer timing from card state, list order, or browser clocks.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkEffectStateV1,
    WorkProductEventSequenceV1, WorkTerminalEvidenceV1,
};

use crate::work::work_authority;
use crate::{
    ApplicationProblem, RequestContext, SafeDiagnostic, WorkAttemptEffectStorageErrorV1,
    WorkAttemptEffectStoragePortV1, WorkAttemptListCoverageV1, WorkAttemptListV1,
};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutionSpanV1 {
    pub identity: WorkAttemptIdentityV1,
    /// Durable Work projection sequence under which execution was admitted.
    pub admitted_projection_sequence: WorkProductEventSequenceV1,
    pub state: WorkAttemptStateV1,
    pub effect_state: WorkEffectStateV1,
    pub started_at: UtcMicros,
    pub ended_at: Option<UtcMicros>,
    /// Present only when both owner instants form a valid non-negative span.
    pub wall_micros: Option<u64>,
    pub terminal_evidence_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkObservedExecutionV1 {
    /// One-based order assigned after sorting the durable terminal instants.
    pub ordinal: u32,
    pub identity: WorkAttemptIdentityV1,
    pub admitted_projection_sequence: WorkProductEventSequenceV1,
    pub observed_at: UtcMicros,
    pub state: WorkAttemptStateV1,
    pub evidence_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkObservedExecutionOrderBasisV1 {
    TerminalObservedAtThenAdmittedProjectionSequenceThenAttemptIdentity,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkExecutionTimingCoverageV1 {
    Complete,
    Partial {
        missing_dispatch: Vec<WorkAttemptIdentityV1>,
        invalid_terminal_span: Vec<WorkAttemptIdentityV1>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkExecutionHistoryV1 {
    Absent,
    Listed {
        spans: Vec<WorkExecutionSpanV1>,
        observed_order: Vec<WorkObservedExecutionV1>,
        order_basis: WorkObservedExecutionOrderBasisV1,
        attempt_coverage: WorkAttemptListCoverageV1,
        timing_coverage: WorkExecutionTimingCoverageV1,
    },
}

pub fn project_work_execution_history<S>(
    storage: &S,
    context: &RequestContext,
    attempts: WorkAttemptListV1,
) -> Result<WorkExecutionHistoryV1, ApplicationProblem>
where
    S: WorkAttemptEffectStoragePortV1,
{
    let authority = work_authority(context)?;
    let WorkAttemptListV1::Listed {
        attempts, coverage, ..
    } = attempts
    else {
        return Ok(WorkExecutionHistoryV1::Absent);
    };
    let mut spans = Vec::new();
    let mut observed = Vec::new();
    let mut missing_dispatch = Vec::new();
    let mut invalid_terminal_span = Vec::new();
    for attempt in attempts {
        let terminal = attempt.terminal();
        let ended_at = terminal.map(WorkTerminalEvidenceV1::observed_at);
        let terminal_evidence_digest = terminal.map(terminal_digest).cloned();
        if let (Some(terminal), Some(evidence_digest)) =
            (terminal, terminal_evidence_digest.clone())
        {
            observed.push(WorkObservedExecutionV1 {
                ordinal: 0,
                identity: attempt.identity().clone(),
                admitted_projection_sequence: attempt.projection_binding().event_sequence(),
                observed_at: terminal.observed_at(),
                state: attempt.state(),
                evidence_digest,
            });
        }
        let holder = match storage.load_effect_dispatch(&authority, attempt.identity()) {
            Ok(Some(holder)) => holder,
            Ok(None) | Err(WorkAttemptEffectStorageErrorV1::NotFoundOrNotAuthorized) => {
                missing_dispatch.push(attempt.identity().clone());
                continue;
            }
            Err(WorkAttemptEffectStorageErrorV1::Conflict) => {
                return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.work-execution-history.conflict".to_owned(),
                    message: "The Work execution timing authority is inconsistent.".to_owned(),
                }));
            }
            Err(WorkAttemptEffectStorageErrorV1::Unavailable) => {
                return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.work-execution-history.unavailable".to_owned(),
                    message: "The Work execution timing authority is unavailable.".to_owned(),
                }));
            }
        };
        let wall_micros = ended_at
            .and_then(|ended| ended.0.checked_sub(holder.dispatched_at().0))
            .and_then(|duration| u64::try_from(duration).ok());
        if ended_at.is_some() && wall_micros.is_none() {
            invalid_terminal_span.push(attempt.identity().clone());
        }
        spans.push(WorkExecutionSpanV1 {
            identity: attempt.identity().clone(),
            admitted_projection_sequence: attempt.projection_binding().event_sequence(),
            state: attempt.state(),
            effect_state: holder.effect_state(),
            started_at: holder.dispatched_at(),
            ended_at,
            wall_micros,
            terminal_evidence_digest,
        });
    }
    spans.sort_by(|left, right| {
        (left.started_at, &left.identity).cmp(&(right.started_at, &right.identity))
    });
    observed.sort_by(|left, right| {
        (
            left.observed_at,
            left.admitted_projection_sequence,
            &left.identity,
        )
            .cmp(&(
                right.observed_at,
                right.admitted_projection_sequence,
                &right.identity,
            ))
    });
    for (index, row) in observed.iter_mut().enumerate() {
        row.ordinal = u32::try_from(index.saturating_add(1)).map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-execution-history.overflow".to_owned(),
                message: "The Work execution history exceeded its declared bound.".to_owned(),
            })
        })?;
    }
    let timing_coverage = if missing_dispatch.is_empty() && invalid_terminal_span.is_empty() {
        WorkExecutionTimingCoverageV1::Complete
    } else {
        WorkExecutionTimingCoverageV1::Partial {
            missing_dispatch,
            invalid_terminal_span,
        }
    };
    Ok(WorkExecutionHistoryV1::Listed {
        spans,
        observed_order: observed,
        order_basis: WorkObservedExecutionOrderBasisV1::TerminalObservedAtThenAdmittedProjectionSequenceThenAttemptIdentity,
        attempt_coverage: coverage,
        timing_coverage,
    })
}

fn terminal_digest(terminal: &WorkTerminalEvidenceV1) -> &ManifestDigest {
    match terminal {
        WorkTerminalEvidenceV1::Succeeded {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::Failed {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::TimedOut {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::Cancelled {
            evidence_digest, ..
        } => evidence_digest,
    }
}
