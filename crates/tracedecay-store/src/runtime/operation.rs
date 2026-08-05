use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracedecay_domain::{ObservationScopeV1, UtcMicros};

use crate::{
    AnchoredObservationWrite, ConfigurationCommitV1, DiagnosticGenerationSupersessionV1,
    EvidenceAssemblyWriteV1, FactWriteBatch, GitIndexTransactionRecordV1, ObservationCursorAdvance,
    RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    SanitizedCleanDiagnosticSnapshotV1, SessionSummaryPublicationRequestV1,
    SessionTemporalProjectionBatchV1, SourceAcquisitionQueueCasV1, SourceCommitV1,
    SourceProjectionCommitV1, TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

use super::identity::{canonical_id, validate_canonical_id};
use super::{
    CodeShardScopeV1, CommitSequenceV1, StorageRuntimeContractErrorV1, StoreAuthorityEpochV1,
    StoreClientIdV1, StoreIdempotencyKeyV1, StoreIncarnationV1, StoreOperationIdV1, StoreShardIdV1,
    StoreShardScopeV1,
};

pub const DEFAULT_PER_SHARD_QUEUE_OPERATIONS: u32 = 2_048;
pub const DEFAULT_PER_SHARD_QUEUE_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_GLOBAL_QUEUE_BYTES: u64 = 64 * 1024 * 1024;
pub const WORKSTATION_GLOBAL_QUEUE_BYTES: u64 = 256 * 1024 * 1024;
pub const FOREGROUND_BATCH_MAX_OPERATIONS: u32 = 128;
pub const FOREGROUND_BATCH_MAX_BYTES: u64 = 1024 * 1024;
pub const FOREGROUND_BATCH_MAX_DELAY_MS: u64 = 2;
pub const BACKGROUND_BATCH_MAX_OPERATIONS: u32 = 512;
pub const BACKGROUND_BATCH_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const BACKGROUND_BATCH_MAX_DELAY_MS: u64 = 10;
pub const WAL_SOFT_LIMIT_BYTES: u64 = 32 * 1024 * 1024;
pub const WAL_HARD_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MIN_READERS_PER_HOT_SHARD: u16 = 2;
pub const DEFAULT_MAX_READERS_PER_HOT_SHARD: u16 = 8;
pub const DEFAULT_MIN_GLOBAL_READERS: u16 = 8;
pub const DEFAULT_MAX_GLOBAL_READERS: u16 = 32;
pub const DEFAULT_OPEN_PROJECT_RUNTIMES: u16 = 4;
pub const MAX_OPEN_PROJECT_RUNTIMES: u16 = 8;
pub const IDLE_BURST_READER_RETIRE_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityClassV1 {
    /// Canonical state, receipts, configuration, outbox, and migrations.
    Full,
    /// Fully rebuildable code projections only.
    RebuildableProjection,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OperationPriorityV1 {
    Health,
    Foreground,
    Background,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueBudgetV1 {
    pub max_operations: u32,
    pub max_bytes: u64,
}

impl QueueBudgetV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.max_operations == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "queue max operations",
            });
        }
        if self.max_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "queue max bytes",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchBudgetV1 {
    pub max_operations: u32,
    pub max_bytes: u64,
    pub max_delay_ms: u64,
}

impl BatchBudgetV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.max_operations == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "batch max operations",
            });
        }
        if self.max_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "batch max bytes",
            });
        }
        if self.max_delay_ms == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "batch max delay",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GlobalQueueProfileV1 {
    Standard,
    ExplicitWorkstation,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReaderBudgetV1 {
    pub min_per_hot_shard: u16,
    pub max_per_hot_shard: u16,
    pub min_global: u16,
    pub max_global: u16,
    pub open_project_runtimes: u16,
    pub idle_burst_retire_ms: u64,
}

impl ReaderBudgetV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.min_per_hot_shard < 2 {
            return Err(StorageRuntimeContractErrorV1::BelowMinimum {
                field: "minimum readers per hot shard",
                actual: u64::from(self.min_per_hot_shard),
                min: 2,
            });
        }
        if self.min_per_hot_shard > self.max_per_hot_shard {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "readers per hot shard",
                min: u64::from(self.min_per_hot_shard),
                max: u64::from(self.max_per_hot_shard),
            });
        }
        if self.max_per_hot_shard > 8 {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "maximum readers per hot shard",
                actual: u64::from(self.max_per_hot_shard),
                max: 8,
            });
        }
        if self.min_global < 8 {
            return Err(StorageRuntimeContractErrorV1::BelowMinimum {
                field: "minimum global readers",
                actual: u64::from(self.min_global),
                min: 8,
            });
        }
        if self.min_global > self.max_global {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "global readers",
                min: u64::from(self.min_global),
                max: u64::from(self.max_global),
            });
        }
        if self.max_global > 32 {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "maximum global readers",
                actual: u64::from(self.max_global),
                max: 32,
            });
        }
        if self.open_project_runtimes < DEFAULT_OPEN_PROJECT_RUNTIMES {
            return Err(StorageRuntimeContractErrorV1::BelowMinimum {
                field: "open project runtimes",
                actual: u64::from(self.open_project_runtimes),
                min: u64::from(DEFAULT_OPEN_PROJECT_RUNTIMES),
            });
        }
        if self.open_project_runtimes > MAX_OPEN_PROJECT_RUNTIMES {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "open project runtimes",
                actual: u64::from(self.open_project_runtimes),
                max: u64::from(MAX_OPEN_PROJECT_RUNTIMES),
            });
        }
        if self.idle_burst_retire_ms == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "idle burst reader retirement",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WalBudgetV1 {
    pub soft_limit_bytes: u64,
    pub hard_limit_bytes: u64,
}

