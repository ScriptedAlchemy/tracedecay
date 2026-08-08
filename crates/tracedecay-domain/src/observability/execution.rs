use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::CoverageStateV1;

pub const MAX_LOCAL_ANCHORS_V1: usize = 8;
pub const MAX_CAUSE_BUCKETS_V1: usize = 16;

macro_rules! closed_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

closed_enum!(ExecutionTopologyKindV1 {
    Single,
    Sequential,
    Parallel,
    Hierarchical,
    Hybrid,
});
closed_enum!(ExecutionPlacementV1 {
    None,
    InPlace,
    LinkedWorktree,
    IsolatedClone,
});
closed_enum!(WorkTopologyBranchV1 {
    NoBranches,
    Unbranched,
    IndependentBranches,
    LocalStack,
});
closed_enum!(ReviewTopologyV1 {
    NoReview,
    IndependentReview,
    StandardPullRequests,
    GitHubStackedPullRequests,
});
closed_enum!(IntegrationStrategyV1 {
    NoIntegration,
    ExternalObservedOnly,
    FastForwardOnly,
    MergeCommit,
    CherryPickExactCommits,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTopologySampledV1 {
    pub topology: ExecutionTopologyKindV1,
    pub placement: ExecutionPlacementV1,
    pub branch_topology: WorkTopologyBranchV1,
    pub review_topology: ReviewTopologyV1,
    pub integration_strategy: IntegrationStrategyV1,
    pub requested_width: u16,
    pub accepted_width: u16,
    pub admitted_width: u16,
    pub active_width: u16,
    pub useful_width: u16,
    pub runnable_count: u16,
    pub blocked_count: u16,
    pub shared_authority_serialized_count: u16,
    pub local_anchor_refs: Vec<String>,
}

impl ExecutionTopologySampledV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_anchors(&self.local_anchor_refs)?;
        if self.accepted_width > self.requested_width
            || self.admitted_width > self.accepted_width
            || self.active_width > self.admitted_width
            || self.useful_width > self.active_width
            || self.shared_authority_serialized_count > self.admitted_width
        {
            return Err("execution_topology_widths");
        }
        Ok(())
    }
}

closed_enum!(ConflictKindV1 {
    Mechanical,
    Semantic,
    Combined,
});
closed_enum!(ConflictPredictionV1 {
    Conflict,
    NoConflict,
    Abstained,
    Unknown,
});
closed_enum!(ConflictScoreKindV1 {
    Rule,
    CalibratedProbability,
    Hybrid,
});
closed_enum!(ConflictOutcomeV1 {
    Conflict,
    NoConflict,
    Censored,
    Unknown,
});
closed_enum!(ConflictAdjudicatorV1 {
    NativeGit,
    IndependentTest,
    IndependentReview,
    Combined,
    None,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkConflictPredictionObservedV1 {
    pub prediction_ref: String,
    pub kind: ConflictKindV1,
    pub prediction: ConflictPredictionV1,
    pub score_kind: ConflictScoreKindV1,
    pub descriptor_revision: String,
    pub calibration_revision: String,
    pub eligible_relation_count: u16,
    pub expires_at_micros: i64,
    pub coverage: CoverageStateV1,
    pub local_anchor_refs: Vec<String>,
}

impl WorkConflictPredictionObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_local_ref(&self.prediction_ref)?;
        validate_revision(&self.descriptor_revision)?;
        validate_revision(&self.calibration_revision)?;
        validate_anchors(&self.local_anchor_refs)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkConflictOutcomeLinkedV1 {
    pub prediction_ref: String,
    pub kind: ConflictKindV1,
    pub outcome: ConflictOutcomeV1,
    pub adjudicator: ConflictAdjudicatorV1,
    pub horizon_micros: u64,
    pub coverage: CoverageStateV1,
    pub correction_revision: u32,
}

impl WorkConflictOutcomeLinkedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_local_ref(&self.prediction_ref)?;
        if matches!(
            (self.kind, self.adjudicator),
            (ConflictKindV1::Semantic, ConflictAdjudicatorV1::NativeGit)
                | (
                    ConflictKindV1::Mechanical,
                    ConflictAdjudicatorV1::IndependentTest
                )
                | (
                    ConflictKindV1::Mechanical,
                    ConflictAdjudicatorV1::IndependentReview
                )
        ) {
            return Err("conflict_adjudicator");
        }
        if matches!(
            self.outcome,
            ConflictOutcomeV1::Conflict | ConflictOutcomeV1::NoConflict
        ) && self.adjudicator == ConflictAdjudicatorV1::None
        {
            return Err("conflict_adjudicator");
        }
        Ok(())
    }
}

