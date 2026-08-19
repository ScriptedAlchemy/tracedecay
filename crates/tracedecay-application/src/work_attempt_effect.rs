//! Durable source receipts for potentially effectful Work-attempt dispatch.
//!
//! A provider process or app-server session is not itself evidence that its
//! effect is settled. This authority records the exact admitted attempt before
//! dispatch and records either a proved no-effect result or an explicit unknown
//! after terminal reconciliation. Leak adjudication can therefore distinguish
//! an unavailable source from a retained unknown effect without treating an
//! absent in-memory process holder as a fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{UtcMicros, WorkAttemptIdentityV1, WorkAuthority, WorkEffectStateV1};

use crate::work::work_authority;
use crate::{ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic};

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkAttemptEffectHolderErrorV1 {
    #[error("Work attempt effect holder has an invalid lifecycle time")]
    Invalid,
}

/// The terminal certainty retained by an exact Work-attempt effect holder.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkAttemptEffectResolutionV1 {
    /// The provider dispatch is proved not to have reached an effect boundary.
    NoEffect,
    /// The Work attempt became terminal without a source receipt that proves
    /// whether the provider effect committed.
    Unknown,
}

/// Whether dispatch persistence inserted a new source receipt or found the
/// same retained receipt. Replaying the receipt never authorizes a second
/// provider launch: a pending/unknown effect must be reconciled first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkAttemptEffectDispatchOutcomeV1 {
    Recorded(WorkAttemptEffectHolderV1),
    Replayed(WorkAttemptEffectHolderV1),
}

impl WorkAttemptEffectDispatchOutcomeV1 {
    pub const fn holder(&self) -> &WorkAttemptEffectHolderV1 {
        match self {
            Self::Recorded(holder) | Self::Replayed(holder) => holder,
        }
    }
}

/// Durable source receipt for a single exact Work-attempt dispatch lifecycle.
///
/// `dispatched_at` is written before the provider reaches its external effect
/// boundary. `resolution` remains absent while the provider is live; terminal
/// reconciliation fills it with a typed fact instead of fabricating success.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptEffectHolderV1 {
    attempt: WorkAttemptIdentityV1,
    effect_state: WorkEffectStateV1,
    dispatched_at: UtcMicros,
    deadline: UtcMicros,
    resolution: Option<WorkAttemptEffectResolutionV1>,
    resolved_at: Option<UtcMicros>,
}

impl WorkAttemptEffectHolderV1 {
    pub fn dispatched(
        attempt: WorkAttemptIdentityV1,
        effect_state: WorkEffectStateV1,
        dispatched_at: UtcMicros,
        deadline: UtcMicros,
    ) -> Result<Self, WorkAttemptEffectHolderErrorV1> {
        let holder = Self {
            attempt,
            effect_state,
            dispatched_at,
            deadline,
            resolution: None,
            resolved_at: None,
        };
        holder.validate()?;
        Ok(holder)
    }

    pub fn attempt(&self) -> &WorkAttemptIdentityV1 {
        &self.attempt
    }

    pub const fn effect_state(&self) -> WorkEffectStateV1 {
        self.effect_state
    }

    pub const fn dispatched_at(&self) -> UtcMicros {
        self.dispatched_at
    }

    pub const fn deadline(&self) -> UtcMicros {
        self.deadline
    }

    pub const fn resolution(&self) -> Option<WorkAttemptEffectResolutionV1> {
        self.resolution
    }

    pub const fn resolved_at(&self) -> Option<UtcMicros> {
        self.resolved_at
    }

    pub fn with_resolution(
        &self,
        resolution: WorkAttemptEffectResolutionV1,
        resolved_at: UtcMicros,
    ) -> Result<Self, WorkAttemptEffectHolderErrorV1> {
        let holder = Self {
            attempt: self.attempt.clone(),
            effect_state: self.effect_state,
            dispatched_at: self.dispatched_at,
            deadline: self.deadline,
            resolution: Some(resolution),
            resolved_at: Some(resolved_at),
        };
        holder.validate()?;
        Ok(holder)
    }

    pub fn validate(&self) -> Result<(), WorkAttemptEffectHolderErrorV1> {
        if self.dispatched_at.0 <= 0 || self.deadline.0 <= self.dispatched_at.0 {
            return Err(WorkAttemptEffectHolderErrorV1::Invalid);
        }
        match (self.resolution, self.resolved_at) {
            (None, None) => Ok(()),
            (Some(_), Some(resolved_at)) if resolved_at.0 >= self.dispatched_at.0 => Ok(()),
            _ => Err(WorkAttemptEffectHolderErrorV1::Invalid),
        }
    }

    pub fn is_unknown_past_deadline(
        &self,
        scan_started_at: UtcMicros,
        detection_horizon_micros: u64,
    ) -> bool {
        let Some(horizon) = i64::try_from(detection_horizon_micros).ok() else {
            return false;
        };
        let Some(leak_deadline) = self.deadline.0.checked_add(horizon) else {
            return false;
        };
        !matches!(self.effect_state, WorkEffectStateV1::Observational)
            && self.resolution != Some(WorkAttemptEffectResolutionV1::NoEffect)
            && scan_started_at.0 >= leak_deadline
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkAttemptEffectStorageErrorV1 {
    #[error("Work attempt effect holder was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work attempt effect holder conflicts with the exact dispatch")]
    Conflict,
    #[error("Work attempt effect storage is unavailable")]
    Unavailable,
}

/// Persistence owned by the exact Work-attempt authority. Implementations
/// must make the immutable dispatch identity replay-safe and settlement CASed.
pub trait WorkAttemptEffectStoragePortV1: Send + Sync {
    fn begin_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        holder: &WorkAttemptEffectHolderV1,
    ) -> Result<WorkAttemptEffectDispatchOutcomeV1, WorkAttemptEffectStorageErrorV1>;

    fn settle_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptIdentityV1,
        resolution: WorkAttemptEffectResolutionV1,
        resolved_at: UtcMicros,
    ) -> Result<WorkAttemptEffectHolderV1, WorkAttemptEffectStorageErrorV1>;