impl WalBudgetV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.soft_limit_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "WAL soft limit",
            });
        }
        if self.hard_limit_bytes <= self.soft_limit_bytes {
            return Err(StorageRuntimeContractErrorV1::BelowMinimum {
                field: "WAL hard limit",
                actual: self.hard_limit_bytes,
                min: self.soft_limit_bytes.saturating_add(1),
            });
        }
        Ok(())
    }
}

/// Bounded runtime admission policy with conservative selected defaults.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmissionConfigV1 {
    pub per_shard_queue: QueueBudgetV1,
    pub global_queue_max_bytes: u64,
    pub global_queue_profile: GlobalQueueProfileV1,
    pub foreground_batch: BatchBudgetV1,
    pub background_batch: BatchBudgetV1,
    pub readers: ReaderBudgetV1,
    pub wal: WalBudgetV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionConfigWireV1 {
    per_shard_queue: QueueBudgetV1,
    global_queue_max_bytes: u64,
    global_queue_profile: GlobalQueueProfileV1,
    foreground_batch: BatchBudgetV1,
    background_batch: BatchBudgetV1,
    readers: ReaderBudgetV1,
    wal: WalBudgetV1,
}

impl Default for AdmissionConfigV1 {
    fn default() -> Self {
        Self {
            per_shard_queue: QueueBudgetV1 {
                max_operations: DEFAULT_PER_SHARD_QUEUE_OPERATIONS,
                max_bytes: DEFAULT_PER_SHARD_QUEUE_BYTES,
            },
            global_queue_max_bytes: DEFAULT_GLOBAL_QUEUE_BYTES,
            global_queue_profile: GlobalQueueProfileV1::Standard,
            foreground_batch: BatchBudgetV1 {
                max_operations: FOREGROUND_BATCH_MAX_OPERATIONS,
                max_bytes: FOREGROUND_BATCH_MAX_BYTES,
                max_delay_ms: FOREGROUND_BATCH_MAX_DELAY_MS,
            },
            background_batch: BatchBudgetV1 {
                max_operations: BACKGROUND_BATCH_MAX_OPERATIONS,
                max_bytes: BACKGROUND_BATCH_MAX_BYTES,
                max_delay_ms: BACKGROUND_BATCH_MAX_DELAY_MS,
            },
            readers: ReaderBudgetV1 {
                min_per_hot_shard: DEFAULT_MIN_READERS_PER_HOT_SHARD,
                max_per_hot_shard: DEFAULT_MAX_READERS_PER_HOT_SHARD,
                min_global: DEFAULT_MIN_GLOBAL_READERS,
                max_global: DEFAULT_MAX_GLOBAL_READERS,
                open_project_runtimes: DEFAULT_OPEN_PROJECT_RUNTIMES,
                idle_burst_retire_ms: IDLE_BURST_READER_RETIRE_MS,
            },
            wal: WalBudgetV1 {
                soft_limit_bytes: WAL_SOFT_LIMIT_BYTES,
                hard_limit_bytes: WAL_HARD_LIMIT_BYTES,
            },
        }
    }
}

