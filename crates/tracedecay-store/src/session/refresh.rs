use std::fmt;
use std::future::Future;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    SessionId, SessionRefreshKeyV1, SessionRefreshOperationIdV1, SessionSourceCoverageReceiptV1,
    SessionTemporalCoverageRequestV1, TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::common::{
    SessionRefreshBeginOrJoinPermit, SessionRefreshCancelPermit, SessionRefreshCompletePermit,
    SessionRefreshFailPermit, SessionRefreshFailureCodeInvalidReasonV1,
    SessionRefreshProgressPersistPermit, SessionRefreshProgressReadPermit,
    SessionRefreshReceiptReadPermit, SessionRefreshStateV1, SessionStoreError, SessionStoreResult,
    SessionTemporalCapabilityProvider,
};

/// Source and committed frontier for a durable refresh operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRefreshFrontierV1 {
    observed_through: u64,
    committed_through: u64,
}

impl SessionRefreshFrontierV1 {
    pub fn new(observed_through: u64, committed_through: u64) -> SessionStoreResult<Self> {
        if committed_through > observed_through {
            return Err(SessionStoreError::InvalidRefreshFrontier {
                observed_through,
                committed_through,
            });
        }
        Ok(Self {
            observed_through,
            committed_through,
        })
    }

    pub const fn observed_through(&self) -> u64 {
        self.observed_through
    }

    pub const fn committed_through(&self) -> u64 {
        self.committed_through
    }

    pub const fn is_complete(&self) -> bool {
        self.observed_through == self.committed_through
    }
}

/// Request to create or join the durable refresh for an equivalent session frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshBeginOrJoinRequestV1 {
    session_id: SessionId,
    target_frontier: SessionRefreshFrontierV1,
    refresh_key: Option<SessionRefreshKeyV1>,
    coverage_request: SessionTemporalCoverageRequestV1,
}

impl SessionRefreshBeginOrJoinRequestV1 {
    pub fn new(session_id: SessionId, target_frontier: SessionRefreshFrontierV1) -> Self {
        Self {
            session_id,
            target_frontier,
            refresh_key: None,
            coverage_request: SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
        }
    }

    pub fn with_refresh_key(mut self, refresh_key: SessionRefreshKeyV1) -> Self {
        self.refresh_key = Some(refresh_key);
        self
    }

    /// Selects the temporal coverage reported to this caller. The default is
    /// `Current`; query-only coverage does not alter refresh operation identity.
    pub fn with_coverage_request(
        mut self,
        coverage_request: SessionTemporalCoverageRequestV1,
    ) -> Self {
        self.coverage_request = coverage_request;
        self
    }

    pub const fn coverage_request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.coverage_request
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn target_frontier(&self) -> SessionRefreshFrontierV1 {
        self.target_frontier
    }

    pub fn refresh_key(&self) -> Option<&SessionRefreshKeyV1> {
        self.refresh_key.as_ref()
    }

    /// Join equivalence binds only projection-affecting source and scope inputs.
    pub fn is_equivalent_to(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.target_frontier == other.target_frontier
            && self.refresh_key == other.refresh_key
    }
}

/// Whether a begin-or-join request created or joined an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshDispositionV1 {
    Started,
    Joined,
}

/// Durable receipt for beginning or joining a refresh operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshBeginOrJoinReceiptV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    target_frontier: SessionRefreshFrontierV1,
    disposition: SessionRefreshDispositionV1,
    accepted_at: UtcMicros,
}

impl SessionRefreshBeginOrJoinReceiptV1 {
    pub fn new(
        operation_id: SessionRefreshOperationIdV1,
        session_id: SessionId,
        target_frontier: SessionRefreshFrontierV1,
        disposition: SessionRefreshDispositionV1,
        accepted_at: UtcMicros,
    ) -> Self {
        Self {
            operation_id,
            session_id,
            target_frontier,
            disposition,
            accepted_at,
        }
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn target_frontier(&self) -> SessionRefreshFrontierV1 {
        self.target_frontier
    }

    pub const fn disposition(&self) -> SessionRefreshDispositionV1 {
        self.disposition
    }

    pub const fn accepted_at(&self) -> UtcMicros {
        self.accepted_at
    }
}

/// Query for the last committed progress of one session refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshProgressRequestV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
}

