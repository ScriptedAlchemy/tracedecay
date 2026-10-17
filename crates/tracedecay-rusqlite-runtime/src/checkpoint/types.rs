use std::error::Error;
use std::fmt;
use std::time::Duration;

use tracedecay_store::{
    SnapshotLeaseIdV1, WAL_HARD_LIMIT_BYTES, WAL_SOFT_LIMIT_BYTES, WalBudgetV1,
};

use crate::RuntimeWriteAuthorityStage;

/// The checkpoint controller's thresholds are the store contract's WAL budget.
/// Re-deriving them here rather than restating the numbers keeps a single
/// authority for what the operator is allowed to configure.
pub(crate) const DEFAULT_SOFT_WAL_BYTES: u64 = WAL_SOFT_LIMIT_BYTES;
pub(crate) const DEFAULT_HARD_WAL_BYTES: u64 = WAL_HARD_LIMIT_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CheckpointConfig {
    pub(crate) soft_wal_bytes: u64,
    pub(crate) hard_wal_bytes: u64,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            soft_wal_bytes: DEFAULT_SOFT_WAL_BYTES,
            hard_wal_bytes: DEFAULT_HARD_WAL_BYTES,
        }
    }
}

/// The configured budget is the checkpoint policy. `WalBudgetV1::validate`
/// already rejects a zero soft limit and a hard limit that is not above it, and
/// `AdmissionConfigV1::validate` caps both against the contract ceilings, so
/// this conversion is total; [`CheckpointConfig::validate`] re-checks it at
/// controller construction rather than trusting the caller.
impl From<&WalBudgetV1> for CheckpointConfig {
    fn from(budget: &WalBudgetV1) -> Self {
        Self {
            soft_wal_bytes: budget.soft_limit_bytes,
            hard_wal_bytes: budget.hard_limit_bytes,
        }
    }
}

impl CheckpointConfig {
    pub(crate) fn validate(self) -> Result<Self, CheckpointConfigError> {
        if self.soft_wal_bytes == 0 {
            return Err(CheckpointConfigError::ZeroSoftLimit);
        }
        if self.hard_wal_bytes <= self.soft_wal_bytes {
            return Err(CheckpointConfigError::HardLimitNotAboveSoftLimit);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointConfigError {
    ZeroSoftLimit,
    HardLimitNotAboveSoftLimit,
}

impl fmt::Display for CheckpointConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroSoftLimit => "WAL soft checkpoint limit must be non-zero",
            Self::HardLimitNotAboveSoftLimit => {
                "WAL hard checkpoint limit must be greater than the soft limit"
            }
        })
    }
}

impl Error for CheckpointConfigError {}

/// Bounded inventory supplied by the snapshot-reader authority.
///
/// The checkpoint controller observes this inventory; it never becomes a
/// second snapshot registry or lease authority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckpointBlockers {
    pub blockers: Vec<CheckpointBlocker>,
    pub omitted: usize,
}

impl CheckpointBlockers {
    pub const fn is_clear(&self) -> bool {
        self.blockers.is_empty() && self.omitted == 0
    }