closed_enum!(IntegrationPhaseV1 {
    Ready,
    ProposalCreated,
    DryRunRequested,
    DryRunTerminal,
    ApplyRequested,
    ApplyTerminal,
    NativeIntegratedObserved,
    RequiredChecksTerminal,
    AcceptedOutcomeObserved,
    Cancelled,
    Censored,
    Unknown,
});
closed_enum!(IntegrationResultV1 {
    Succeeded,
    Conflicted,
    Rejected,
    Denied,
    Stale,
    Locked,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
    Unsupported,
    Unknown,
});
closed_enum!(IntegrationOperationKindV1 {
    FastForward,
    MergeCommit,
    Rebase,
    CherryPick,
    StackRetarget,
    GraphOnly,
    ExternalObserved,
    Unknown,
});
closed_enum!(IntegrationScopeClassV1 {
    Worktree,
    BranchStack,
    Repository,
    External,
    Unknown,
});
closed_enum!(IntegrationOwnerReceiptV1 {
    GitApply,
    NativeGitObservation,
    ExternalProvider,
    None,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkIntegrationTransitionObservedV1 {
    pub phase: IntegrationPhaseV1,
    pub result: IntegrationResultV1,
    pub operation: IntegrationOperationKindV1,
    pub source_scope: IntegrationScopeClassV1,
    pub target_scope: IntegrationScopeClassV1,
    pub dependency_commits_eligible: u16,
    pub dependency_commits_observed: u16,
    pub required_checks_eligible: u16,
    pub required_checks_observed: u16,
    pub owner_receipt: IntegrationOwnerReceiptV1,
    pub coverage: CoverageStateV1,
    pub local_anchor_refs: Vec<String>,
}

impl WorkIntegrationTransitionObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_anchors(&self.local_anchor_refs)?;
        if self.dependency_commits_observed > self.dependency_commits_eligible
            || self.required_checks_observed > self.required_checks_eligible
        {
            return Err("integration_coverage");
        }
        if self.phase == IntegrationPhaseV1::ApplyRequested
            && !matches!(
                self.operation,
                IntegrationOperationKindV1::FastForward
                    | IntegrationOperationKindV1::MergeCommit
                    | IntegrationOperationKindV1::CherryPick
            )
        {
            return Err("integration_apply_owner");
        }
        if self.phase == IntegrationPhaseV1::NativeIntegratedObserved
            && !matches!(
                self.owner_receipt,
                IntegrationOwnerReceiptV1::NativeGitObservation
                    | IntegrationOwnerReceiptV1::ExternalProvider
            )
        {
            return Err("integration_native_receipt");
        }
        Ok(())
    }
}

closed_enum!(StackDriftKindV1 {
    HeadAdvanced,
    BaseAdvanced,
    MergeBaseChanged,
    DependencyMissing,
    RefDeleted,
    WorktreeGenerationChanged,
    Retargeted,
    Superseded,
    Unknown,
});
closed_enum!(IntervalStateV1 { Open, Closed });
closed_enum!(DurationBucketV1 {
    Under1m,
    From1mTo5m,
    From5mTo15m,
    From15mTo1h,
    From1hTo4h,
    From4hTo24h,
    From1dTo7d,
    Over7d,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkStackDriftObservedV1 {
    pub kind: StackDriftKindV1,
    pub state: IntervalStateV1,
    pub first_observed_micros: i64,
    pub terminal_micros: Option<i64>,
    pub age_bucket: DurationBucketV1,
    pub coverage: CoverageStateV1,
}

impl WorkStackDriftObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        match (self.state, self.terminal_micros) {
            (IntervalStateV1::Open, None) => Ok(()),
            (IntervalStateV1::Closed, Some(terminal)) if terminal >= self.first_observed_micros => {
                Ok(())
            }
            _ => Err("stack_drift_interval"),
        }
    }
}

closed_enum!(GitHubStackCapabilityV1 {
    Unavailable,
    PrivatePreviewDisabled,
    Enabled,
    Degraded,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitHubStackCapabilityObservedV1 {
    pub capability: GitHubStackCapabilityV1,
    pub probe_revision: String,
    pub standard_git_fallback_available: bool,
    pub other_forge_fallback_available: bool,
    pub coverage: CoverageStateV1,
}

impl GitHubStackCapabilityObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_revision(&self.probe_revision)
    }
}

closed_enum!(DuplicateEffortKindV1 {
    ExactDuplicate,
    SupersededOverlap,
    RepeatedInvestigation,
    DuplicateEffect,
    NotDuplicate,
    Censored,
    Unknown,
});
closed_enum!(QuantityEvidenceClassV1 {
    OwnerReceipt,
    LocallyMeasured,
    Estimated,
    Unknown,
});
closed_enum!(DuplicateEffectOutcomeV1 {
    Prevented,
    Committed,
    Unknown,
    NotApplicable,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkDuplicateEffortObservedV1 {
    pub kind: DuplicateEffortKindV1,
    pub wall_micros: Option<u64>,
    pub token_count: Option<u64>,
    pub cost_micros: Option<u64>,
    pub test_count: Option<u64>,
    pub effect_count: Option<u64>,
    pub evidence: QuantityEvidenceClassV1,
    pub effect_outcome: DuplicateEffectOutcomeV1,
    pub coverage: CoverageStateV1,
    pub local_anchor_refs: Vec<String>,
}

impl WorkDuplicateEffortObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_anchors(&self.local_anchor_refs)?;
        let has_quantity = self.wall_micros.is_some()
            || self.token_count.is_some()
            || self.cost_micros.is_some()
            || self.test_count.is_some()
            || self.effect_count.is_some();
        if has_quantity && self.evidence == QuantityEvidenceClassV1::Unknown {
            return Err("duplicate_effort_evidence");
        }
        Ok(())
    }
}

