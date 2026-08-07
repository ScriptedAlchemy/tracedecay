//! The durable run-control aggregate for admitted Work runs.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "One runtime, run control, and effect budget") requires that "every run has
//! one durable control aggregate containing immutable admitted limits and
//! snapshots plus monotonically versioned authority, cancellation, deadline
//! checkpoint, and shared budget ledger", and that "pause and cancellation
//! fence new reservations and reconcile active effects before publishing a
//! stable state".
//!
//! This module owns exactly that aggregate's shape and its legal transitions.
//! Three invariants are structural rather than documented:
//!
//! 1. **Remaining time never increases.** The plan states it flatly:
//!    "remaining time never increases after pause, human wait, retry,
//!    reconnect, failover, clock rollback, or daemon restart". Pause snapshots
//!    the remaining micros left against the admitted deadline; resume republishes
//!    a deadline exactly `remaining` micros out from the resume instant. A clock
//!    that runs backwards therefore cannot buy a run more budget, because the
//!    snapshot is taken at pause and never recomputed upward.
//! 2. **Authority is monotonic.** Every transition mints the next authority
//!    version. A caller that names a stale version is refused rather than
//!    silently applied, which is what makes a pause/resume race resolvable
//!    without reading a second store.
//! 3. **The fenced frontier is recorded, not inferred.** A pause records the
//!    exact attempt identities that were live at the moment it published, so a
//!    later reader can tell "no attempts were running" from "we did not look".

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{AttemptId, RunId, TaskId, UtcMicros};

/// Ceiling on the attempt frontier one pause records, so a pathological run
/// cannot make the control row unbounded.
pub const MAX_FENCED_WORK_ATTEMPTS: usize = 256;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkRunControlContractError {
    #[error("Work run control authority version must be non-zero")]
    InvalidAuthorityVersion,
    #[error("Work run control authority version overflowed")]
    AuthorityVersionOverflow,
    #[error("Work run control deadline checkpoint is not consistent with its instant")]
    InvalidDeadlineCheckpoint,
    #[error("Work run control fenced too many attempts")]
    TooManyFencedAttempts,
    #[error("Work run control repeats a fenced attempt identity")]
    DuplicateFencedAttempt,
    #[error("Work run is already paused")]
    AlreadyPaused,
    #[error("Work run is not paused")]
    NotPaused,
    #[error("Work run control transition moved backwards in time")]
    NonMonotonicTransition,
}

/// The published control state of one run.
///
/// There is no `Cancelled` here: cancellation is an attempt-level authority
/// that Plan 32 already owns through the lease fence, and duplicating it as a
/// run state would create a second place a run could be "over".
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "WorkRunControlStateV1")]
pub enum WorkRunControlStateV1 {
    /// New reservations are admitted.
    Running,
    /// New reservations are fenced; committed evidence is preserved.
    Paused,
}

