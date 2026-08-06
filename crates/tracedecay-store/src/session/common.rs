use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DataVersionDigest, SessionContractError, SessionId, SessionProjectionGenerationV1,
    SessionRefreshOperationIdV1, SignedCursorKeyRefV1, UtcMicros,
};

/// Features a session-temporal adapter can support without opening storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionTemporalCapabilityV1 {
    /// Freeze and consume immutable watermarks for retrieval.
    FrozenWatermarks,
    /// Begin, persist, and activate candidate projection generations.
    GenerationRebuild,
    /// Publish or exactly replay immutable summaries.
    ImmutableSummaryPublication,
    RefreshJoin,
    RefreshProgressPersistence,
    RefreshCancellation,
}

/// Declares the session-temporal capabilities enforced by store ports.
pub trait SessionTemporalCapabilityProvider {
    fn session_temporal_capabilities(&self) -> &SessionTemporalCapabilitiesV1;
}

/// Stable set of supported session-temporal features for one frozen snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTemporalCapabilitiesV1 {
    capabilities: BTreeSet<SessionTemporalCapabilityV1>,
}

impl SessionTemporalCapabilitiesV1 {
    pub fn new(capabilities: impl IntoIterator<Item = SessionTemporalCapabilityV1>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub fn supports(&self, capability: SessionTemporalCapabilityV1) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SessionTemporalCapabilityV1> {
        self.capabilities.iter()
    }
}

/// Read watermarks captured together before a temporal operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionFrozenWatermarksV1 {
    active_generation: SessionProjectionGenerationV1,
    source_frontier: u64,
    projection_frontier: u64,
    summary_frontier: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
}

impl SessionFrozenWatermarksV1 {
    pub const fn new(
        active_generation: SessionProjectionGenerationV1,
        source_frontier: u64,
        projection_frontier: u64,
        summary_frontier: u64,
    ) -> Self {
        Self {
            active_generation,
            source_frontier,
            projection_frontier,
            summary_frontier,
            cursor_key: None,
        }
    }

    pub fn with_cursor_key(mut self, cursor_key: SignedCursorKeyRefV1) -> Self {
        self.cursor_key = Some(cursor_key);
        self
    }

    pub const fn active_generation(&self) -> SessionProjectionGenerationV1 {
        self.active_generation
    }

    pub const fn source_frontier(&self) -> u64 {
        self.source_frontier
    }

    pub const fn projection_frontier(&self) -> u64 {
        self.projection_frontier
    }

    pub const fn summary_frontier(&self) -> u64 {
        self.summary_frontier
    }

    pub fn cursor_key(&self) -> Option<&SignedCursorKeyRefV1> {
        self.cursor_key.as_ref()
    }

    pub fn has_same_frontiers_and_cursor(&self, other: &Self) -> bool {
        self.source_frontier == other.source_frontier
            && self.projection_frontier == other.projection_frontier
            && self.summary_frontier == other.summary_frontier
            && self.cursor_key == other.cursor_key
    }
}

/// Immutable retrieval snapshot for exactly one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalSnapshotV1 {
    session_id: SessionId,
    frozen_at: UtcMicros,
    watermarks: SessionFrozenWatermarksV1,
    capabilities: SessionTemporalCapabilitiesV1,
}

impl SessionTemporalSnapshotV1 {
    pub fn new(
        session_id: SessionId,
        frozen_at: UtcMicros,
        watermarks: SessionFrozenWatermarksV1,
        capabilities: SessionTemporalCapabilitiesV1,
    ) -> Self {
        Self {
            session_id,
            frozen_at,
            watermarks,
            capabilities,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frozen_at(&self) -> UtcMicros {
        self.frozen_at
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub fn capabilities(&self) -> &SessionTemporalCapabilitiesV1 {
        &self.capabilities
    }
}

/// Scope for freezing a session-temporal retrieval snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalSnapshotRequestV1 {
    session_id: SessionId,
}

impl SessionTemporalSnapshotRequestV1 {
    pub fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

/// Complete lifecycle state of a durable session refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshStateV1 {
    Running,
    Complete,
    Failed,
    Cancelled,
}

/// Non-sensitive reason that a refresh failure code was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshFailureCodeInvalidReasonV1 {
    Empty,
    TooLong,
    ContainsControl,
    NonCanonical,
}

/// Non-sensitive reason a session-temporal digest was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTemporalDigestInvalidReasonV1 {
    TooLong,
    Malformed,
}

/// Bounded canonical digest used for projection and migration idempotency.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionTemporalDigestV1(DataVersionDigest);

impl SessionTemporalDigestV1 {
    /// `sha512:` plus 128 lowercase hexadecimal digits.
    pub const MAX_LEN: usize = 135;

