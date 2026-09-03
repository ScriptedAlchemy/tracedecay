use std::future::Future;
use std::pin::Pin;

use super::{
    CommitSequenceV1, ConsistencyModeV1, FrozenWatermarkCoverageV1, FrozenWatermarkVectorV1,
    GraphNodeV1, GraphSearchResultV1, GraphStatsV1, MaintenanceTelemetryV1, OperationPriorityV1,
    ReaderHealthLeaseIdV1, ReaderHealthLeaseV1, RuntimeCancellationIdentityV1,
    RuntimeCancellationStageV1, RuntimeDeadlineV1, RuntimeRequestControlV1,
    RuntimeTransactionScopeV1, SaturationScopeV1, ShardWatermarkV1, SnapshotLeaseIdV1,
    SnapshotLeaseV1, StorageRuntimeContractErrorV1, StorageRuntimeErrorV1, StoreCommitReceiptV1,
    StoreRuntimeBindingV1, UnavailableReasonV1, WatermarkCoverageStatusV1,
};
use super::{
    RepositoryOperationEnvelopeV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    StoreAuthorityEpochV1, StoreShardIdV1,
};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// One caller-owned monotonic interruption decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeInterruptionV1 {
    Cancelled,
    DeadlineExceeded,
}

/// Live observation of the caller-owned cancellation token and monotonic
/// deadline budget.
///
/// Both identities must equal those carried by the request. Once
/// `interruption` returns a decision, implementations must return that same
/// decision on every later poll. Compatibility adapters poll immediately
/// before and after each legacy call; those calls are not mid-call
/// interruptible.
pub trait RuntimeRequestProbeV1: Send + Sync {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1;
    fn deadline_identity(&self) -> &RuntimeDeadlineV1;
    fn interruption(&self) -> Option<RuntimeInterruptionV1>;

    /// Atomically arbitrates cancellation against the sole irreversible
    /// durable commit. A probe must return `true` at most once across every
    /// context that shares it. Read-only probes return `false`.
    fn try_begin_commit(&self) -> bool;

    /// An externally arbitrated request cannot share its commit transaction
    /// with unrelated work.
    fn requires_isolated_commit(&self) -> bool {
        false
    }
}

/// A validated, closed write request for the daemon-owned runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSubmitRequestV1 {
    envelope: RepositoryOperationEnvelopeV1,
    transaction_scope: RuntimeTransactionScopeV1,
    control: RuntimeRequestControlV1,
}

impl RuntimeSubmitRequestV1 {
    pub fn new(
        envelope: RepositoryOperationEnvelopeV1,
        transaction_scope: RuntimeTransactionScopeV1,
        control: RuntimeRequestControlV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            envelope,
            transaction_scope,
            control,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn envelope(&self) -> &RepositoryOperationEnvelopeV1 {
        &self.envelope
    }

    pub fn transaction_scope(&self) -> &RuntimeTransactionScopeV1 {
        &self.transaction_scope
    }

    pub fn control(&self) -> &RuntimeRequestControlV1 {
        &self.control
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.transaction_scope.compatibility.binding
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.envelope.validate()?;
        self.control.validate()?;
        self.transaction_scope
            .validate_operation(&self.envelope.metadata)?;
        if self.control.requested_at != self.envelope.metadata.admitted_at {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime request admission time",
            });
        }
        Ok(())
    }
}

/// Idempotent write outcomes, including expected admission and cancellation
/// states. Driver failures remain `StorageRuntimePortErrorV1`; these variants
/// are stable runtime decisions callers must handle explicitly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeSubmitOutcomeV1 {
    Committed {
        receipt: StoreCommitReceiptV1,
    },
    ExactReplay {
        receipt: StoreCommitReceiptV1,
    },
    IdempotencyConflict {
        existing_receipt: StoreCommitReceiptV1,
    },
    Saturated {
        shard_id: Option<StoreShardIdV1>,
        scope: SaturationScopeV1,
        retry_after_ms: u64,
    },
    Fenced {
        expected: StoreAuthorityEpochV1,
        actual: StoreAuthorityEpochV1,
    },
    DeadlineExceededBeforeCommit {
        deadline: RuntimeDeadlineV1,
    },
    CancelledBeforeCommit {
        cancellation: RuntimeCancellationIdentityV1,
        stage: RuntimeCancellationStageV1,
    },
    CommittedAfterCancellation {
        receipt: StoreCommitReceiptV1,
        cancellation: RuntimeCancellationIdentityV1,
    },
    Unavailable {
        reason: UnavailableReasonV1,
    },
}

