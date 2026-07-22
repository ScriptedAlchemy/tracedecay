use std::fmt;

use tracedecay_store::{
    RuntimeMaintenanceStateV1, StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1,
    StoreShardIdV1,
};

use crate::checkpoint::CheckpointBlockers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceOwnerId(u64);

impl MaintenanceOwnerId {
    pub fn new(value: u64) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationPlanId(String);

impl MigrationPlanId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        validated_id(value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtsIndexId(String);

impl FtsIndexId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        validated_id(value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validated_id(value: String) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    .then_some(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMaintenanceArtifact {
    pub artifact_id: String,
    pub shard_id: StoreShardIdV1,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionMode {
    Incremental,
    Full,
}

/// Closed maintenance menu. None of these variants contains SQL or a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenanceAction {
    Migration {
        plan: MigrationPlanId,
    },
    FtsRebuild {
        index: FtsIndexId,
    },
    Restore {
        artifact: VerifiedMaintenanceArtifact,
    },
    Compaction {
        mode: CompactionMode,
    },
    ShardReplacement {
        artifact: VerifiedMaintenanceArtifact,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceRequest {
    pub owner: MaintenanceOwnerId,
    pub expected: StoreRuntimeRegistryPublicationV1,
    pub action: MaintenanceAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainBlockers {
    pub admissions: u32,
    pub readers: u32,
    pub snapshots: CheckpointBlockers,
    pub writer_active: bool,
}

impl DrainBlockers {
    pub const fn is_clear(&self) -> bool {
        self.admissions == 0
            && self.readers == 0
            && self.snapshots.is_clear()
            && !self.writer_active
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationBoundary {
    BeforeDrain,
    AwaitingDrain,
    BeforeAction,
}

pub trait MaintenanceCancellation {
    fn is_cancelled(&self, boundary: CancellationBoundary) -> bool;
}

impl<F> MaintenanceCancellation for F
where
    F: Fn(CancellationBoundary) -> bool,
{
    fn is_cancelled(&self, boundary: CancellationBoundary) -> bool {
        self(boundary)
    }
}

/// Linear evidence that every runtime user was observed drained for one exact
/// canonical registry publication.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DrainedStateProof {
    publication: StoreRuntimeRegistryPublicationV1,
    observed: DrainBlockers,
}

impl DrainedStateProof {
    pub(crate) fn observe(
        publication: StoreRuntimeRegistryPublicationV1,
        blockers: DrainBlockers,
    ) -> Result<Self, MaintenanceError> {
        if !blockers.is_clear() {
            return Err(MaintenanceError::Lifecycle {
                stage: "exclusive permit issued before drain",
            });
        }
        Ok(Self {
            publication,
            observed: blockers,
        })
    }
}

/// Linear exclusive capability. It intentionally cannot be cloned: exactly
/// one terminal reopen or fault transition consumes it.
#[derive(Debug, PartialEq, Eq)]
pub struct ExclusiveMaintenancePermit {
    owner: MaintenanceOwnerId,
    publication: StoreRuntimeRegistryPublicationV1,
    drained: DrainedStateProof,
}

impl ExclusiveMaintenancePermit {
    /// Only a lifecycle adapter that has fenced and drained its runtime should
    /// issue this capability.
    pub(crate) fn issue_after_drain(
        owner: MaintenanceOwnerId,
        publication: StoreRuntimeRegistryPublicationV1,
        drained: DrainedStateProof,
    ) -> Result<Self, MaintenanceError> {
        if drained.publication != publication || !drained.observed.is_clear() {
            return Err(MaintenanceError::Lifecycle {
                stage: "exclusive permit proof fence",
            });
        }
        Ok(Self {
            owner,
            publication,
            drained,
        })
    }

    #[cfg(test)]
    pub(crate) fn issue(owner: MaintenanceOwnerId, binding: StoreRuntimeBindingV1) -> Self {
        let publication: StoreRuntimeRegistryPublicationV1 =
            serde_json::from_value(serde_json::json!({
                "publication_id": "publication.test-only",
                "binding": binding,
                "published_at": 1
            }))
            .expect("test publication is valid");
        let drained = DrainedStateProof::observe(publication.clone(), DrainBlockers::default())
            .expect("test runtime is drained");
        Self::issue_after_drain(owner, publication, drained).expect("test permit is fenced")
    }

    pub const fn owner(&self) -> MaintenanceOwnerId {
        self.owner
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.publication.binding
    }

    pub fn publication(&self) -> &StoreRuntimeRegistryPublicationV1 {
        &self.publication
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverMaintenanceError {
    pub code: &'static str,
    pub retryable: bool,
}

impl fmt::Display for DriverMaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.code)?;
        if self.retryable {
            formatter.write_str(" (retryable)")?;
        }
        Ok(())
    }
}

impl std::error::Error for DriverMaintenanceError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenanceError {
    AlreadyOwned {
        owner: MaintenanceOwnerId,
    },
    NoActiveMaintenance,
    WrongOwner,
    Fenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    InvalidState {
        expected: RuntimeMaintenanceStateV1,
        actual: RuntimeMaintenanceStateV1,
    },
    ArtifactShardMismatch,
    CanonicalAuthority {
        stage: &'static str,
    },
    Lifecycle {
        stage: &'static str,
    },
    Driver(DriverMaintenanceError),
}

impl fmt::Display for MaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOwned { .. } => formatter.write_str("maintenance is already owned"),
            Self::NoActiveMaintenance => formatter.write_str("no maintenance request is active"),
            Self::WrongOwner => {
                formatter.write_str("maintenance request belongs to a different owner")
            }
            Self::Fenced { .. } => {
                formatter.write_str("maintenance publication fence no longer matches")
            }
            Self::InvalidState { .. } => {
                formatter.write_str("runtime is not in the required maintenance state")
            }
            Self::ArtifactShardMismatch => {
                formatter.write_str("maintenance artifact belongs to a different shard")
            }
            Self::CanonicalAuthority { stage } => {
                write!(formatter, "canonical registry authority failed at {stage}")
            }
            Self::Lifecycle { stage } => {
                write!(formatter, "maintenance lifecycle failed at {stage}")
            }
            Self::Driver(error) => write!(formatter, "maintenance driver failed: {error}"),
        }
    }
}

impl std::error::Error for MaintenanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Driver(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceStart {
    Started,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplacementPublicationKind {
    Reopen,
    Fault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementPublicationRequest {
    pub prior: StoreRuntimeRegistryPublicationV1,
    pub kind: ReplacementPublicationKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementPublicationReceipt {
    pub request: ReplacementPublicationRequest,
    pub publication: StoreRuntimeRegistryPublicationV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationAttempt {
    pub request: ReplacementPublicationRequest,
    pub receipt: Option<ReplacementPublicationReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenanceProgress {
    Blocked {
        blockers: DrainBlockers,
        cancellation_recorded: bool,
    },
    Reopened {
        publication: Box<ReplacementPublicationReceipt>,
        action_performed: bool,
    },
    Faulted {
        error: MaintenanceError,
        publication: Box<PublicationAttempt>,
    },
}