impl AdmissionConfigV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.per_shard_queue.validate()?;
        self.foreground_batch.validate()?;
        self.background_batch.validate()?;
        self.readers.validate()?;
        self.wal.validate()?;

        if self.per_shard_queue.max_operations > DEFAULT_PER_SHARD_QUEUE_OPERATIONS {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "per-shard queue operations",
                actual: u64::from(self.per_shard_queue.max_operations),
                max: u64::from(DEFAULT_PER_SHARD_QUEUE_OPERATIONS),
            });
        }
        if self.per_shard_queue.max_bytes > DEFAULT_PER_SHARD_QUEUE_BYTES {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "per-shard queue bytes",
                actual: self.per_shard_queue.max_bytes,
                max: DEFAULT_PER_SHARD_QUEUE_BYTES,
            });
        }

        let allowed_global = match self.global_queue_profile {
            GlobalQueueProfileV1::Standard => DEFAULT_GLOBAL_QUEUE_BYTES,
            GlobalQueueProfileV1::ExplicitWorkstation => WORKSTATION_GLOBAL_QUEUE_BYTES,
        };
        if self.global_queue_max_bytes > allowed_global {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "global queue bytes",
                actual: self.global_queue_max_bytes,
                max: allowed_global,
            });
        }
        if self.global_queue_max_bytes < self.per_shard_queue.max_bytes {
            return Err(StorageRuntimeContractErrorV1::BelowMinimum {
                field: "global queue bytes",
                actual: self.global_queue_max_bytes,
                min: self.per_shard_queue.max_bytes,
            });
        }
        validate_batch_ceiling(
            &self.foreground_batch,
            "foreground batch",
            FOREGROUND_BATCH_MAX_OPERATIONS,
            FOREGROUND_BATCH_MAX_BYTES,
            FOREGROUND_BATCH_MAX_DELAY_MS,
        )?;
        validate_batch_ceiling(
            &self.background_batch,
            "background batch",
            BACKGROUND_BATCH_MAX_OPERATIONS,
            BACKGROUND_BATCH_MAX_BYTES,
            BACKGROUND_BATCH_MAX_DELAY_MS,
        )?;
        if self.wal.soft_limit_bytes > WAL_SOFT_LIMIT_BYTES {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "WAL soft limit",
                actual: self.wal.soft_limit_bytes,
                max: WAL_SOFT_LIMIT_BYTES,
            });
        }
        if self.wal.hard_limit_bytes > WAL_HARD_LIMIT_BYTES {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "WAL hard limit",
                actual: self.wal.hard_limit_bytes,
                max: WAL_HARD_LIMIT_BYTES,
            });
        }
        if self.foreground_batch.max_operations > self.per_shard_queue.max_operations
            || self.background_batch.max_operations > self.per_shard_queue.max_operations
        {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "batch operations",
                actual: u64::from(
                    self.foreground_batch
                        .max_operations
                        .max(self.background_batch.max_operations),
                ),
                max: u64::from(self.per_shard_queue.max_operations),
            });
        }
        if self.foreground_batch.max_bytes > self.per_shard_queue.max_bytes
            || self.background_batch.max_bytes > self.per_shard_queue.max_bytes
        {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "batch bytes",
                actual: self
                    .foreground_batch
                    .max_bytes
                    .max(self.background_batch.max_bytes),
                max: self.per_shard_queue.max_bytes,
            });
        }
        Ok(())
    }
}

impl TryFrom<AdmissionConfigWireV1> for AdmissionConfigV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: AdmissionConfigWireV1) -> Result<Self, Self::Error> {
        let config = Self {
            per_shard_queue: wire.per_shard_queue,
            global_queue_max_bytes: wire.global_queue_max_bytes,
            global_queue_profile: wire.global_queue_profile,
            foreground_batch: wire.foreground_batch,
            background_batch: wire.background_batch,
            readers: wire.readers,
            wal: wire.wal,
        };
        config.validate()?;
        Ok(config)
    }
}

impl<'de> Deserialize<'de> for AdmissionConfigV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(AdmissionConfigWireV1::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

fn validate_batch_ceiling(
    budget: &BatchBudgetV1,
    field: &'static str,
    max_operations: u32,
    max_bytes: u64,
    max_delay_ms: u64,
) -> Result<(), StorageRuntimeContractErrorV1> {
    let actual = u64::from(budget.max_operations);
    if actual > u64::from(max_operations) {
        return Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field,
            actual,
            max: u64::from(max_operations),
        });
    }
    if budget.max_bytes > max_bytes {
        return Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field,
            actual: budget.max_bytes,
            max: max_bytes,
        });
    }
    if budget.max_delay_ms > max_delay_ms {
        return Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field,
            actual: budget.max_delay_ms,
            max: max_delay_ms,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct CommandDigestV1(String);

impl CommandDigestV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
        let value = value.into();
        let valid =
            tracedecay_domain::canonical_text::is_tagged_lowercase_hex(&value, "sha256:", 64);
        if !valid {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "command digest",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CommandDigestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Storage-owned projection used to distinguish replay from conflict.
///
/// The key is not the observation-domain `IdempotencyKeyV1`, whose semantics
/// and derivation are observation-specific. Application idempotency keys cross
/// this dependency boundary through `StoreIdempotencyKeyV1`'s validated,
/// lossless string conversion.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyIdentityV1 {
    pub key: StoreIdempotencyKeyV1,
    pub command_digest: CommandDigestV1,
}

impl IdempotencyIdentityV1 {
    pub fn check_replay(&self, candidate: &Self) -> Result<bool, StorageRuntimeContractErrorV1> {
        if self.key != candidate.key {
            return Ok(false);
        }
        if self.command_digest != candidate.command_digest {
            return Err(StorageRuntimeContractErrorV1::IdempotencyConflict);
        }
        Ok(true)
    }
}

canonical_id!(RuntimeDeadlineIdV1, "runtime deadline id");
canonical_id!(RuntimeCancellationIdV1, "runtime cancellation id");

/// Application-owned deadline identity propagated unchanged for correlation.
///
/// Expiry is deliberately not represented as wall-clock time here. The caller
/// owns the monotonic deadline budget and exposes only its current decision
/// through `RuntimeRequestProbeV1`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDeadlineV1 {
    pub deadline_id: RuntimeDeadlineIdV1,
}