impl WorkRunControlStateV1 {
    /// Whether this state admits a new attempt reservation.
    pub const fn admits_reservation(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Why a run was paused or resumed. A closed vocabulary keeps the reason out
/// of free text so it can be read by a projection without being a prompt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "WorkRunControlReasonV1")]
pub enum WorkRunControlReasonV1 {
    /// An authorized operator asked for the transition.
    OperatorRequest,
    /// The run is waiting on a human approval or answer.
    HumanWait,
    /// The shared budget ledger is exhausted for now.
    BudgetExhausted,
    /// Recovery or failover is reconciling the run.
    Recovery,
}

/// A monotonically versioned control authority.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
#[schemars(title = "WorkRunControlAuthorityV1")]
pub struct WorkRunControlAuthorityV1(u64);

impl WorkRunControlAuthorityV1 {
    /// The authority a freshly published control aggregate carries.
    pub const FIRST: Self = Self(1);

    pub fn new(value: u64) -> Result<Self, WorkRunControlContractError> {
        if value == 0 {
            return Err(WorkRunControlContractError::InvalidAuthorityVersion);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, WorkRunControlContractError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WorkRunControlContractError::AuthorityVersionOverflow)
    }
}

impl<'de> Deserialize<'de> for WorkRunControlAuthorityV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The run's deadline as of the last control transition.
///
/// `remaining_micros` is the authority; `deadline` is the absolute instant that
/// remaining resolves to while the run is running. Both are recorded so a
/// reader never has to recompute one from a clock it does not trust.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkRunDeadlineCheckpointV1")]
pub struct WorkRunDeadlineCheckpointV1 {
    /// The absolute deadline the run is measured against right now.
    pub deadline: UtcMicros,
    /// Micros left against that deadline at `checkpoint_at`. Never negative,
    /// and never larger than the value the previous checkpoint carried.
    pub remaining_micros: i64,
    /// The instant the checkpoint was taken.
    pub checkpoint_at: UtcMicros,
}

impl WorkRunDeadlineCheckpointV1 {
    /// Takes a checkpoint of a running run against its admitted deadline.
    ///
    /// An already-expired deadline checkpoints to zero remaining rather than a
    /// negative budget: exhaustion is a state, not a debt.
    pub fn observed(
        deadline: UtcMicros,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRunControlContractError> {
        let remaining_micros = deadline.0.saturating_sub(observed_at.0).max(0);
        Ok(Self {
            deadline,
            remaining_micros,
            checkpoint_at: observed_at,
        })
    }

    /// Republishes the checkpoint at `resumed_at` preserving exactly the
    /// remaining micros it already carried.
    fn resumed(self, resumed_at: UtcMicros) -> Result<Self, WorkRunControlContractError> {
        let deadline = resumed_at
            .0
            .checked_add(self.remaining_micros)
            .ok_or(WorkRunControlContractError::InvalidDeadlineCheckpoint)?;
        Ok(Self {
            deadline: UtcMicros(deadline),
            remaining_micros: self.remaining_micros,
            checkpoint_at: resumed_at,
        })
    }

    /// Whether the run has any admitted time left.
    pub const fn is_exhausted(self) -> bool {
        self.remaining_micros == 0
    }
}

/// One run's durable control aggregate.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkRunControlV1")]
pub struct WorkRunControlV1 {
    task_id: TaskId,
    run_id: RunId,
    state: WorkRunControlStateV1,
    authority: WorkRunControlAuthorityV1,
    deadline: WorkRunDeadlineCheckpointV1,
    reason: Option<WorkRunControlReasonV1>,
    transitioned_at: UtcMicros,
    /// The exact attempts that were live when the current state published.
    /// Empty means "none were live", never "we did not look".
    fenced_attempts: Vec<AttemptId>,
}

impl WorkRunControlV1 {
    /// Publishes the first control aggregate for an admitted run.
    ///
    /// The deadline is the run's own admitted deadline, taken from the
    /// execution snapshot the attempt was admitted under; nothing here invents
    /// or extends it.
    pub fn admitted(
        task_id: TaskId,
        run_id: RunId,
        deadline: UtcMicros,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRunControlContractError> {
        Ok(Self {
            task_id,
            run_id,
            state: WorkRunControlStateV1::Running,
            authority: WorkRunControlAuthorityV1::FIRST,
            deadline: WorkRunDeadlineCheckpointV1::observed(deadline, observed_at)?,
            reason: None,
            transitioned_at: observed_at,
            fenced_attempts: Vec::new(),
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub const fn state(&self) -> WorkRunControlStateV1 {
        self.state
    }

    pub const fn authority(&self) -> WorkRunControlAuthorityV1 {
        self.authority
    }

    pub const fn deadline(&self) -> WorkRunDeadlineCheckpointV1 {
        self.deadline
    }

    pub const fn reason(&self) -> Option<WorkRunControlReasonV1> {
        self.reason
    }

    pub const fn transitioned_at(&self) -> UtcMicros {
        self.transitioned_at
    }

    pub fn fenced_attempts(&self) -> &[AttemptId] {
        &self.fenced_attempts
    }

    /// Whether a new attempt reservation may be admitted against this run.
    pub const fn admits_reservation(&self) -> bool {
        self.state.admits_reservation()
    }

    /// Fences new reservations and records the attempt frontier that was live.
    ///
    /// Pausing an already-paused run is a typed refusal rather than an
    /// idempotent no-op, because the two answers differ: the caller that
    /// re-pauses is working from a stale reading of the authority and needs to
    /// re-read it, not be told its pause landed.
    pub fn pause(
        &self,
        reason: WorkRunControlReasonV1,
        occurred_at: UtcMicros,
        live_attempts: Vec<AttemptId>,
    ) -> Result<Self, WorkRunControlContractError> {
        if self.state == WorkRunControlStateV1::Paused {
            return Err(WorkRunControlContractError::AlreadyPaused);
        }
        if occurred_at.0 < self.transitioned_at.0 {
            return Err(WorkRunControlContractError::NonMonotonicTransition);
        }
        validate_fenced_attempts(&live_attempts)?;
        // The checkpoint is taken against the currently published deadline, so
        // the time the run spent running is spent; only the balance survives.
        let deadline = WorkRunDeadlineCheckpointV1::observed(self.deadline.deadline, occurred_at)?;
        Ok(Self {
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            state: WorkRunControlStateV1::Paused,
            authority: self.authority.next()?,
            deadline,
            reason: Some(reason),
            transitioned_at: occurred_at,
            fenced_attempts: live_attempts,
        })
    }

    /// Readmits new reservations, republishing the deadline from the exact
    /// remaining micros the pause snapshotted.
    pub fn resume(
        &self,
        reason: WorkRunControlReasonV1,
        occurred_at: UtcMicros,
    ) -> Result<Self, WorkRunControlContractError> {
        if self.state != WorkRunControlStateV1::Paused {
            return Err(WorkRunControlContractError::NotPaused);
        }
        if occurred_at.0 < self.transitioned_at.0 {
            return Err(WorkRunControlContractError::NonMonotonicTransition);
        }
        Ok(Self {
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            state: WorkRunControlStateV1::Running,
            authority: self.authority.next()?,
            deadline: self.deadline.resumed(occurred_at)?,
            reason: Some(reason),
            transitioned_at: occurred_at,
            fenced_attempts: Vec::new(),
        })
    }
}

fn validate_fenced_attempts(attempts: &[AttemptId]) -> Result<(), WorkRunControlContractError> {
    if attempts.len() > MAX_FENCED_WORK_ATTEMPTS {
        return Err(WorkRunControlContractError::TooManyFencedAttempts);
    }
    let mut seen = std::collections::BTreeSet::new();
    for attempt in attempts {
        if !seen.insert(attempt) {
            return Err(WorkRunControlContractError::DuplicateFencedAttempt);
        }
    }
    Ok(())
}

impl<'de> Deserialize<'de> for WorkRunControlV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            task_id: TaskId,
            run_id: RunId,
            state: WorkRunControlStateV1,
            authority: WorkRunControlAuthorityV1,
            deadline: WorkRunDeadlineCheckpointV1,
            reason: Option<WorkRunControlReasonV1>,
            transitioned_at: UtcMicros,
            fenced_attempts: Vec<AttemptId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        if wire.deadline.remaining_micros < 0 {
            return Err(serde::de::Error::custom(
                WorkRunControlContractError::InvalidDeadlineCheckpoint,
            ));
        }
        validate_fenced_attempts(&wire.fenced_attempts).map_err(serde::de::Error::custom)?;
        Ok(Self {
            task_id: wire.task_id,
            run_id: wire.run_id,
            state: wire.state,
            authority: wire.authority,
            deadline: wire.deadline,
            reason: wire.reason,
            transitioned_at: wire.transitioned_at,
            fenced_attempts: wire.fenced_attempts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> WorkRunControlV1 {
        WorkRunControlV1::admitted(
            TaskId::new("task.run-control").expect("task id"),
            RunId::new("run.run-control").expect("run id"),
            UtcMicros(1_000),
            UtcMicros(100),
        )
        .expect("admitted control")
    }

    #[test]
    fn an_admitted_run_admits_reservations_and_carries_its_remaining_budget() {
        let control = control();
        assert!(control.admits_reservation());
        assert_eq!(control.authority().get(), 1);
        assert_eq!(control.deadline().remaining_micros, 900);
        assert!(control.fenced_attempts().is_empty());
    }

    #[test]
    fn pausing_fences_reservations_and_records_the_live_frontier() {
        let paused = control()
            .pause(
                WorkRunControlReasonV1::OperatorRequest,
                UtcMicros(400),
                vec![AttemptId::new("attempt.one").expect("attempt id")],
            )
            .expect("pause");
        assert!(!paused.admits_reservation());
        assert_eq!(paused.authority().get(), 2);
        assert_eq!(paused.deadline().remaining_micros, 600);
        assert_eq!(paused.fenced_attempts().len(), 1);
        assert_eq!(
            paused.reason(),
            Some(WorkRunControlReasonV1::OperatorRequest)
        );
    }

    #[test]
    fn resuming_preserves_remaining_time_and_never_extends_it() {
        let paused = control()
            .pause(
                WorkRunControlReasonV1::HumanWait,
                UtcMicros(400),
                Vec::new(),
            )
            .expect("pause");
        // A long human wait: wall time moved 10x the whole admitted budget.
        let resumed = paused
            .resume(WorkRunControlReasonV1::OperatorRequest, UtcMicros(10_000))
            .expect("resume");
        assert!(resumed.admits_reservation());
        assert_eq!(resumed.authority().get(), 3);
        // The balance is preserved exactly; the wait bought nothing and cost
        // nothing.
        assert_eq!(resumed.deadline().remaining_micros, 600);
        assert_eq!(resumed.deadline().deadline, UtcMicros(10_600));
        assert!(resumed.fenced_attempts().is_empty());
    }

    #[test]
    fn a_second_pause_or_an_unpaused_resume_is_refused_rather_than_absorbed() {
        let paused = control()
            .pause(
                WorkRunControlReasonV1::OperatorRequest,
                UtcMicros(400),
                Vec::new(),
            )
            .expect("pause");
        assert_eq!(
            paused
                .pause(
                    WorkRunControlReasonV1::OperatorRequest,
                    UtcMicros(500),
                    Vec::new()
                )
                .expect_err("second pause"),
            WorkRunControlContractError::AlreadyPaused
        );
        assert_eq!(
            control()
                .resume(WorkRunControlReasonV1::OperatorRequest, UtcMicros(500))
                .expect_err("resume of a running run"),
            WorkRunControlContractError::NotPaused
        );
    }

    #[test]
    fn a_clock_that_runs_backwards_cannot_buy_budget() {
        let paused = control()
            .pause(WorkRunControlReasonV1::Recovery, UtcMicros(900), Vec::new())
            .expect("pause");
        assert_eq!(paused.deadline().remaining_micros, 100);
        assert_eq!(
            paused
                .resume(WorkRunControlReasonV1::Recovery, UtcMicros(500))
                .expect_err("backwards resume"),
            WorkRunControlContractError::NonMonotonicTransition
        );
    }

    #[test]
    fn an_expired_deadline_checkpoints_to_zero_rather_than_a_negative_budget() {
        let paused = control()
            .pause(
                WorkRunControlReasonV1::BudgetExhausted,
                UtcMicros(5_000),
                Vec::new(),
            )
            .expect("pause");
        assert_eq!(paused.deadline().remaining_micros, 0);
        assert!(paused.deadline().is_exhausted());
        let resumed = paused
            .resume(WorkRunControlReasonV1::OperatorRequest, UtcMicros(6_000))
            .expect("resume");
        assert!(resumed.deadline().is_exhausted());
        assert_eq!(resumed.deadline().deadline, UtcMicros(6_000));
    }

    #[test]
    fn the_wire_shape_round_trips_and_refuses_a_negative_balance() {
        let paused = control()
            .pause(
                WorkRunControlReasonV1::OperatorRequest,
                UtcMicros(400),
                vec![AttemptId::new("attempt.one").expect("attempt id")],
            )
            .expect("pause");
        let encoded = serde_json::to_value(&paused).expect("encode");
        let decoded: WorkRunControlV1 = serde_json::from_value(encoded.clone()).expect("decode");
        assert_eq!(decoded, paused);

        let mut broken = encoded;
        broken["deadline"]["remaining_micros"] = serde_json::json!(-1);
        assert!(serde_json::from_value::<WorkRunControlV1>(broken).is_err());
    }
}
