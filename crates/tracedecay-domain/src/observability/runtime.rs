use serde::{Deserialize, Serialize};

use super::CoverageStateV1;
use super::execution::{validate_local_ref, validate_revision};

macro_rules! closed_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

closed_enum!(WorkflowStageClassV1 {
    Admission,
    Queue,
    Execute,
    Verify,
    Integrate,
    Deliver,
    Unknown,
});
closed_enum!(NoProgressEscalationV1 {
    Observe,
    Interrupt,
    Cancel,
    Terminate,
    Kill,
    Unknown,
});
closed_enum!(EffectReconciliationOutcomeV1 {
    Committed,
    Prevented,
    Reconciled,
    Unknown,
    NotApplicable,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NoProgressObservedV1 {
    pub run_deadline_ref: String,
    pub concurrency_policy_revision: String,
    pub workflow_stage: WorkflowStageClassV1,
    pub configured_timeout_micros: u64,
    pub last_committed_frontier: u64,
    pub elapsed_stall_micros: u64,
    pub remaining_run_budget_micros: u64,
    pub escalation: NoProgressEscalationV1,
    pub effect_outcome: EffectReconciliationOutcomeV1,
}

impl NoProgressObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_local_ref(&self.run_deadline_ref)?;
        validate_revision(&self.concurrency_policy_revision)?;
        if self.configured_timeout_micros == 0
            || self.elapsed_stall_micros < self.configured_timeout_micros
        {
            return Err("no_progress_timeout");
        }
        Ok(())
    }
}

closed_enum!(LatencyStageV1 {
    Queue,
    StoreLock,
    IndexLock,
    Io,
    Parse,
    Projection,
    Model,
    Rank,
    Merge,
    Hydration,
    Synthesis,
    Render,
    Persist,
    ProviderDiscovery,
    ProviderNegotiation,
    LeaseToStart,
    ContextAssembly,
    EventIngestion,
    FirstProgress,
    Cancellation,
    Terminal,
    Reconnect,
    Resume,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LatencyObservedV1 {
    pub stage: LatencyStageV1,
    pub scheduled_arrival_micros: u64,
    pub service_micros: u64,
    pub deadline_budget_micros: Option<u64>,
    pub coverage: CoverageStateV1,
}

impl LatencyObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.deadline_budget_micros == Some(0) {
            return Err("deadline_budget");
        }
        Ok(())
    }
}

closed_enum!(DeadlineClassV1 {
    Request,
    Run,
    Stage,
    Provider,
    Shutdown,
});
closed_enum!(DeadlineOutcomeV1 {
    CompletedWithinBudget,
    Cancelled,
    TimedOut,
    EffectUnknown,
    Unknown,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeadlineObservedV1 {
    pub deadline_class: DeadlineClassV1,
    pub budget_micros: u64,
    pub elapsed_micros: u64,
    pub outcome: DeadlineOutcomeV1,
}

impl DeadlineObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.budget_micros == 0
            || (self.outcome == DeadlineOutcomeV1::TimedOut
                && self.elapsed_micros < self.budget_micros)
        {
            return Err("deadline");
        }
        Ok(())
    }
}

closed_enum!(StorageObservationKindV1 {
    ReadLatency,
    WriteLatency,
    LockWait,
    QueueBytes,
    DatabaseBytes,
    TemporaryBytes,
    ReadAmplification,
    WriteAmplification,
    RetentionExpired,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageObservedV1 {
    pub kind: StorageObservationKindV1,
    pub duration_micros: Option<u64>,
    pub quantity: Option<u64>,
    pub coverage: CoverageStateV1,
}

impl StorageObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        let duration_kind = matches!(
            self.kind,
            StorageObservationKindV1::ReadLatency
                | StorageObservationKindV1::WriteLatency
                | StorageObservationKindV1::LockWait
        );
        if duration_kind != self.duration_micros.is_some()
            || duration_kind == self.quantity.is_some()
        {
            return Err("storage_measurement");
        }
        Ok(())
    }
}

closed_enum!(IndexObservationKindV1 {
    EventToReconcile,
    EventToReady,
    Debounce,
    Rescan,
    Candidate,
    Parse,
    ChangedRange,
    Chunk,
    RelationInvalidation,
    Projection,
    Queue,
    Cancellation,
    FullRebuild,
    Publication,
});
closed_enum!(QueueDepthBucketV1 {
    Zero,
    OneToEight,
    NineTo32,
    ThirtyThreeTo128,
    Over128,
});
closed_enum!(IndexOutcomeV1 {
    Completed,
    Published,
    NoOp,
    Superseded,
    Cancelled,
    Partial,
    Failed,
    Unknown,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexObservedV1 {
    pub kind: IndexObservationKindV1,
    pub duration_micros: Option<u64>,
    pub item_count: Option<u64>,
    pub queue_depth_bucket: QueueDepthBucketV1,
    pub outcome: IndexOutcomeV1,
    pub coverage: CoverageStateV1,
}

impl IndexObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.duration_micros.is_none() && self.item_count.is_none() {
            return Err("index_measurement");
        }
        Ok(())
    }
}