/// Stable cancellation-token identity. A generation prevents a reset or reused
/// token from cancelling work admitted under an earlier generation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCancellationIdentityV1 {
    pub cancellation_id: RuntimeCancellationIdV1,
    pub generation: u64,
}

impl RuntimeCancellationIdentityV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.generation == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "runtime cancellation generation",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RuntimeCancellationIdentityV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            cancellation_id: RuntimeCancellationIdV1,
            generation: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let identity = Self {
            cancellation_id: wire.cancellation_id,
            generation: wire.generation,
        };
        identity.validate().map_err(serde::de::Error::custom)?;
        Ok(identity)
    }
}

/// Caller-owned interruption identities. Runtime adapters observe the current
/// monotonic decision through the probe passed to the async port; they do not
/// create a second deadline, clock, or cancellation authority.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequestControlV1 {
    pub requested_at: UtcMicros,
    pub deadline: RuntimeDeadlineV1,
    pub cancellation: RuntimeCancellationIdentityV1,
}

impl RuntimeRequestControlV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.cancellation.validate()
    }
}

impl<'de> Deserialize<'de> for RuntimeRequestControlV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            requested_at: UtcMicros,
            deadline: RuntimeDeadlineV1,
            cancellation: RuntimeCancellationIdentityV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let control = Self {
            requested_at: wire.requested_at,
            deadline: wire.deadline,
            cancellation: wire.cancellation,
        };
        control.validate().map_err(serde::de::Error::custom)?;
        Ok(control)
    }
}

/// Driver-neutral projection of one node returned by the code-graph store.
///
/// Kind and visibility remain validated canonical labels so adding a language
/// extractor does not require the storage contract crate to copy the graph
/// implementation's enums.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphNodeV1 {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: u32,
    pub attrs_start_line: u32,
    pub end_line: u32,
    pub start_column: u32,
    pub end_column: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub visibility: String,
    pub is_async: bool,
    pub branches: u32,
    pub loops: u32,
    pub returns: u32,
    pub max_nesting: u32,
    pub unsafe_blocks: u32,
    pub unchecked_calls: u32,
    pub assertions: u32,
    pub updated_at: u64,
    pub parent_id: Option<String>,
}

impl GraphNodeV1 {
    pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_canonical_id(&self.id, "graph node id", 4_096)?;
        validate_canonical_id(&self.kind, "graph node kind", 128)?;
        validate_canonical_id(&self.name, "graph node name", 16_384)?;
        validate_canonical_id(&self.file_path, "graph node file path", 65_536)?;
        validate_canonical_id(&self.visibility, "graph node visibility", 128)?;
        if let Some(parent_id) = &self.parent_id {
            validate_canonical_id(parent_id, "graph parent node id", 4_096)?;
        }
        for (field, value) in [
            ("graph qualified name", Some(self.qualified_name.as_str())),
            ("graph node signature", self.signature.as_deref()),
            ("graph node docstring", self.docstring.as_deref()),
        ] {
            if let Some(value) = value
                && value.len() > Self::MAX_TEXT_BYTES
            {
                return Err(StorageRuntimeContractErrorV1::TooLong {
                    field,
                    actual: value.len(),
                    max: Self::MAX_TEXT_BYTES,
                });
            }
        }
        Ok(())
    }
}

/// Aggregate code-graph statistics with deterministically ordered dimensions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphStatsV1 {
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub nodes_by_kind: std::collections::BTreeMap<String, u64>,
    pub edges_by_kind: std::collections::BTreeMap<String, u64>,
    pub db_size_bytes: u64,
    pub last_updated: u64,
    pub total_source_bytes: u64,
    pub files_by_language: std::collections::BTreeMap<String, u64>,
    pub last_sync_at: u64,
    pub last_full_sync_at: u64,
    pub last_sync_duration_ms: u64,
}

/// Finite graph-search relevance score.
///
/// The constructor and deserializer reject NaN and infinities, which makes the
/// otherwise floating-point value a sound `Eq` member of runtime responses.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct GraphSearchScoreV1(f64);

impl GraphSearchScoreV1 {
    pub fn new(value: f64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if !value.is_finite() {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "graph search score",
            });
        }
        Ok(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Eq for GraphSearchScoreV1 {}

impl Serialize for GraphSearchScoreV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for GraphSearchScoreV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphSearchResultV1 {
    pub node: GraphNodeV1,
    pub score: GraphSearchScoreV1,
}

impl GraphSearchResultV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.node.validate()?;
        GraphSearchScoreV1::new(self.score.get()).map(|_| ())
    }
}