impl RuntimeSubmitOutcomeV1 {
    pub fn validate_for(
        &self,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        request.validate()?;
        let metadata = &request.envelope().metadata;
        match self {
            Self::Committed { receipt } => receipt.validate_for(metadata),
            Self::ExactReplay { receipt } => receipt.validate_replay_for(metadata),
            Self::IdempotencyConflict { existing_receipt } => {
                existing_receipt.validate()?;
                if existing_receipt.shard_id != metadata.shard_id {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt shard id",
                    });
                }
                if existing_receipt.incarnation != metadata.incarnation {
                    return Err(StorageRuntimeContractErrorV1::IncarnationMismatch {
                        field: "conflict receipt incarnation",
                        expected: metadata.incarnation,
                        actual: existing_receipt.incarnation,
                    });
                }
                if existing_receipt.authority_epoch != metadata.authority_epoch
                    || existing_receipt.idempotency.key != metadata.idempotency.key
                    || existing_receipt.idempotency.command_digest
                        == metadata.idempotency.command_digest
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt idempotency binding",
                    });
                }
                Ok(())
            }
            Self::Saturated {
                shard_id,
                scope,
                retry_after_ms,
            } => {
                if *retry_after_ms == 0 {
                    return Err(StorageRuntimeContractErrorV1::Zero {
                        field: "saturation retry delay",
                    });
                }
                let shard_scoped = matches!(
                    scope,
                    SaturationScopeV1::ShardOperations
                        | SaturationScopeV1::ShardBytes
                        | SaturationScopeV1::ReaderPool
                );
                if shard_scoped && shard_id.as_ref() != Some(&metadata.shard_id) {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "saturation shard id",
                    });
                }
                if *scope == SaturationScopeV1::GlobalBytes && shard_id.is_some() {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "global saturation shard id",
                    });
                }
                Ok(())
            }
            Self::Fenced { expected, actual } => {
                if *expected != metadata.authority_epoch || expected == actual {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "fencing authority epoch",
                    });
                }
                Ok(())
            }
            Self::DeadlineExceededBeforeCommit { deadline } => {
                if deadline != &request.control().deadline {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "runtime deadline outcome",
                    });
                }
                Ok(())
            }
            Self::CancelledBeforeCommit {
                cancellation,
                stage,
            } => {
                if cancellation != &request.control().cancellation
                    || !matches!(
                        stage,
                        RuntimeCancellationStageV1::BeforeAdmission
                            | RuntimeCancellationStageV1::Queued
                            | RuntimeCancellationStageV1::BeforeCommit
                    )
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "runtime cancellation outcome",
                    });
                }
                Ok(())
            }
            Self::CommittedAfterCancellation {
                receipt,
                cancellation,
            } => {
                if cancellation != &request.control().cancellation {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "post-commit cancellation identity",
                    });
                }
                receipt.validate_for(metadata)
            }
            Self::Unavailable { reason } => {
                if matches!(
                    reason,
                    UnavailableReasonV1::Cancelled
                        | UnavailableReasonV1::DeadlineExceeded
                        | UnavailableReasonV1::WrongAuthorityEpoch
                ) {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "submit decision channel",
                    });
                }
                Ok(())
            }
        }
    }
}

/// Closed read operations admitted by the storage runtime. No operation carries
/// a driver query or a physical locator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeReadOperationV1 {
    CurrentWatermark,
    SnapshotLease { lease_id: SnapshotLeaseIdV1 },
    FrozenCoverage,
    MaintenanceTelemetry,
    ReaderHealthLease { lease_id: ReaderHealthLeaseIdV1 },
    TemporalHealth,
    GraphStats,
    GraphNode { node_id: String },
    GraphSearch { query: String, limit: u32 },
    GraphQuickCheck,
    Repository { op: RepositoryReadOperationV1 },
}

impl RuntimeReadOperationV1 {
    pub const MAX_GRAPH_QUERY_BYTES: usize = 16_384;
    pub const MAX_GRAPH_SEARCH_RESULTS: u32 = 1_000;

    fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        let (field, value, max) = match self {
            Self::GraphNode { node_id } => ("graph node id", Some(node_id), 4_096),
            Self::GraphSearch { query, limit } => {
                if *limit == 0 {
                    return Err(StorageRuntimeContractErrorV1::Zero {
                        field: "graph search limit",
                    });
                }
                if *limit > Self::MAX_GRAPH_SEARCH_RESULTS {
                    return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                        field: "graph search limit",
                        actual: u64::from(*limit),
                        max: u64::from(Self::MAX_GRAPH_SEARCH_RESULTS),
                    });
                }
                (
                    "graph search query",
                    Some(query),
                    Self::MAX_GRAPH_QUERY_BYTES,
                )
            }
            // Repository DTO and exact-shard validation runs on the complete
            // request because it requires the daemon-verified binding.
            Self::Repository { .. } => return Ok(()),
            _ => return Ok(()),
        };
        let value = value.expect("graph operation validation always supplies text");
        if value.is_empty() {
            return Err(StorageRuntimeContractErrorV1::Empty { field });
        }
        if value.len() > max {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field,
                actual: value.len(),
                max,
            });
        }
        Ok(())
    }
}

/// A validated one-runtime read request. `consistency` directly represents
/// Latest, AtLeast, ExactSnapshot, or a frozen cross-shard vector.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RuntimeReadRequestV1 {
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
    operation: RuntimeReadOperationV1,
    priority: OperationPriorityV1,
    admission_bytes: u64,
    control: RuntimeRequestControlV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReadRequestWireV1 {
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
    operation: RuntimeReadOperationV1,
    priority: OperationPriorityV1,
    admission_bytes: u64,
    control: RuntimeRequestControlV1,
}

impl RuntimeReadRequestV1 {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        consistency: ConsistencyModeV1,
        operation: RuntimeReadOperationV1,
        priority: OperationPriorityV1,
        admission_bytes: u64,
        control: RuntimeRequestControlV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            binding,
            consistency,
            operation,
            priority,
            admission_bytes,
            control,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn consistency(&self) -> &ConsistencyModeV1 {
        &self.consistency
    }

    pub fn operation(&self) -> &RuntimeReadOperationV1 {
        &self.operation
    }

    pub fn priority(&self) -> OperationPriorityV1 {
        self.priority
    }

    pub fn admission_bytes(&self) -> u64 {
        self.admission_bytes
    }