    fn load_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptEffectHolderV1>, WorkAttemptEffectStorageErrorV1>;
}

/// Application boundary used by the daemon provider runtime and leak source.
pub struct WorkAttemptEffectServiceV1<S> {
    storage: S,
}

impl<S> WorkAttemptEffectServiceV1<S>
where
    S: WorkAttemptEffectStoragePortV1,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn record_dispatch(
        &self,
        context: &RequestContext,
        attempt: WorkAttemptIdentityV1,
        effect_state: WorkEffectStateV1,
        dispatched_at: UtcMicros,
        deadline: UtcMicros,
    ) -> Result<WorkAttemptEffectDispatchOutcomeV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let holder =
            WorkAttemptEffectHolderV1::dispatched(attempt, effect_state, dispatched_at, deadline)
                .map_err(|_| invalid_holder_problem())?;
        self.storage
            .begin_effect_dispatch(&authority, &holder)
            .map_err(effect_problem)
    }

    pub fn settle(
        &self,
        context: &RequestContext,
        attempt: &WorkAttemptIdentityV1,
        resolution: WorkAttemptEffectResolutionV1,
        resolved_at: UtcMicros,
    ) -> Result<WorkAttemptEffectHolderV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        self.storage
            .settle_effect_dispatch(&authority, attempt, resolution, resolved_at)
            .map_err(effect_problem)
    }

    pub fn load(
        &self,
        context: &RequestContext,
        attempt: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptEffectHolderV1>, ApplicationProblem> {
        let authority = work_authority(context)?;
        self.storage
            .load_effect_dispatch(&authority, attempt)
            .map_err(effect_problem)
    }
}

fn effect_problem(error: WorkAttemptEffectStorageErrorV1) -> ApplicationProblem {
    match error {
        WorkAttemptEffectStorageErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkAttemptEffectStorageErrorV1::Conflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "application.work-attempt-effect.conflict".to_owned(),
                message: "The Work attempt effect receipt conflicts with its prior dispatch."
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        WorkAttemptEffectStorageErrorV1::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-attempt-effect.unavailable".to_owned(),
                message: "The Work attempt effect authority is unavailable.".to_owned(),
            })
        }
    }
}

fn invalid_holder_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-attempt-effect.invalid-holder".to_owned(),
            message: "The Work attempt effect lifecycle time is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> WorkAttemptIdentityV1 {
        serde_json::from_value(serde_json::json!({
            "task_id": "task.effect-holder",
            "run_id": "run.effect-holder",
            "attempt_id": "attempt.effect-holder"
        }))
        .expect("valid Work attempt identity fixture")
    }

    #[test]
    fn unknown_effect_requires_deadline_and_horizon_and_never_relabels_no_effect() {
        let holder = WorkAttemptEffectHolderV1::dispatched(
            identity(),
            WorkEffectStateV1::Intercepted,
            UtcMicros(10),
            UtcMicros(20),
        )
        .expect("valid dispatch receipt");
        assert!(!holder.is_unknown_past_deadline(UtcMicros(39), 20));
        assert!(holder.is_unknown_past_deadline(UtcMicros(40), 20));
        assert!(
            holder
                .with_resolution(WorkAttemptEffectResolutionV1::Unknown, UtcMicros(25))
                .expect("valid unknown reconciliation")
                .is_unknown_past_deadline(UtcMicros(40), 20)
        );
        assert!(
            !holder
                .with_resolution(WorkAttemptEffectResolutionV1::NoEffect, UtcMicros(31))
                .expect("valid no-effect reconciliation")
                .is_unknown_past_deadline(UtcMicros(40), 20)
        );
        assert!(
            !WorkAttemptEffectHolderV1::dispatched(
                identity(),
                WorkEffectStateV1::Observational,
                UtcMicros(10),
                UtcMicros(20),
            )
            .expect("valid observational dispatch receipt")
            .is_unknown_past_deadline(UtcMicros(40), 20)
        );
    }

    #[test]
    fn invalid_lifecycle_times_are_rejected_before_storage() {
        assert!(
            WorkAttemptEffectHolderV1::dispatched(
                identity(),
                WorkEffectStateV1::CompoundNonRepeatable,
                UtcMicros(20),
                UtcMicros(20),
            )
            .is_err()
        );
        let holder = WorkAttemptEffectHolderV1::dispatched(
            identity(),
            WorkEffectStateV1::CompoundNonRepeatable,
            UtcMicros(20),
            UtcMicros(30),
        )
        .expect("valid dispatch receipt");
        assert!(
            holder
                .with_resolution(WorkAttemptEffectResolutionV1::Unknown, UtcMicros(19))
                .is_err()
        );
    }
}