/// Metadata common to every admitted repository operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreOperationMetadataV1 {
    pub operation_id: StoreOperationIdV1,
    pub client_id: StoreClientIdV1,
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub idempotency: IdempotencyIdentityV1,
    pub durability: DurabilityClassV1,
    pub priority: OperationPriorityV1,
    /// Exact bytes charged against admission. Adapters may reject an
    /// under-estimate but must never silently admit uncharged payload bytes.
    pub admission_bytes: u64,
    pub admitted_at: UtcMicros,
}

impl StoreOperationMetadataV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.admission_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "operation admission bytes",
            });
        }
        Ok(())
    }
}

/// Closed, repository-specific write payloads admitted by the runtime.
///
/// Every variant wraps a DTO validated by its owning store contract. There is
/// intentionally no query string, untyped JSON value, byte blob, or generic
/// command variant. Adding a repository operation therefore requires adding a
/// typed store projection first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryWritePayloadV1 {
    Configuration(Box<ConfigurationCommitV1>),
    Fact(Box<FactWriteBatch>),
    Observation(Box<AnchoredObservationWrite>),
    ObservationCursorAdvance(Box<ObservationCursorAdvance>),
    Diagnostics(Box<SanitizedCleanDiagnosticSnapshotV1>),
    DiagnosticSupersession(Box<DiagnosticGenerationSupersessionV1>),
    EvidenceAssembly(Box<EvidenceAssemblyWriteV1>),
    ExternalSource(Box<SourceCommitV1>),
    ExternalSourceProjection(Box<SourceProjectionCommitV1>),
    ExternalSourceAcquisition(Box<SourceAcquisitionQueueCasV1>),
    RetrievalAnchorDisposition(Box<RetrievalAnchorDispositionRecordV1>),
    RetrievalAnchorDerivative(Box<RetrievalAnchorDerivativeV1>),
    SessionProjection(Box<SessionTemporalProjectionBatchV1>),
    SessionSummary(Box<SessionSummaryPublicationRequestV1>),
    GitIndexTransaction(Box<GitIndexTransactionRecordV1>),
    EnqueueOutbox(Box<TransactionalOutboxEntryV1>),
    ApplyInbox(Box<TransactionalOutboxEntryV1>),
    AcknowledgeOutbox(Box<TransactionalInboxReceiptV1>),
}

impl RepositoryWritePayloadV1 {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "commit configuration",
            Self::Fact(_) => "commit fact lineage",
            Self::Observation(_) => "commit observation",
            Self::ObservationCursorAdvance(_) => "advance observation source cursor",
            Self::Diagnostics(_) => "publish diagnostics",
            Self::DiagnosticSupersession(_) => "supersede diagnostic generation",
            Self::EvidenceAssembly(_) => "publish evidence assembly",
            Self::ExternalSource(_) => "commit external source",
            Self::ExternalSourceProjection(_) => "project external source",
            Self::ExternalSourceAcquisition(_) => "schedule external source acquisition",
            Self::RetrievalAnchorDisposition(_) => "append retrieval anchor disposition",
            Self::RetrievalAnchorDerivative(_) => "publish retrieval anchor derivative",
            Self::SessionProjection(_) => "persist temporal projection",
            Self::SessionSummary(_) => "publish summary",
            Self::GitIndexTransaction(_) => "record git index transaction",
            Self::EnqueueOutbox(_) => "enqueue outbox effect",
            Self::ApplyInbox(_) => "apply inbox effect",
            Self::AcknowledgeOutbox(_) => "acknowledge outbox effect",
        }
    }

    pub fn required_durability(&self) -> DurabilityClassV1 {
        DurabilityClassV1::Full
    }

    fn family_name(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "profile",
            Self::Observation(_) | Self::ObservationCursorAdvance(_) => "observation",
            Self::Fact(_)
            | Self::Diagnostics(_)
            | Self::DiagnosticSupersession(_)
            | Self::EvidenceAssembly(_)
            | Self::RetrievalAnchorDisposition(_)
            | Self::RetrievalAnchorDerivative(_) => "project",
            Self::ExternalSource(_)
            | Self::ExternalSourceProjection(_)
            | Self::ExternalSourceAcquisition(_) => "external_source",
            Self::SessionProjection(_) | Self::SessionSummary(_) => "sessions",
            Self::GitIndexTransaction(_) => "code",
            Self::EnqueueOutbox(_) | Self::ApplyInbox(_) | Self::AcknowledgeOutbox(_) => "effects",
        }
    }

    fn matches_scope(&self, scope: &StoreShardScopeV1) -> bool {
        match self {
            Self::Configuration(_) => matches!(scope, StoreShardScopeV1::Profile),
            Self::Observation(write) => {
                observation_scope_matches(write.observation().scope(), scope)
            }
            Self::ObservationCursorAdvance(advance) => {
                observation_scope_matches(advance.next_cursor().scope(), scope)
            }
            Self::Fact(_) => {
                matches!(
                    scope,
                    StoreShardScopeV1::ProfileMemory | StoreShardScopeV1::Project { .. }
                )
            }
            Self::Diagnostics(_) | Self::DiagnosticSupersession(_) => {
                matches!(scope, StoreShardScopeV1::Project { .. })
            }
            Self::EvidenceAssembly(_) => matches!(
                scope,
                StoreShardScopeV1::Project { .. }
                    | StoreShardScopeV1::ProjectSessions { .. }
                    | StoreShardScopeV1::ProfileSessions
            ),
            Self::ExternalSource(commit) => matches!(
                (&commit.binding().owner, scope),
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(_),
                    StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProjectSessions { .. },
                ) | (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(_),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                )
            ),
            Self::ExternalSourceProjection(projection) => matches!(
                (&projection.source_frontier().binding().owner, scope),
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(_),
                    StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProjectSessions { .. },
                ) | (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(_),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                )
            ),
            Self::ExternalSourceAcquisition(command) => matches!(
                (&command.binding().owner, scope),
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(_),
                    StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProjectSessions { .. },
                ) | (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(_),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                )
            ),
            Self::RetrievalAnchorDisposition(_) | Self::RetrievalAnchorDerivative(_) => matches!(
                scope,
                StoreShardScopeV1::Project { .. }
                    | StoreShardScopeV1::ProjectSessions { .. }
                    | StoreShardScopeV1::ProfileSessions
            ),
            Self::SessionProjection(_) | Self::SessionSummary(_) => {
                matches!(
                    scope,
                    StoreShardScopeV1::ProfileSessions | StoreShardScopeV1::ProjectSessions { .. }
                )
            }
            Self::GitIndexTransaction(_) => matches!(
                scope,
                StoreShardScopeV1::Code {
                    scope: CodeShardScopeV1::Worktree { .. } | CodeShardScopeV1::Branch { .. },
                    ..
                }
            ),
            Self::EnqueueOutbox(_) | Self::ApplyInbox(_) | Self::AcknowledgeOutbox(_) => {
                scope.is_mutable()
            }
        }
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        match self {
            Self::Configuration(commit) => commit.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::GitIndexTransaction(record) => record.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::EnqueueOutbox(entry) => entry.validate(),
            Self::ApplyInbox(entry) => entry.validate(),
            Self::AcknowledgeOutbox(inbox) => inbox.validate(),
            Self::RetrievalAnchorDisposition(record) => record.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::RetrievalAnchorDerivative(derivative) => derivative.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::EvidenceAssembly(write) => write.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::ExternalSource(commit) => commit.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::ExternalSourceProjection(projection) => projection.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::ExternalSourceAcquisition(command) => command.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::DiagnosticSupersession(request) => request.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: self.name(),
                }
            }),
            Self::Fact(_)
            | Self::Observation(_)
            | Self::ObservationCursorAdvance(_)
            | Self::Diagnostics(_)
            | Self::SessionProjection(_)
            | Self::SessionSummary(_) => Ok(()),
        }
    }
}