    pub fn control(&self) -> &RuntimeRequestControlV1 {
        &self.control
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.control.validate()?;
        self.operation.validate()?;
        if let RuntimeReadOperationV1::Repository { op } = &self.operation {
            op.validate_for_binding(&self.binding)?;
        }
        if self.admission_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "read admission bytes",
            });
        }
        match &self.consistency {
            ConsistencyModeV1::ExactSnapshot { lease } => {
                lease.validate()?;
                if !binding_matches_watermark(&self.binding, &lease.watermark) {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "snapshot lease runtime binding",
                    });
                }
                if let RuntimeReadOperationV1::SnapshotLease { lease_id } = &self.operation
                    && *lease_id != lease.lease_id
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "snapshot lease request id",
                    });
                }
            }
            ConsistencyModeV1::FrozenWatermarkVector { vector }
                if vector.get(&self.binding.shard_id).is_none_or(|watermark| {
                    !binding_matches_watermark(&self.binding, watermark)
                }) =>
            {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "frozen watermark runtime binding",
                });
            }
            _ => {}
        }
        if self.operation == RuntimeReadOperationV1::FrozenCoverage
            && !matches!(
                self.consistency,
                ConsistencyModeV1::FrozenWatermarkVector { .. }
            )
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "frozen coverage consistency",
            });
        }
        if self.operation == RuntimeReadOperationV1::TemporalHealth
            && self.priority != OperationPriorityV1::Health
        {
            return Err(StorageRuntimeContractErrorV1::ReaderHealthLaneRequired);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RuntimeReadRequestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RuntimeReadRequestWireV1::deserialize(deserializer)?;
        Self::new(
            wire.binding,
            wire.consistency,
            wire.operation,
            wire.priority,
            wire.admission_bytes,
            wire.control,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeReadResultV1 {
    CurrentWatermark { watermark: ShardWatermarkV1 },
    SnapshotLease { lease: Option<SnapshotLeaseV1> },
    FrozenCoverage { coverage: FrozenWatermarkCoverageV1 },
    MaintenanceTelemetry { telemetry: MaintenanceTelemetryV1 },
    ReaderHealthLease { lease: Option<ReaderHealthLeaseV1> },
    TemporalHealth { healthy: bool },
    GraphStats { stats: GraphStatsV1 },
    GraphNode { node: Option<GraphNodeV1> },
    GraphSearch { results: Vec<GraphSearchResultV1> },
    GraphQuickCheck { healthy: bool },
    Repository { result: RepositoryReadResultV1 },
}

/// Explicit history coverage for every read. `Partial`, `Stale`, and
/// `Unavailable` are successful typed read decisions, not malformed responses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeReadCoverageV1 {
    Latest {
        /// Canonical runtime watermark when one exists. Compatibility reads
        /// may honestly serve latest state without inventing a commit position.
        observed: Option<ShardWatermarkV1>,
    },
    Complete {
        coverage: FrozenWatermarkCoverageV1,
    },
    Partial {
        coverage: FrozenWatermarkCoverageV1,
    },
    Stale {
        coverage: FrozenWatermarkCoverageV1,
    },
    Unavailable {
        coverage: Option<FrozenWatermarkCoverageV1>,
        reason: UnavailableReasonV1,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RuntimeReadOutcomeV1 {
    value: Option<RuntimeReadResultV1>,
    coverage: RuntimeReadCoverageV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReadOutcomeWireV1 {
    value: Option<RuntimeReadResultV1>,
    coverage: RuntimeReadCoverageV1,
}

impl RuntimeReadOutcomeV1 {
    pub fn new(
        value: Option<RuntimeReadResultV1>,
        coverage: RuntimeReadCoverageV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let outcome = Self { value, coverage };
        outcome.validate_shape()?;
        Ok(outcome)
    }

    pub fn value(&self) -> Option<&RuntimeReadResultV1> {
        self.value.as_ref()
    }

    pub fn coverage(&self) -> &RuntimeReadCoverageV1 {
        &self.coverage
    }

    fn validate_shape(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        match (&self.coverage, &self.value) {
            (
                RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. },
                None,
            )
            | (
                RuntimeReadCoverageV1::Stale { .. } | RuntimeReadCoverageV1::Unavailable { .. },
                Some(_),
            ) => Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime read value coverage shape",
            }),
            _ => Ok(()),
        }
    }

    pub fn validate_for(
        &self,
        request: &RuntimeReadRequestV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        request.validate()?;
        self.validate_shape()?;
        validate_read_coverage(request, &self.coverage)?;
        if let Some(value) = &self.value {
            validate_read_value(request, value, &self.coverage)?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RuntimeReadOutcomeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RuntimeReadOutcomeWireV1::deserialize(deserializer)?;
        Self::new(wire.value, wire.coverage).map_err(serde::de::Error::custom)
    }
}

fn validate_read_coverage(
    request: &RuntimeReadRequestV1,
    response: &RuntimeReadCoverageV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if matches!(request.consistency(), ConsistencyModeV1::LatestAvailable) {
        return match response {
            RuntimeReadCoverageV1::Latest {
                observed: Some(observed),
            } if binding_matches_watermark(request.binding(), observed) => Ok(()),
            RuntimeReadCoverageV1::Latest { observed: None } => Ok(()),
            RuntimeReadCoverageV1::Unavailable { coverage: None, .. } => Ok(()),
            _ => Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "latest read coverage",
            }),
        };
    }

    let required = required_vector(request)?;
    let (coverage, expected_class) = match response {
        RuntimeReadCoverageV1::Complete { coverage } => (coverage, 0_u8),
        RuntimeReadCoverageV1::Partial { coverage } => (coverage, 1),
        RuntimeReadCoverageV1::Stale { coverage } => (coverage, 2),
        RuntimeReadCoverageV1::Unavailable {
            coverage: Some(coverage),
            ..
        } => (coverage, 3),
        RuntimeReadCoverageV1::Unavailable { coverage: None, .. } => return Ok(()),
        RuntimeReadCoverageV1::Latest { .. } => {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "bounded read coverage",
            });
        }
    };
    coverage.validate()?;
    if coverage.required != required {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "read coverage required vector",
        });
    }
    let has_stale = coverage
        .required
        .iter()
        .any(|(shard_id, _)| coverage.status_for(shard_id) == WatermarkCoverageStatusV1::Stale);
    let has_unavailable = coverage.required.iter().any(|(shard_id, _)| {
        coverage.status_for(shard_id) == WatermarkCoverageStatusV1::Unavailable
    });
    let actual_class = if coverage.is_complete() {
        0
    } else if coverage.is_partial() {
        1
    } else if has_stale {
        2
    } else if has_unavailable {
        3
    } else {
        unreachable!("a non-empty coverage vector always has a derived class")
    };
    if actual_class != expected_class {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "read coverage classification",
        });
    }
    Ok(())
}