closed_enum!(BlockedCauseV1 {
    Dependency,
    NeedsInput,
    Capability,
    Policy,
    Scope,
    Conflict,
    Lease,
    Backpressure,
    Test,
    Ci,
    Review,
    EffectUnknown,
    Other,
    Unknown,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkBlockedIntervalObservedV1 {
    pub cause: BlockedCauseV1,
    pub interval_revision: u32,
    pub valid_from_micros: i64,
    pub valid_until_micros: Option<i64>,
    pub coverage: CoverageStateV1,
}

impl WorkBlockedIntervalObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.interval_revision == 0
            || self
                .valid_until_micros
                .is_some_and(|until| until < self.valid_from_micros)
        {
            return Err("blocked_interval");
        }
        Ok(())
    }
}

closed_enum!(RerunSourceV1 { Runtime, Test, Ci });
closed_enum!(RerunCauseV1 {
    RuntimeRetry,
    RuntimeFallback,
    TestRerun,
    CiRerun,
    Recovery,
    HumanRequested,
    Unknown,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkRerunObservedV1 {
    pub source: RerunSourceV1,
    pub cause: RerunCauseV1,
    pub eligible_original_count: u16,
    pub linked_rerun_count: u16,
    pub latency_bucket: DurationBucketV1,
    pub coverage: CoverageStateV1,
}

impl WorkRerunObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.linked_rerun_count > self.eligible_original_count {
            return Err("rerun_counts");
        }
        Ok(())
    }
}

closed_enum!(WorkExecutionLeakKindV1 {
    LeaseAfterTerminal,
    AttemptWithoutLiveOwner,
    EffectUnknownPastDeadline,
    MissingWorktreeBinding,
    UnboundedDelivery,
    None,
    Unknown,
});
closed_enum!(WorkExecutionLeakRecoveryV1 {
    NotRequired,
    Pending,
    Recovered,
    Failed,
    Unknown,
});
closed_enum!(LeakOwnerClassV1 {
    Work,
    Workflow,
    Git,
    Delivery,
    Unknown,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkExecutionLeakObservedV1 {
    pub kind: WorkExecutionLeakKindV1,
    pub detection_horizon_micros: u64,
    pub recovery: WorkExecutionLeakRecoveryV1,
    pub owner_class: LeakOwnerClassV1,
    pub coverage: CoverageStateV1,
}

impl WorkExecutionLeakObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.kind == WorkExecutionLeakKindV1::None
            && self.recovery != WorkExecutionLeakRecoveryV1::NotRequired
        {
            return Err("leak_recovery");
        }
        Ok(())
    }
}

closed_enum!(DeliverySurfaceFamilyV1 {
    Hook,
    Mcp,
    Lsp,
    Dashboard,
    Cli,
    Other,
});
closed_enum!(DeliveryEventClassV1 {
    OperationAccepted,
    OperationProgress,
    OperationTerminal,
    Diagnostic,
    Activity,
    Other,
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkDeliveryFanoutObservedV1 {
    pub event_class: DeliveryEventClassV1,
    pub surface: DeliverySurfaceFamilyV1,
    pub eligible: u16,
    pub attempted: u16,
    pub delivered: u16,
    pub deduplicated: u16,
    pub dropped: u16,
    pub unknown: u16,
}

impl WorkDeliveryFanoutObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.attempted > self.eligible
            || self
                .delivered
                .saturating_add(self.deduplicated)
                .saturating_add(self.dropped)
                .saturating_add(self.unknown)
                > self.attempted
        {
            return Err("delivery_fanout_counts");
        }
        Ok(())
    }
}

pub(super) fn validate_anchors(anchors: &[String]) -> Result<(), &'static str> {
    if anchors.len() > MAX_LOCAL_ANCHORS_V1
        || anchors
            .iter()
            .any(|anchor| validate_local_ref(anchor).is_err())
    {
        return Err("local_anchor_refs");
    }
    Ok(())
}

pub(super) fn validate_local_ref(value: &str) -> Result<(), &'static str> {
    if !crate::canonical_text::is_canonical_text_within(value, 128)
        || !value.starts_with(|character: char| character.is_ascii_lowercase())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
    {
        return Err("local_ref");
    }
    Ok(())
}

pub(super) fn validate_revision(value: &str) -> Result<(), &'static str> {
    if crate::canonical_text::is_canonical_text_within(value, 96) {
        Ok(())
    } else {
        Err("revision")
    }
}