fn observation_scope_matches(
    observation_scope: &ObservationScopeV1,
    shard_scope: &StoreShardScopeV1,
) -> bool {
    match (observation_scope, shard_scope) {
        (ObservationScopeV1::Profile, StoreShardScopeV1::ProfileSessions) => true,
        (
            ObservationScopeV1::Project {
                project_id: observation_project_id,
            },
            StoreShardScopeV1::ProjectSessions {
                project_id: shard_project_id,
            },
        ) => observation_project_id == shard_project_id,
        _ => false,
    }
}

/// Closed repository operation envelope carrying an executable typed payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryOperationEnvelopeV1 {
    pub metadata: StoreOperationMetadataV1,
    pub payload: RepositoryWritePayloadV1,
}

impl RepositoryOperationEnvelopeV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.metadata.validate()?;
        self.payload.validate()?;
        if !self.metadata.shard_id.is_mutable() {
            return Err(StorageRuntimeContractErrorV1::ImmutableShard {
                operation: self.payload.name(),
            });
        }
        if !self.payload.matches_scope(&self.metadata.shard_id.scope) {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: self.payload.family_name(),
                shard_family: match self.metadata.shard_id.scope {
                    StoreShardScopeV1::Profile => "profile",
                    StoreShardScopeV1::ProfileMemory => "profile_memory",
                    StoreShardScopeV1::ProfileSessions => "profile_sessions",
                    StoreShardScopeV1::Project { .. } => "project",
                    StoreShardScopeV1::ProjectSessions { .. } => "sessions",
                    StoreShardScopeV1::Code { .. } => "code",
                },
            });
        }
        if let RepositoryWritePayloadV1::Fact(batch) = &self.payload
            && !fact_owner_matches_shard(batch.owner(), &self.metadata.shard_id)
        {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: self.payload.family_name(),
                shard_family: "memory",
            });
        }
        if let RepositoryWritePayloadV1::EvidenceAssembly(write) = &self.payload {
            let exact_owner = write.owner.owner.profile_id() == &self.metadata.shard_id.profile_id
                && write.owner.owner.project_id() == self.metadata.shard_id.scope.project_id();
            if !exact_owner {
                return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                    operation: self.payload.family_name(),
                    shard_family: "project",
                });
            }
        }
        if let RepositoryWritePayloadV1::ExternalSource(commit) = &self.payload {
            let exact_owner = match (&commit.binding().owner, &self.metadata.shard_id.scope) {
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(project_id),
                    StoreShardScopeV1::Project {
                        project_id: shard_project,
                    }
                    | StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project,
                    },
                ) => project_id == shard_project,
                (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(profile_id),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                ) => profile_id == &self.metadata.shard_id.profile_id,
                _ => false,
            };
            if !exact_owner {
                return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                    operation: self.payload.family_name(),
                    shard_family: "external_source",
                });
            }
        }
        if let RepositoryWritePayloadV1::ExternalSourceAcquisition(command) = &self.payload {
            let exact_owner = match (&command.binding().owner, &self.metadata.shard_id.scope) {
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(project_id),
                    StoreShardScopeV1::Project {
                        project_id: shard_project,
                    }
                    | StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project,
                    },
                ) => project_id == shard_project,
                (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(profile_id),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                ) => profile_id == &self.metadata.shard_id.profile_id,
                _ => false,
            };
            if !exact_owner {
                return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                    operation: self.payload.family_name(),
                    shard_family: "external_source",
                });
            }
        }
        if let RepositoryWritePayloadV1::ExternalSourceProjection(projection) = &self.payload {
            let exact_owner = match (
                &projection.source_frontier().binding().owner,
                &self.metadata.shard_id.scope,
            ) {
                (
                    tracedecay_domain::SourceBindingOwnerV1::Project(project_id),
                    StoreShardScopeV1::Project {
                        project_id: shard_project,
                    }
                    | StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project,
                    },
                ) => project_id == shard_project,
                (
                    tracedecay_domain::SourceBindingOwnerV1::Profile(profile_id),
                    StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
                ) => profile_id == &self.metadata.shard_id.profile_id,
                _ => false,
            };
            if !exact_owner {
                return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                    operation: self.payload.family_name(),
                    shard_family: "external_source",
                });
            }
        }
        if let RepositoryWritePayloadV1::RetrievalAnchorDisposition(record) = &self.payload
            && !retrieval_anchor_owner_matches_shard(record.owner(), &self.metadata.shard_id)
        {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: self.payload.family_name(),
                shard_family: "project",
            });
        }
        if let RepositoryWritePayloadV1::RetrievalAnchorDerivative(derivative) = &self.payload
            && !retrieval_anchor_owner_matches_shard(derivative.owner(), &self.metadata.shard_id)
        {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: self.payload.family_name(),
                shard_family: "project",
            });
        }
        let required = self.payload.required_durability();
        if self.metadata.durability != required {
            return Err(StorageRuntimeContractErrorV1::DurabilityMismatch {
                operation: self.payload.name(),
                required,
                actual: self.metadata.durability,
            });
        }
        Ok(())
    }
}