impl SessionRefreshProgressRequestV1 {
    pub fn new(operation_id: SessionRefreshOperationIdV1, session_id: SessionId) -> Self {
        Self {
            operation_id,
            session_id,
        }
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

/// Progress committed by a running refresh operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshProgressV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: TemporalCoverageCountsV1,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
    committed_batches: u64,
    committed_records: u64,
    updated_at: UtcMicros,
}

impl SessionRefreshProgressV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_id: SessionRefreshOperationIdV1,
        session_id: SessionId,
        frontier: SessionRefreshFrontierV1,
        coverage: TemporalCoverageCountsV1,
        committed_batches: u64,
        committed_records: u64,
        updated_at: UtcMicros,
    ) -> Self {
        Self {
            operation_id,
            session_id,
            frontier,
            coverage,
            source_coverage: None,
            committed_batches,
            committed_records,
            updated_at,
        }
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frontier(&self) -> SessionRefreshFrontierV1 {
        self.frontier
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn with_source_coverage(mut self, source_coverage: SessionSourceCoverageReceiptV1) -> Self {
        self.source_coverage = Some(source_coverage);
        self
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }

    pub const fn committed_batches(&self) -> u64 {
        self.committed_batches
    }

    pub const fn committed_records(&self) -> u64 {
        self.committed_records
    }

    pub const fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }

    /// Validate monotonic durable progress for the same operation.
    pub fn validate_successor(&self, next: &Self) -> SessionStoreResult<()> {
        if self.operation_id != next.operation_id || self.session_id != next.session_id {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "refresh progress successor",
            });
        }
        let current = self.coverage;
        let candidate = next.coverage;
        if self.frontier.observed_through != next.frontier.observed_through
            || next.frontier.committed_through < self.frontier.committed_through
            || next.committed_batches < self.committed_batches
            || next.committed_records < self.committed_records
            || candidate.visible < current.visible
            || candidate.hidden < current.hidden
            || candidate.unknown < current.unknown
            || candidate.redacted < current.redacted
            || !source_coverage_is_successor(
                self.source_coverage.as_ref(),
                next.source_coverage.as_ref(),
            )
            || next.updated_at < self.updated_at
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh progress successor",
            });
        }
        Ok(())
    }
}

fn source_coverage_is_successor(
    current: Option<&SessionSourceCoverageReceiptV1>,
    next: Option<&SessionSourceCoverageReceiptV1>,
) -> bool {
    match (current, next) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(current), Some(next)) => {
            current.request() == next.request()
                && current.sources().len() == next.sources().len()
                && current
                    .sources()
                    .iter()
                    .zip(next.sources())
                    .all(|(current, next)| {
                        current.source_id() == next.source_id()
                            && current.observed_frontier() == next.observed_frontier()
                            && current.target_watermark() == next.target_watermark()
                            && next.committed_frontier() >= current.committed_frontier()
                    })
        }
    }
}

/// Request to complete a refresh at its fully committed target frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshCompletionRequestV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: TemporalCoverageCountsV1,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
}

impl SessionRefreshCompletionRequestV1 {
    pub fn new(
        operation_id: SessionRefreshOperationIdV1,
        session_id: SessionId,
        frontier: SessionRefreshFrontierV1,
        coverage: TemporalCoverageCountsV1,
    ) -> SessionStoreResult<Self> {
        if !frontier.is_complete() {
            return Err(SessionStoreError::InvalidRefreshState {
                operation_id,
                state: SessionRefreshStateV1::Running,
            });
        }
        Ok(Self {
            operation_id,
            session_id,
            frontier,
            coverage,
            source_coverage: None,
        })
    }

    pub fn with_source_coverage(mut self, source_coverage: SessionSourceCoverageReceiptV1) -> Self {
        self.source_coverage = Some(source_coverage);
        self
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frontier(&self) -> SessionRefreshFrontierV1 {
        self.frontier
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }
}

/// Validated, persistence-stable code for a non-sensitive refresh failure class.
///
/// JSON deserialization always routes through [`SessionRefreshFailureCodeV1::new`]
/// so empty, oversized, control-bearing, and noncanonical/sensitive-shaped values
/// are rejected with the same typed errors as the constructor.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SessionRefreshFailureCodeV1(String);

impl SessionRefreshFailureCodeV1 {
    pub const MAX_LEN: usize = 64;