fn validate_read_value(
    request: &RuntimeReadRequestV1,
    value: &RuntimeReadResultV1,
    coverage: &RuntimeReadCoverageV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    match (request.operation(), value) {
        (
            RuntimeReadOperationV1::CurrentWatermark,
            RuntimeReadResultV1::CurrentWatermark { watermark },
        ) if binding_matches_watermark(request.binding(), watermark)
            && coverage_observes(coverage, watermark) =>
        {
            Ok(())
        }
        (
            RuntimeReadOperationV1::SnapshotLease { lease_id },
            RuntimeReadResultV1::SnapshotLease { lease },
        ) => {
            if lease.is_none()
                && matches!(
                    request.consistency(),
                    ConsistencyModeV1::ExactSnapshot { .. }
                )
            {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "exact snapshot lease read result",
                });
            }
            if let Some(lease) = lease {
                lease.validate()?;
                if lease.lease_id != *lease_id
                    || !binding_matches_watermark(request.binding(), &lease.watermark)
                    || !coverage_observes(coverage, &lease.watermark)
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "snapshot lease read result",
                    });
                }
            }
            Ok(())
        }
        (
            RuntimeReadOperationV1::FrozenCoverage,
            RuntimeReadResultV1::FrozenCoverage {
                coverage: value_coverage,
            },
        ) if matches!(
            coverage,
            RuntimeReadCoverageV1::Complete { coverage }
                | RuntimeReadCoverageV1::Partial { coverage }
                if coverage == value_coverage
        ) =>
        {
            Ok(())
        }
        (
            RuntimeReadOperationV1::MaintenanceTelemetry,
            RuntimeReadResultV1::MaintenanceTelemetry { telemetry },
        ) if telemetry.shard_id == request.binding().shard_id
            && telemetry.incarnation == request.binding().incarnation
            && telemetry.authority_epoch == request.binding().authority_epoch =>
        {
            Ok(())
        }
        (
            RuntimeReadOperationV1::ReaderHealthLease { lease_id },
            RuntimeReadResultV1::ReaderHealthLease { lease },
        ) => {
            if let Some(lease) = lease {
                lease.validate()?;
                if lease.lease_id != *lease_id || lease.binding != *request.binding() {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "reader health lease read result",
                    });
                }
            }
            Ok(())
        }
        (RuntimeReadOperationV1::GraphStats, RuntimeReadResultV1::GraphStats { .. })
        | (RuntimeReadOperationV1::TemporalHealth, RuntimeReadResultV1::TemporalHealth { .. })
        | (RuntimeReadOperationV1::GraphQuickCheck, RuntimeReadResultV1::GraphQuickCheck { .. })
        // Repository reads carry results validated by their typed store DTOs; the
        // runtime port only enforces that the result family matches the request.
        | (RuntimeReadOperationV1::Repository { .. }, RuntimeReadResultV1::Repository { .. }) => {
            Ok(())
        }
        (
            RuntimeReadOperationV1::GraphNode { node_id },
            RuntimeReadResultV1::GraphNode { node },
        ) => {
            if let Some(node) = node {
                node.validate()?;
                if node.id != *node_id {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "graph node point result id",
                    });
                }
            }
            Ok(())
        }
        (
            RuntimeReadOperationV1::GraphSearch { limit, .. },
            RuntimeReadResultV1::GraphSearch { results },
        ) => {
            if results.len() > *limit as usize {
                return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                    field: "graph search result count",
                    actual: results.len() as u64,
                    max: u64::from(*limit),
                });
            }
            for result in results {
                result.validate()?;
            }
            Ok(())
        }
        _ => Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "runtime read result operation",
        }),
    }
}

fn required_vector(
    request: &RuntimeReadRequestV1,
) -> Result<FrozenWatermarkVectorV1, StorageRuntimeContractErrorV1> {
    match request.consistency() {
        ConsistencyModeV1::FrozenWatermarkVector { vector } => Ok(vector.clone()),
        ConsistencyModeV1::ExactSnapshot { lease } => {
            FrozenWatermarkVectorV1::new([lease.watermark.clone()])
        }
        ConsistencyModeV1::AtLeast { commit_sequence } => {
            FrozenWatermarkVectorV1::new([ShardWatermarkV1 {
                shard_id: request.binding().shard_id.clone(),
                incarnation: request.binding().incarnation,
                authority_epoch: request.binding().authority_epoch,
                commit_sequence: *commit_sequence,
            }])
        }
        ConsistencyModeV1::LatestAvailable => {
            Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "latest consistency has no required vector",
            })
        }
    }
}