fn fact_owner_matches_shard(
    owner: &tracedecay_domain::FactOwnerV1,
    shard_id: &StoreShardIdV1,
) -> bool {
    match owner {
        tracedecay_domain::FactOwnerV1::Profile => {
            matches!(&shard_id.scope, StoreShardScopeV1::ProfileMemory)
        }
        tracedecay_domain::FactOwnerV1::Project { project_id } => matches!(
            &shard_id.scope,
            StoreShardScopeV1::Project {
                project_id: shard_project_id,
            } if shard_project_id == project_id
        ),
    }
}

fn retrieval_anchor_owner_matches_shard(
    owner: &crate::RetrievalAnchorOwnerV1,
    shard_id: &StoreShardIdV1,
) -> bool {
    match owner {
        crate::RetrievalAnchorOwnerV1::V3(owner) => {
            owner.profile_id() == &shard_id.profile_id
                && owner.project_id() == shard_id.scope.project_id()
        }
        crate::RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Project {
            project_id,
        }) => shard_id.scope.project_id() == Some(project_id),
        crate::RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Profile) => {
            matches!(&shard_id.scope, StoreShardScopeV1::ProfileSessions)
        }
    }
}

/// Durable storage commit evidence.
///
/// It has no parallel free-standing receipt ID: its canonical identity is the
/// operation/idempotency pair plus the fenced shard commit position. Domain
/// receipt IDs remain owned by their specific domain operations.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreCommitReceiptV1 {
    pub operation_id: StoreOperationIdV1,
    pub idempotency: IdempotencyIdentityV1,
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub commit_sequence: CommitSequenceV1,
    pub committed_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreCommitReceiptWireV1 {
    operation_id: StoreOperationIdV1,
    idempotency: IdempotencyIdentityV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    authority_epoch: StoreAuthorityEpochV1,
    commit_sequence: CommitSequenceV1,
    committed_at: UtcMicros,
}