    pub fn new(value: impl Into<String>) -> SessionStoreResult<Self> {
        let value = value.into();
        if value.len() > Self::MAX_LEN {
            return Err(SessionStoreError::InvalidTemporalDigest {
                reason: SessionTemporalDigestInvalidReasonV1::TooLong,
            });
        }
        DataVersionDigest::new(value).map(Self).map_err(|_| {
            SessionStoreError::InvalidTemporalDigest {
                reason: SessionTemporalDigestInvalidReasonV1::Malformed,
            }
        })
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Errors returned by transport-neutral session-temporal store contracts.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("session temporal {field} count {count} exceeds the maximum of {max}")]
    BatchLimitExceeded {
        field: &'static str,
        count: usize,
        max: usize,
    },
    #[error("session temporal page limit {limit} must be between 1 and {max}")]
    InvalidPageLimit { limit: usize, max: usize },
    #[error(
        "session refresh committed frontier {committed_through} is past observed frontier {observed_through}"
    )]
    InvalidRefreshFrontier {
        observed_through: u64,
        committed_through: u64,
    },
    #[error("session identity mismatch in {context}")]
    SessionMismatch { context: &'static str },
    #[error("projection batch belongs to a different generation")]
    ProjectionBatchGenerationMismatch,
    #[error("projection batch uses different frozen watermarks")]
    FrozenWatermarkMismatch,
    #[error("session temporal cursor pagination requires a frozen cursor key")]
    CursorKeyRequired,
    #[error(
        "session temporal receipt count mismatch for {field}: expected {expected}, actual {actual}"
    )]
    ReceiptCountMismatch {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("session temporal receipt identity mismatch in {context}")]
    ReceiptIdentityMismatch { context: &'static str },
    #[error("session temporal idempotency conflict in {context}")]
    IdempotencyConflict { context: &'static str },
    #[error("invalid session temporal state transition in {context}")]
    InvalidStateTransition { context: &'static str },
    #[error("session temporal capability {capability:?} is unsupported")]
    UnsupportedCapability {
        capability: SessionTemporalCapabilityV1,
    },
    #[error("session temporal operation was cancelled")]
    Cancelled,
    #[error("session temporal operation deadline elapsed")]
    DeadlineExceeded,
    #[error("session temporal operation exceeded its {resource} budget")]
    BudgetExceeded { resource: &'static str },
    #[error("session temporal generation {generation:?} is missing")]
    MissingGeneration {
        generation: SessionProjectionGenerationV1,
    },
    #[error("session temporal generation is stale: expected {expected:?}, actual {actual:?}")]
    StaleGeneration {
        expected: SessionProjectionGenerationV1,
        actual: SessionProjectionGenerationV1,
    },
    #[error("refresh operation {operation_id:?} cannot transition from state {state:?}")]
    InvalidRefreshState {
        operation_id: SessionRefreshOperationIdV1,
        state: SessionRefreshStateV1,
    },
    #[error("invalid non-sensitive refresh failure code: {reason:?}")]
    InvalidRefreshFailureCode {
        reason: SessionRefreshFailureCodeInvalidReasonV1,
    },
    #[error("invalid session-temporal digest: {reason:?}")]
    InvalidTemporalDigest {
        reason: SessionTemporalDigestInvalidReasonV1,
    },
    #[error("session-temporal contract validation failed")]
    Contract(#[from] SessionContractError),
    #[error("session-temporal storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
}

impl SessionStoreError {
    /// Map only adapter/infrastructure failures to `Storage`; semantic and
    /// contract failures should be returned unchanged.
    pub fn storage(operation: &'static str, source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Storage {
            operation,
            source: Box::new(source),
        }
    }

    pub const fn is_storage(&self) -> bool {
        matches!(self, Self::Storage { .. })
    }
}

pub type SessionStoreResult<T> = Result<T, SessionStoreError>;

/// Unforgeable proof that one session-temporal operation was authorized.
///
/// Constructible only inside `tracedecay-store` via capability-gated port
/// methods. The operation parameter makes permits non-interchangeable even
/// when two operations are guarded by the same runtime capability.
///
/// ```compile_fail
/// use tracedecay_store::{
///     SessionRefreshBeginOrJoinRequestV1, SessionRefreshStore,
/// };
///
/// fn bypass_capability_guard<T: SessionRefreshStore>(
///     ports: &T,
///     request: SessionRefreshBeginOrJoinRequestV1,
/// ) {
///     // Missing the unforgeable permit argument.
///     let _ = ports.begin_or_join_session_refresh_supported(request);
/// }
/// ```
///
/// ```compile_fail
/// use std::marker::PhantomData;
///
/// use tracedecay_store::{
///     SessionRefreshBeginOrJoinOperation, SessionTemporalOperationPermit,
/// };
///
/// fn forge_permit(
/// ) -> SessionTemporalOperationPermit<SessionRefreshBeginOrJoinOperation> {
///     SessionTemporalOperationPermit {
///         _operation: PhantomData,
///     }
/// }
/// ```
///
/// ```compile_fail,E0308
/// use tracedecay_store::{
///     SessionRefreshBeginOrJoinPermit, SessionRefreshProgressPersistPermit,
/// };
///
/// fn persist_progress(_permit: SessionRefreshProgressPersistPermit) {}
///
/// fn cross_use_refresh_permit(permit: SessionRefreshBeginOrJoinPermit) {
///     persist_progress(permit);
/// }
/// ```
#[derive(Debug)]
pub struct SessionTemporalOperationPermit<Operation> {
    _operation: PhantomData<fn() -> Operation>,
}

pub(super) trait SessionTemporalOperation {
    const CAPABILITY: SessionTemporalCapabilityV1;
}

impl<Operation> SessionTemporalOperationPermit<Operation> {
    pub(super) fn grant(capabilities: &SessionTemporalCapabilitiesV1) -> SessionStoreResult<Self>
    where
        Operation: SessionTemporalOperation,
    {
        require_declared_capability(capabilities, Operation::CAPABILITY)?;
        Ok(Self {
            _operation: PhantomData,
        })
    }
}

macro_rules! declare_session_temporal_operation {
    ($operation:ident, $permit:ident, $capability:expr) => {
        #[doc = concat!("Operation marker for [`", stringify!($permit), "`].")]
        #[derive(Debug)]
        pub struct $operation;

        impl $operation {
            pub const REQUIRED_CAPABILITY: SessionTemporalCapabilityV1 = $capability;
        }

        impl SessionTemporalOperation for $operation {
            const CAPABILITY: SessionTemporalCapabilityV1 = Self::REQUIRED_CAPABILITY;
        }

        #[doc = concat!("Permit authorizing the `", stringify!($operation), "` operation.")]
        pub type $permit = SessionTemporalOperationPermit<$operation>;
    };
}

declare_session_temporal_operation!(
    SessionSnapshotFreezeOperation,
    SessionSnapshotFreezePermit,
    SessionTemporalCapabilityV1::FrozenWatermarks
);
declare_session_temporal_operation!(
    SessionTemporalPageRetrieveOperation,
    SessionTemporalPageRetrievePermit,
    SessionTemporalCapabilityV1::FrozenWatermarks
);
declare_session_temporal_operation!(
    SessionGenerationRebuildBeginOperation,
    SessionGenerationRebuildBeginPermit,
    SessionTemporalCapabilityV1::GenerationRebuild
);
declare_session_temporal_operation!(
    SessionProjectionBatchPersistOperation,
    SessionProjectionBatchPersistPermit,
    SessionTemporalCapabilityV1::GenerationRebuild
);
declare_session_temporal_operation!(
    SessionGenerationActivateOperation,
    SessionGenerationActivatePermit,
    SessionTemporalCapabilityV1::GenerationRebuild
);
declare_session_temporal_operation!(
    SessionRefreshBeginOrJoinOperation,
    SessionRefreshBeginOrJoinPermit,
    SessionTemporalCapabilityV1::RefreshJoin
);
declare_session_temporal_operation!(
    SessionRefreshProgressPersistOperation,
    SessionRefreshProgressPersistPermit,
    SessionTemporalCapabilityV1::RefreshProgressPersistence
);
declare_session_temporal_operation!(
    SessionRefreshProgressReadOperation,
    SessionRefreshProgressReadPermit,
    SessionTemporalCapabilityV1::RefreshProgressPersistence
);
declare_session_temporal_operation!(
    SessionRefreshCompleteOperation,
    SessionRefreshCompletePermit,
    SessionTemporalCapabilityV1::RefreshProgressPersistence
);
declare_session_temporal_operation!(
    SessionRefreshFailOperation,
    SessionRefreshFailPermit,
    SessionTemporalCapabilityV1::RefreshProgressPersistence
);
declare_session_temporal_operation!(
    SessionRefreshCancelOperation,
    SessionRefreshCancelPermit,
    SessionTemporalCapabilityV1::RefreshCancellation
);
declare_session_temporal_operation!(
    SessionRefreshReceiptReadOperation,
    SessionRefreshReceiptReadPermit,
    SessionTemporalCapabilityV1::RefreshProgressPersistence
);
pub(super) fn require_snapshot_session(
    session_id: &SessionId,
    snapshot: &SessionTemporalSnapshotV1,
    context: &'static str,
) -> SessionStoreResult<()> {
    if session_id != snapshot.session_id() {
        return Err(SessionStoreError::SessionMismatch { context });
    }
    Ok(())
}

pub(super) fn require_capability(
    snapshot: &SessionTemporalSnapshotV1,
    capability: SessionTemporalCapabilityV1,
) -> SessionStoreResult<()> {
    require_declared_capability(snapshot.capabilities(), capability)
}

pub(super) fn require_declared_capability(
    capabilities: &SessionTemporalCapabilitiesV1,
    capability: SessionTemporalCapabilityV1,
) -> SessionStoreResult<()> {
    if !capabilities.supports(capability) {
        return Err(SessionStoreError::UnsupportedCapability { capability });
    }
    Ok(())
}

pub(super) fn require_newer_generation(
    candidate: SessionProjectionGenerationV1,
    active: SessionProjectionGenerationV1,
) -> SessionStoreResult<()> {
    if candidate <= active {
        return Err(SessionStoreError::StaleGeneration {
            expected: candidate,
            actual: active,
        });
    }
    Ok(())
}