    pub fn count(&self) -> usize {
        self.blockers.len().saturating_add(self.omitted)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointBlocker {
    SnapshotLease {
        lease_id: SnapshotLeaseIdV1,
        age: Duration,
    },
    PhysicalReader {
        reader_id: u64,
        age: Duration,
    },
    /// Another process holds the database open while no reader of this
    /// runtime does, so it is the connection pinning the WAL. `pid` is that
    /// process and `process` its command name.
    ForeignConnection {
        pid: u32,
        process: String,
    },
}

pub(crate) trait CheckpointBlockerSource: Send + Sync {
    fn checkpoint_blockers(&self) -> CheckpointBlockers;

    /// Completes once snapshots whose owners had already released them when
    /// this call began have rolled back, so a requested checkpoint never
    /// reports a reader that no caller still holds as pinning the WAL.
    fn await_released_snapshots(&self) -> futures_util::future::BoxFuture<'static, ()>;

    /// Ends every snapshot that has been open at least `age`: its worker
    /// interrupts any running statement and rolls the read back, and the
    /// snapshot's later reads fail as revoked. Returns how many were asked.
    fn revoke_snapshots_older_than(&self, _age: Duration) -> usize {
        0
    }
}

/// How long a fenced drain lets in-flight snapshots run before it revokes
/// them: the background read deadline.
pub(crate) const FENCE_READ_DEADLINE: Duration = Duration::from_secs(5);

/// How long a fence may stay up. Past it, whatever still pins the WAL is
/// named in a finding, writes resume, and the checkpoint retries with backoff.
pub(crate) const FENCE_LIMIT: Duration = Duration::from_secs(6);

/// Retry interval while the fence is up.
pub(crate) const FENCED_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// First and largest retry interval once the fence has expired.
pub(crate) const EXPIRED_RETRY_MIN: Duration = Duration::from_secs(1);
pub(crate) const EXPIRED_RETRY_MAX: Duration = Duration::from_mins(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointMode {
    Restart,
    Truncate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WalPressure {
    BelowSoft,
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WalSample {
    pub(crate) frames: u64,
    pub(crate) bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CheckpointReport {
    pub(crate) busy: bool,
    pub(crate) log_frames: u64,
    pub(crate) checkpointed_frames: u64,
}

impl CheckpointReport {
    pub(crate) const fn complete(self) -> bool {
        !self.busy && self.checkpointed_frames >= self.log_frames
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointInterruption {
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointKind {
    Restart,
    Truncate,
}

impl CheckpointKind {
    fn from_internal(mode: CheckpointMode) -> Self {
        match mode {
            CheckpointMode::Restart => Self::Restart,
            CheckpointMode::Truncate => Self::Truncate,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointResult {
    Decision {
        sample: WalSample,
        decision: CheckpointDecision,
    },
    Interrupted {
        reason: CheckpointInterruption,
        sample: Option<WalSample>,
        snapshot_blockers: CheckpointBlockers,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckpointWal {
    pub frames: u64,
    pub bytes: u64,
}

impl CheckpointWal {
    pub(crate) fn from_sample(sample: WalSample) -> Self {
        Self {
            frames: sample.frames,
            bytes: sample.bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointFrameReport {
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
}

impl CheckpointFrameReport {
    fn from_internal(report: CheckpointReport) -> Self {
        Self {
            busy: report.busy,
            log_frames: report.log_frames,
            checkpointed_frames: report.checkpointed_frames,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointOutcome {
    BelowSoft {
        wal: CheckpointWal,
    },
    Complete {
        kind: CheckpointKind,
        wal: CheckpointWal,
        report: CheckpointFrameReport,
        elapsed: Duration,
    },
    Pending {
        kind: CheckpointKind,
        wal: CheckpointWal,
        report: CheckpointFrameReport,
        blockers: CheckpointBlockers,
        hard_pressure: bool,
        elapsed: Duration,
    },
    Interrupted {
        reason: CheckpointInterruption,
        wal: Option<CheckpointWal>,
        blockers: CheckpointBlockers,
    },
}

impl CheckpointOutcome {
    pub(crate) fn from_internal(result: CheckpointResult) -> Self {
        match result {
            CheckpointResult::Decision {
                sample,
                decision: CheckpointDecision::BelowSoftLimit { .. },
            } => Self::BelowSoft {
                wal: CheckpointWal::from_sample(sample),
            },
            CheckpointResult::Decision {
                sample,
                decision:
                    CheckpointDecision::Complete {
                        mode,
                        report,
                        elapsed,
                        ..
                    },
            } => Self::Complete {
                kind: CheckpointKind::from_internal(mode),
                wal: CheckpointWal::from_sample(sample),
                report: CheckpointFrameReport::from_internal(report),
                elapsed,
            },
            CheckpointResult::Decision {
                sample,
                decision:
                    CheckpointDecision::Pending {
                        mode,
                        report,
                        snapshot_blockers,
                        hard_drain_required,
                        elapsed,
                        ..
                    },
            } => Self::Pending {
                kind: CheckpointKind::from_internal(mode),
                wal: CheckpointWal::from_sample(sample),
                report: CheckpointFrameReport::from_internal(report),
                blockers: snapshot_blockers,
                hard_pressure: hard_drain_required,
                elapsed,
            },
            CheckpointResult::Interrupted {
                reason,
                sample,
                snapshot_blockers,
            } => Self::Interrupted {
                reason,
                wal: sample.map(CheckpointWal::from_sample),
                blockers: snapshot_blockers,
            },
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckpointStatus {
    pub latest: Option<CheckpointOutcome>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CheckpointPressure {
    #[default]
    Open,
    BlockGeneral {
        wal: CheckpointWal,
        blockers: CheckpointBlockers,
    },
    /// A hard-pressure drain outlived its fence: `holders` still pin the WAL,
    /// so the fence dropped and writes run while the checkpoint retries with
    /// backoff. Past twice the hard limit the writer blocks again
    /// ([`Self::BlockGeneral`]), so the WAL stays bounded.
    FenceExpired {
        wal: CheckpointWal,
        holders: CheckpointBlockers,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointDecision {
    BelowSoftLimit {
        wal_bytes: u64,
    },
    Complete {
        mode: CheckpointMode,
        pressure: WalPressure,
        wal_bytes: u64,
        report: CheckpointReport,
        elapsed: Duration,
    },
    Pending {
        mode: CheckpointMode,
        pressure: WalPressure,
        wal_bytes: u64,
        report: CheckpointReport,
        snapshot_blockers: CheckpointBlockers,
        hard_drain_required: bool,
        fence_expired: bool,
        elapsed: Duration,
    },
}

#[derive(Debug)]
pub(crate) enum CheckpointError<E> {
    InvalidConfig(CheckpointConfigError),
    Driver(E),
    AuthorityDenied(RuntimeWriteAuthorityStage),
}

impl<E: fmt::Display> fmt::Display for CheckpointError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => write!(formatter, "invalid checkpoint policy: {error}"),
            Self::Driver(error) => write!(formatter, "SQLite checkpoint failed: {error}"),
            Self::AuthorityDenied(stage) => {
                write!(formatter, "runtime write authority denied at {stage:?}")
            }
        }
    }
}

impl<E: Error + 'static> Error for CheckpointError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConfig(error) => Some(error),
            Self::Driver(error) => Some(error),
            _ => None,
        }
    }
}