impl StoreCommitReceiptV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.commit_sequence.0 == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "receipt commit sequence",
            });
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        if self.operation_id != metadata.operation_id {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "receipt operation id",
            });
        }
        if self.idempotency != metadata.idempotency {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "receipt idempotency identity",
            });
        }
        if self.shard_id != metadata.shard_id {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "receipt shard id",
            });
        }
        if self.incarnation != metadata.incarnation {
            return Err(StorageRuntimeContractErrorV1::IncarnationMismatch {
                field: "receipt incarnation",
                expected: metadata.incarnation,
                actual: self.incarnation,
            });
        }
        if self.authority_epoch != metadata.authority_epoch {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "receipt authority epoch",
            });
        }
        Ok(())
    }

    /// A replay returns the original durable receipt. It must bind to the
    /// idempotency identity and shard history, but its operation id may belong
    /// to the original submission rather than the retry attempt.
    pub fn validate_replay_for(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        if self.idempotency != metadata.idempotency {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "replay receipt idempotency identity",
            });
        }
        if self.shard_id != metadata.shard_id {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "replay receipt shard id",
            });
        }
        if self.incarnation != metadata.incarnation {
            return Err(StorageRuntimeContractErrorV1::IncarnationMismatch {
                field: "replay receipt incarnation",
                expected: metadata.incarnation,
                actual: self.incarnation,
            });
        }
        if self.authority_epoch != metadata.authority_epoch {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "replay receipt authority epoch",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for StoreCommitReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StoreCommitReceiptWireV1::deserialize(deserializer)?;
        let receipt = Self {
            operation_id: wire.operation_id,
            idempotency: wire.idempotency,
            shard_id: wire.shard_id,
            incarnation: wire.incarnation,
            authority_epoch: wire.authority_epoch,
            commit_sequence: wire.commit_sequence,
            committed_at: wire.committed_at,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

impl fmt::Display for CommandDigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{BrainId, FactOwnerV1, ProjectId, UserProfileId};

    use super::{
        ObservationScopeV1, StoreShardIdV1, StoreShardScopeV1, fact_owner_matches_shard,
        observation_scope_matches, retrieval_anchor_owner_matches_shard,
    };

    #[test]
    fn observation_scope_requires_the_exact_authoritative_shard() {
        let project_id = ProjectId::new("project.fixture").unwrap();
        let other_project_id = ProjectId::new("project.other").unwrap();
        let project = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };

        assert!(observation_scope_matches(
            &ObservationScopeV1::Profile,
            &StoreShardScopeV1::ProfileSessions
        ));
        assert!(!observation_scope_matches(
            &ObservationScopeV1::Profile,
            &StoreShardScopeV1::Profile
        ));
        assert!(observation_scope_matches(
            &project,
            &StoreShardScopeV1::ProjectSessions {
                project_id: project_id.clone(),
            }
        ));
        assert!(!observation_scope_matches(
            &project,
            &StoreShardScopeV1::ProjectSessions {
                project_id: other_project_id,
            }
        ));
        assert!(!observation_scope_matches(
            &project,
            &StoreShardScopeV1::Project {
                project_id: project_id.clone(),
            }
        ));
        assert!(!observation_scope_matches(
            &project,
            &StoreShardScopeV1::Profile
        ));
        assert!(!observation_scope_matches(
            &ObservationScopeV1::Profile,
            &StoreShardScopeV1::ProjectSessions { project_id }
        ));
    }

    #[test]
    fn profile_retrieval_anchor_requires_the_injected_profile_sessions_shard() {
        let profile_id = UserProfileId::new("profile.fixture").unwrap();
        let shard = StoreShardIdV1::profile_sessions(
            BrainId::new("brain.fixture").unwrap(),
            profile_id.clone(),
        );
        assert!(retrieval_anchor_owner_matches_shard(
            &FactOwnerV1::Profile.into(),
            &shard
        ));

        let project_shard = StoreShardIdV1::project(
            BrainId::new("brain.fixture").unwrap(),
            profile_id,
            ProjectId::new("project.fixture").unwrap(),
        );
        assert!(!retrieval_anchor_owner_matches_shard(
            &FactOwnerV1::Profile.into(),
            &project_shard
        ));

        let project_id = ProjectId::new("project.fixture").unwrap();
        let project_sessions_shard = StoreShardIdV1::project_sessions(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            project_id.clone(),
        );
        assert!(retrieval_anchor_owner_matches_shard(
            &FactOwnerV1::Project { project_id }.into(),
            &project_sessions_shard
        ));
    }

    #[test]
    fn profile_facts_require_the_dedicated_profile_memory_shard() {
        let profile_memory = StoreShardIdV1::profile_memory(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        );
        let profile = StoreShardIdV1::profile(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        );
        let profile_sessions = StoreShardIdV1::profile_sessions(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        );

        assert!(fact_owner_matches_shard(
            &FactOwnerV1::Profile,
            &profile_memory,
        ));
        assert!(!fact_owner_matches_shard(&FactOwnerV1::Profile, &profile));
        assert!(!fact_owner_matches_shard(
            &FactOwnerV1::Profile,
            &profile_sessions,
        ));
    }
}