fn coverage_observes(coverage: &RuntimeReadCoverageV1, watermark: &ShardWatermarkV1) -> bool {
    match coverage {
        RuntimeReadCoverageV1::Latest {
            observed: Some(observed),
        } => observed == watermark,
        RuntimeReadCoverageV1::Latest { observed: None } => false,
        RuntimeReadCoverageV1::Complete { coverage }
        | RuntimeReadCoverageV1::Partial { coverage } => {
            coverage.observed(&watermark.shard_id) == Some(watermark)
        }
        RuntimeReadCoverageV1::Stale { .. } | RuntimeReadCoverageV1::Unavailable { .. } => false,
    }
}

fn binding_matches_watermark(
    binding: &StoreRuntimeBindingV1,
    watermark: &ShardWatermarkV1,
) -> bool {
    binding.shard_id == watermark.shard_id
        && binding.incarnation == watermark.incarnation
        && binding.authority_epoch == watermark.authority_epoch
}

fn validate_probe(
    control: &RuntimeRequestControlV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if probe.cancellation_identity() != &control.cancellation {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "runtime cancellation probe identity",
        });
    }
    if probe.deadline_identity() != &control.deadline {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "runtime deadline probe identity",
        });
    }
    Ok(())
}

fn read_interruption(
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<Option<RuntimeReadOutcomeV1>, StorageRuntimeContractErrorV1> {
    let reason = match probe.interruption() {
        Some(RuntimeInterruptionV1::Cancelled) => UnavailableReasonV1::Cancelled,
        Some(RuntimeInterruptionV1::DeadlineExceeded) => UnavailableReasonV1::DeadlineExceeded,
        None => return Ok(None),
    };
    RuntimeReadOutcomeV1::new(
        None,
        RuntimeReadCoverageV1::Unavailable {
            coverage: None,
            reason,
        },
    )
    .map(Some)
}

#[derive(Debug, Error)]
pub enum StorageRuntimePortErrorV1 {
    #[error("invalid storage runtime request: {0}")]
    InvalidRequest(StorageRuntimeContractErrorV1),
    #[error("invalid storage runtime response: {0}")]
    InvalidResponse(StorageRuntimeContractErrorV1),
    #[error(transparent)]
    Runtime(Box<StorageRuntimeErrorV1>),
}

impl From<StorageRuntimeErrorV1> for StorageRuntimePortErrorV1 {
    fn from(error: StorageRuntimeErrorV1) -> Self {
        Self::Runtime(Box::new(error))
    }
}

pub type StorageRuntimePortResultV1<T> = Result<T, StorageRuntimePortErrorV1>;
pub type StorageRuntimePortFutureV1<'a, T> =
    Pin<Box<dyn Future<Output = StorageRuntimePortResultV1<T>> + Send + 'a>>;

/// Object-safe std-only asynchronous read boundary.
pub trait StorageRuntimeReadPort: Send + Sync {
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1>;

    fn read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
        Box::pin(async move {
            request
                .validate()
                .and_then(|()| validate_probe(request.control(), probe))
                .map_err(StorageRuntimePortErrorV1::InvalidRequest)?;
            if let Some(outcome) =
                read_interruption(probe).map_err(StorageRuntimePortErrorV1::InvalidResponse)?
            {
                return Ok(outcome);
            }
            let outcome = self.dispatch_read(request.clone(), probe).await?;
            outcome
                .validate_for(&request)
                .map_err(StorageRuntimePortErrorV1::InvalidResponse)?;
            if let Some(interrupted) =
                read_interruption(probe).map_err(StorageRuntimePortErrorV1::InvalidResponse)?
            {
                return Ok(interrupted);
            }
            Ok(outcome)
        })
    }
}

// Keep the single-shard requirement helper explicit so adapter migrations do
// not infer a vector from mutable ambient runtime state.
pub fn single_shard_required_coverage_v1(
    binding: &StoreRuntimeBindingV1,
    commit_sequence: CommitSequenceV1,
    observed: impl IntoIterator<Item = ShardWatermarkV1>,
) -> Result<FrozenWatermarkCoverageV1, StorageRuntimeContractErrorV1> {
    let required = FrozenWatermarkVectorV1::new([ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence,
    }])?;
    FrozenWatermarkCoverageV1::new(required, observed)
}