    pub fn new(value: impl Into<String>) -> SessionStoreResult<Self> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(SessionStoreError::InvalidRefreshFailureCode {
                reason: SessionRefreshFailureCodeInvalidReasonV1::Empty,
            });
        }
        if bytes.len() > Self::MAX_LEN {
            return Err(SessionStoreError::InvalidRefreshFailureCode {
                reason: SessionRefreshFailureCodeInvalidReasonV1::TooLong,
            });
        }
        if bytes.iter().any(u8::is_ascii_control) {
            return Err(SessionStoreError::InvalidRefreshFailureCode {
                reason: SessionRefreshFailureCodeInvalidReasonV1::ContainsControl,
            });
        }
        if !is_canonical_failure_code(bytes) {
            return Err(SessionStoreError::InvalidRefreshFailureCode {
                reason: SessionRefreshFailureCodeInvalidReasonV1::NonCanonical,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SessionRefreshFailureCodeV1 {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SessionRefreshFailureCodeV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SessionRefreshFailureCodeV1 {
    type Err = SessionStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for SessionRefreshFailureCodeV1 {
    type Error = SessionStoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SessionRefreshFailureCodeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn is_canonical_failure_code(bytes: &[u8]) -> bool {
    bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && bytes.last() != Some(&b'_')
        && !bytes.windows(2).any(|window| window == b"__")
}

/// Request to terminate a refresh with a stable, non-sensitive failure code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshFailureRequestV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: TemporalCoverageCountsV1,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
    failure_code: SessionRefreshFailureCodeV1,
}

impl SessionRefreshFailureRequestV1 {
    pub fn new(
        operation_id: SessionRefreshOperationIdV1,
        session_id: SessionId,
        frontier: SessionRefreshFrontierV1,
        coverage: TemporalCoverageCountsV1,
        failure_code: impl Into<String>,
    ) -> SessionStoreResult<Self> {
        Ok(Self {
            operation_id,
            session_id,
            frontier,
            coverage,
            source_coverage: None,
            failure_code: SessionRefreshFailureCodeV1::new(failure_code)?,
        })
    }

    pub fn with_source_coverage(mut self, source_coverage: SessionSourceCoverageReceiptV1) -> Self {
        self.source_coverage = Some(source_coverage);
        self
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frontier(&self) -> SessionRefreshFrontierV1 {
        self.frontier
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }

    pub fn failure_code(&self) -> &SessionRefreshFailureCodeV1 {
        &self.failure_code
    }
}

/// Request to cancel a refresh after its last committed frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshCancellationRequestV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: TemporalCoverageCountsV1,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
}

impl SessionRefreshCancellationRequestV1 {
    pub fn new(
        operation_id: SessionRefreshOperationIdV1,
        session_id: SessionId,
        frontier: SessionRefreshFrontierV1,
        coverage: TemporalCoverageCountsV1,
    ) -> Self {
        Self {
            operation_id,
            session_id,
            frontier,
            coverage,
            source_coverage: None,
        }
    }

    pub fn with_source_coverage(mut self, source_coverage: SessionSourceCoverageReceiptV1) -> Self {
        self.source_coverage = Some(source_coverage);
        self
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frontier(&self) -> SessionRefreshFrontierV1 {
        self.frontier
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }
}

/// Terminal state of a durable refresh operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshTerminalStateV1 {
    Complete,
    Failed,
    Cancelled,
}

/// Terminal refresh receipt with the last committed frontier and explicit coverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshReceiptV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: TemporalCoverageCountsV1,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
    state: SessionRefreshTerminalStateV1,
    failure_code: Option<SessionRefreshFailureCodeV1>,
    terminal_at: UtcMicros,
}

impl SessionRefreshReceiptV1 {
    pub fn completed(request: SessionRefreshCompletionRequestV1, terminal_at: UtcMicros) -> Self {
        Self {
            operation_id: request.operation_id,
            session_id: request.session_id,
            frontier: request.frontier,
            coverage: request.coverage,
            source_coverage: request.source_coverage,
            state: SessionRefreshTerminalStateV1::Complete,
            failure_code: None,
            terminal_at,
        }
    }

    pub fn failed(request: SessionRefreshFailureRequestV1, terminal_at: UtcMicros) -> Self {
        Self {
            operation_id: request.operation_id,
            session_id: request.session_id,
            frontier: request.frontier,
            coverage: request.coverage,
            source_coverage: request.source_coverage,
            state: SessionRefreshTerminalStateV1::Failed,
            failure_code: Some(request.failure_code),
            terminal_at,
        }
    }

    pub fn cancelled(request: SessionRefreshCancellationRequestV1, terminal_at: UtcMicros) -> Self {
        Self {
            operation_id: request.operation_id,
            session_id: request.session_id,
            frontier: request.frontier,
            coverage: request.coverage,
            source_coverage: request.source_coverage,
            state: SessionRefreshTerminalStateV1::Cancelled,
            failure_code: None,
            terminal_at,
        }
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn frontier(&self) -> SessionRefreshFrontierV1 {
        self.frontier
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn with_source_coverage(mut self, source_coverage: SessionSourceCoverageReceiptV1) -> Self {
        self.source_coverage = Some(source_coverage);
        self
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }

    pub const fn state(&self) -> SessionRefreshTerminalStateV1 {
        self.state
    }

    pub fn failure_code(&self) -> Option<&SessionRefreshFailureCodeV1> {
        self.failure_code.as_ref()
    }

    pub const fn terminal_at(&self) -> UtcMicros {
        self.terminal_at
    }

    /// Terminal receipts must preserve identity and never regress committed
    /// progress. Complete additionally requires a fully committed frontier.
    pub fn validate_transition_from(
        &self,
        progress: &SessionRefreshProgressV1,
    ) -> SessionStoreResult<()> {
        if self.operation_id != progress.operation_id || self.session_id != progress.session_id {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "refresh terminal transition",
            });
        }
        if self.frontier.observed_through != progress.frontier.observed_through
            || self.frontier.committed_through < progress.frontier.committed_through
            || self.terminal_at < progress.updated_at
            || !source_coverage_is_successor(
                progress.source_coverage.as_ref(),
                self.source_coverage.as_ref(),
            )
            || (self.state == SessionRefreshTerminalStateV1::Complete
                && !self.frontier.is_complete())
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh terminal transition",
            });
        }
        Ok(())
    }
}

/// Query for one terminal refresh receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshReceiptRequestV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
}

impl SessionRefreshReceiptRequestV1 {
    pub fn new(operation_id: SessionRefreshOperationIdV1, session_id: SessionId) -> Self {
        Self {
            operation_id,
            session_id,
        }
    }

    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

/// Explicit durable refresh operations.
///
/// Public caller entrypoints grant an operation-specific permit before
/// dispatch. Low-level `*_supported` methods require their exact unforgeable
/// permit and are therefore unreachable without the matching capability guard.
pub trait SessionRefreshStore: SessionTemporalCapabilityProvider + Send + Sync {
    fn begin_or_join_session_refresh(
        &self,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1>> + Send {
        async move {
            let permit =
                SessionRefreshBeginOrJoinPermit::grant(self.session_temporal_capabilities())?;
            self.begin_or_join_session_refresh_supported(permit, request)
                .await
        }
    }

    fn begin_or_join_session_refresh_supported(
        &self,
        permit: SessionRefreshBeginOrJoinPermit,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1>> + Send;

    fn persist_session_refresh_progress(
        &self,
        progress: SessionRefreshProgressV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshProgressV1>> + Send {
        async move {
            let permit =
                SessionRefreshProgressPersistPermit::grant(self.session_temporal_capabilities())?;
            self.persist_session_refresh_progress_supported(permit, progress)
                .await
        }
    }

    fn persist_session_refresh_progress_supported(
        &self,
        permit: SessionRefreshProgressPersistPermit,
        progress: SessionRefreshProgressV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshProgressV1>> + Send;

    fn session_refresh_progress(
        &self,
        request: SessionRefreshProgressRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshProgressV1>>> + Send {
        async move {
            let permit =
                SessionRefreshProgressReadPermit::grant(self.session_temporal_capabilities())?;
            self.session_refresh_progress_supported(permit, request)
                .await
        }
    }

    fn session_refresh_progress_supported(
        &self,
        permit: SessionRefreshProgressReadPermit,
        request: SessionRefreshProgressRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshProgressV1>>> + Send;

    fn complete_session_refresh(
        &self,
        request: SessionRefreshCompletionRequestV1,
        execution_control: ExecutionControl,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        async move {
            let permit = SessionRefreshCompletePermit::grant(self.session_temporal_capabilities())?;
            self.complete_session_refresh_supported(permit, request, execution_control)
                .await
        }
    }

    fn complete_session_refresh_supported(
        &self,
        permit: SessionRefreshCompletePermit,
        request: SessionRefreshCompletionRequestV1,
        execution_control: ExecutionControl,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send;

    fn fail_session_refresh(
        &self,
        request: SessionRefreshFailureRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        async move {
            let permit = SessionRefreshFailPermit::grant(self.session_temporal_capabilities())?;
            self.fail_session_refresh_supported(permit, request).await
        }
    }

    fn fail_session_refresh_supported(
        &self,
        permit: SessionRefreshFailPermit,
        request: SessionRefreshFailureRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send;

    fn cancel_session_refresh(
        &self,
        request: SessionRefreshCancellationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        async move {
            let permit = SessionRefreshCancelPermit::grant(self.session_temporal_capabilities())?;
            self.cancel_session_refresh_supported(permit, request).await
        }
    }

    fn cancel_session_refresh_supported(
        &self,
        permit: SessionRefreshCancelPermit,
        request: SessionRefreshCancellationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send;

    fn session_refresh_receipt(
        &self,
        request: SessionRefreshReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshReceiptV1>>> + Send {
        async move {
            let permit =
                SessionRefreshReceiptReadPermit::grant(self.session_temporal_capabilities())?;
            self.session_refresh_receipt_supported(permit, request)
                .await
        }
    }

    fn session_refresh_receipt_supported(
        &self,
        permit: SessionRefreshReceiptReadPermit,
        request: SessionRefreshReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshReceiptV1>>> + Send;
}
