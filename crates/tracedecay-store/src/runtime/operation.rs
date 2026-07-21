use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::UtcMicros;

use super::identity::validate_canonical_id;
use super::{
    AuthorityEpochV1, CodeShardScopeV1, CommitSequenceV1, StorageRuntimeContractErrorV1,
    StoreClientIdV1, StoreIncarnationV1, StoreOperationIdV1, StoreShardIdV1, StoreShardScopeV1,
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
pub struct IdempotencyKeyV1(String);

impl IdempotencyKeyV1 {
    pub const MAX_BYTES: usize = 512;

    pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
        let value = value.into();
        validate_canonical_id(&value, "idempotency key", Self::MAX_BYTES)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IdempotencyKeyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct CommandDigestV1(String);

impl CommandDigestV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
        let value = value.into();
        let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
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

/// Stable command identity used to distinguish replay from conflict.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyIdentityV1 {
    pub key: IdempotencyKeyV1,
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

/// Metadata common to every admitted repository operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreOperationMetadataV1 {
    pub operation_id: StoreOperationIdV1,
    pub client_id: StoreClientIdV1,
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: AuthorityEpochV1,
    pub idempotency: IdempotencyIdentityV1,
    pub durability: DurabilityClassV1,
    pub priority: OperationPriorityV1,
    pub estimated_bytes: u64,
    pub admitted_at: UtcMicros,
}

impl StoreOperationMetadataV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.estimated_bytes == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "operation estimated bytes",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "family",
    content = "operation",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RepositoryOperationV1 {
    Profile(ProfileOperationV1),
    Project(ProjectOperationV1),
    Sessions(SessionOperationV1),
    Code(CodeOperationV1),
    Effects(EffectOperationV1),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOperationV1 {
    CommitConfiguration,
    RecordUserActivity,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectOperationV1 {
    CommitFacts,
    CommitObservations,
    PublishDiagnostics,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionOperationV1 {
    PersistTranscript,
    PersistTemporalProjection,
    PublishSummary,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeOperationV1 {
    IndexRepository,
    PublishProjection,
    RecordGitIndexTransaction,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectOperationV1 {
    EnqueueOutbox,
    ApplyInbox,
    AcknowledgeOutbox,
}

impl RepositoryOperationV1 {
    pub fn name(self) -> &'static str {
        match self {
            Self::Profile(ProfileOperationV1::CommitConfiguration) => "commit configuration",
            Self::Profile(ProfileOperationV1::RecordUserActivity) => "record user activity",
            Self::Project(ProjectOperationV1::CommitFacts) => "commit facts",
            Self::Project(ProjectOperationV1::CommitObservations) => "commit observations",
            Self::Project(ProjectOperationV1::PublishDiagnostics) => "publish diagnostics",
            Self::Sessions(SessionOperationV1::PersistTranscript) => "persist transcript",
            Self::Sessions(SessionOperationV1::PersistTemporalProjection) => {
                "persist temporal projection"
            }
            Self::Sessions(SessionOperationV1::PublishSummary) => "publish summary",
            Self::Code(CodeOperationV1::IndexRepository) => "index repository",
            Self::Code(CodeOperationV1::PublishProjection) => "publish code projection",
            Self::Code(CodeOperationV1::RecordGitIndexTransaction) => {
                "record git index transaction"
            }
            Self::Effects(EffectOperationV1::EnqueueOutbox) => "enqueue outbox effect",
            Self::Effects(EffectOperationV1::ApplyInbox) => "apply inbox effect",
            Self::Effects(EffectOperationV1::AcknowledgeOutbox) => "acknowledge outbox effect",
        }
    }

    pub fn required_durability(self) -> DurabilityClassV1 {
        match self {
            Self::Code(CodeOperationV1::IndexRepository | CodeOperationV1::PublishProjection) => {
                DurabilityClassV1::RebuildableProjection
            }
            Self::Profile(_)
            | Self::Project(_)
            | Self::Sessions(_)
            | Self::Code(CodeOperationV1::RecordGitIndexTransaction)
            | Self::Effects(_) => DurabilityClassV1::Full,
        }
    }

    fn family_name(self) -> &'static str {
        match self {
            Self::Profile(_) => "profile",
            Self::Project(_) => "project",
            Self::Sessions(_) => "sessions",
            Self::Code(_) => "code",
            Self::Effects(_) => "effects",
        }
    }

    fn matches_scope(self, scope: &StoreShardScopeV1) -> bool {
        match self {
            Self::Profile(_) => matches!(scope, StoreShardScopeV1::Profile),
            Self::Project(_) => matches!(scope, StoreShardScopeV1::Project { .. }),
            Self::Sessions(_) => matches!(scope, StoreShardScopeV1::ProjectSessions { .. }),
            Self::Code(_) => matches!(
                scope,
                StoreShardScopeV1::Code {
                    scope: CodeShardScopeV1::Worktree { .. },
                    ..
                }
            ),
            Self::Effects(_) => scope.is_mutable(),
        }
    }
}

/// Closed repository operation envelope. There is intentionally no SQL variant.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryOperationEnvelopeV1 {
    pub metadata: StoreOperationMetadataV1,
    pub operation: RepositoryOperationV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryOperationEnvelopeWireV1 {
    metadata: StoreOperationMetadataV1,
    operation: RepositoryOperationV1,
}

impl RepositoryOperationEnvelopeV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.metadata.validate()?;
        if !self.metadata.shard_id.is_mutable() {
            return Err(StorageRuntimeContractErrorV1::ImmutableShard {
                operation: self.operation.name(),
            });
        }
        if !self.operation.matches_scope(&self.metadata.shard_id.scope) {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: self.operation.family_name(),
                shard_family: match self.metadata.shard_id.scope {
                    StoreShardScopeV1::Profile => "profile",
                    StoreShardScopeV1::Project { .. } => "project",
                    StoreShardScopeV1::ProjectSessions { .. } => "sessions",
                    StoreShardScopeV1::Code { .. } => "code",
                },
            });
        }
        let required = self.operation.required_durability();
        if self.metadata.durability != required {
            return Err(StorageRuntimeContractErrorV1::DurabilityMismatch {
                operation: self.operation.name(),
                required,
                actual: self.metadata.durability,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RepositoryOperationEnvelopeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RepositoryOperationEnvelopeWireV1::deserialize(deserializer)?;
        let envelope = Self {
            metadata: wire.metadata,
            operation: wire.operation,
        };
        envelope.validate().map_err(serde::de::Error::custom)?;
        Ok(envelope)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreCommitReceiptV1 {
    pub operation_id: StoreOperationIdV1,
    pub idempotency: IdempotencyIdentityV1,
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: AuthorityEpochV1,
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
    authority_epoch: AuthorityEpochV1,
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
