use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::Poll;
use std::time::Instant;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    LogicalCopyRecordV1, RetrievalGrainV1, SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES,
    SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS, SessionContractError, SessionId,
    SessionSourceCoverageReasonV1, SessionSourceCoverageReceiptV1, SessionSourceCoverageStateV1,
    SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1, SessionSummaryRecordV1,
    SessionTemporalCoverageRequestV1, SignedCursorKeyRefV1, TemporalModeV1,
};
use zeroize::Zeroizing;

use super::candidates::CandidatePlan;
use super::ranking::RankingCandidate;
use super::resolution::summary::SummarySourceState;
use super::resolution::types::{ResolutionAssertion, ResolutionOccurrence, ValidatedAuthorization};

const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;
/// Absolute ceilings for bounded page construction — callers may not request
/// attacker-chosen `usize::MAX` budgets that force huge pre-allocation.
const MAX_READ_ITEMS: usize = 8_192;
const MAX_READ_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_READ_ITEM_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_ITEMS_CAP: usize = 1_024;
const MAX_CONTINUATION_KEY_BYTES: usize = 4_096;
const MAX_BOUNDED_PAGE_PREALLOC: usize = 64;
const MAX_CURSOR_SECRET_BYTES: usize = 256;
const PROFILE_ROOT_PROJECT_KEY: &str = "user";
pub const MAX_TEMPORAL_PARTICIPANTS: usize = SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS;
pub const MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES: usize =
    SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TemporalPortError {
    #[error("{field} is not a canonical binding")]
    InvalidBinding { field: &'static str },
    #[error("temporal execution generation must be non-zero")]
    ZeroGeneration,
    #[error("temporal execution snapshot was not authorized")]
    UnauthorizedSnapshot,
    #[error("temporal execution participant manifest must not be empty")]
    EmptyParticipantManifest,
    #[error("temporal execution participant manifest contains a duplicate source")]
    DuplicateParticipant,
    #[error("temporal execution participant manifest has {observed} entries; maximum is {maximum}")]
    ParticipantLimitExceeded { observed: usize, maximum: usize },
    #[error(
        "temporal execution participant manifest has {observed} canonical bytes; maximum is {maximum}"
    )]
    ParticipantManifestBytesExceeded { observed: usize, maximum: usize },
    #[error("temporal kernel {field} version must be non-zero")]
    ZeroVersion { field: &'static str },
    #[error("temporal execution was cancelled")]
    Cancelled,
    #[error("temporal execution deadline elapsed")]
    DeadlineExceeded,
    #[error("temporal execution exceeded its {resource} budget")]
    BudgetExceeded { resource: &'static str },
    #[error("temporal read failed during {operation}: {message}")]
    Read {
        operation: &'static str,
        message: String,
    },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ExecutionLimitTighteningError {
    #[error(
        "temporal execution limit {field} cannot increase after authorization \
         (authorized {authorized}, requested {requested})"
    )]
    WouldLoosen {
        field: &'static str,
        authorized: usize,
        requested: usize,
    },
    #[error(transparent)]
    InvalidLimits(#[from] TemporalPortError),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingDigest(String);

impl BindingDigest {
    pub fn new(field: &'static str, value: impl Into<String>) -> Result<Self, TemporalPortError> {
        let value = value.into();
        let valid = value.strip_prefix(SHA256_PREFIX).is_some_and(|hex| {
            hex.len() == SHA256_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid {
            return Err(TemporalPortError::InvalidBinding { field });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub candidate_limit: usize,
    pub candidate_total_bytes: usize,
    pub candidate_item_bytes: usize,
    pub candidate_key_bytes: usize,
    pub candidate_stable_id_bytes: usize,
    pub candidate_anchor_id_bytes: usize,
    pub candidate_metadata_field_bytes: usize,
    pub record_limit: usize,
    pub record_total_bytes: usize,
    pub record_item_bytes: usize,
    pub record_key_bytes: usize,
    pub hydration_limit: usize,
    pub hydration_total_bytes: usize,
    pub hydration_payload_bytes: usize,
    pub hydration_chunk_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            candidate_limit: 256,
            candidate_total_bytes: 4 * 1024 * 1024,
            candidate_item_bytes: 256 * 1024,
            candidate_key_bytes: 256,
            candidate_stable_id_bytes: 4 * 1024,
            candidate_anchor_id_bytes: 4 * 1024,
            candidate_metadata_field_bytes: 64 * 1024,
            record_limit: 1024,
            record_total_bytes: 16 * 1024 * 1024,
            record_item_bytes: 1024 * 1024,
            record_key_bytes: 256,
            hydration_limit: 64,
            hydration_total_bytes: 8 * 1024 * 1024,
            hydration_payload_bytes: 1024 * 1024,
            hydration_chunk_bytes: 64 * 1024,
        }
    }
}

impl ExecutionLimits {
    pub fn validate(self) -> Result<Self, TemporalPortError> {
        for (resource, value, max) in [
            ("candidate item count", self.candidate_limit, MAX_READ_ITEMS),
            (
                "candidate total bytes",
                self.candidate_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "candidate item bytes",
                self.candidate_item_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate key bytes",
                self.candidate_key_bytes,
                MAX_CONTINUATION_KEY_BYTES,
            ),
            ("record item count", self.record_limit, MAX_READ_ITEMS),
            (
                "record total bytes",
                self.record_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "record item bytes",
                self.record_item_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "record key bytes",
                self.record_key_bytes,
                MAX_CONTINUATION_KEY_BYTES,
            ),
            ("hydration item count", self.hydration_limit, MAX_READ_ITEMS),
            (
                "hydration total bytes",
                self.hydration_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "hydration payload bytes",
                self.hydration_payload_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "hydration chunk bytes",
                self.hydration_chunk_bytes,
                MAX_READ_ITEM_BYTES,
            ),
        ] {
            if value == 0 || value > max {
                return Err(TemporalPortError::BudgetExceeded { resource });
            }
        }
        for (resource, value, max) in [
            (
                "candidate stable id bytes",
                self.candidate_stable_id_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate anchor id bytes",
                self.candidate_anchor_id_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate metadata field bytes",
                self.candidate_metadata_field_bytes,
                MAX_READ_ITEM_BYTES,
            ),
        ] {
            if value == 0 || value > max {
                return Err(TemporalPortError::BudgetExceeded { resource });
            }
        }
        Ok(self)
    }
}

#[derive(Clone)]
pub struct ExecutionControl {
    cancellation: Arc<AtomicBool>,
    deadline: Option<Instant>,
    remaining_work: Option<Arc<AtomicUsize>>,
}

impl ExecutionControl {
    pub fn new(deadline: Option<Instant>) -> Self {
        Self {
            cancellation: Arc::new(AtomicBool::new(false)),
            deadline,
            remaining_work: None,
        }
    }

    #[must_use]
    pub fn with_work_limit(mut self, work_units: usize) -> Self {
        self.remaining_work = Some(Arc::new(AtomicUsize::new(work_units)));
        self
    }

    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }

    pub fn checkpoint(&self) -> Result<(), TemporalPortError> {
        self.check_cancellation_and_deadline()?;
        if self.remaining_work.as_ref().is_some_and(|remaining| {
            remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_sub(1)
                })
                .is_err()
        }) {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "work units",
            });
        }
        Ok(())
    }

    fn check_cancellation_and_deadline(&self) -> Result<(), TemporalPortError> {
        if self.cancellation.load(Ordering::Acquire) {
            return Err(TemporalPortError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(TemporalPortError::DeadlineExceeded);
        }
        Ok(())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

pub(super) async fn await_controlled<T, E>(
    control: &ExecutionControl,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E>
where
    E: From<TemporalPortError>,
{
    let mut future = Box::pin(future);
    std::future::poll_fn(|context| {
        if let Err(error) = control.checkpoint() {
            return Poll::Ready(Err(error.into()));
        }
        match future.as_mut().poll(context) {
            Poll::Ready(result) => match control.check_cancellation_and_deadline() {
                Ok(()) => Poll::Ready(result),
                Err(error) => Poll::Ready(Err(error.into())),
            },
            Poll::Pending => match control.checkpoint() {
                Ok(()) => Poll::Pending,
                Err(error) => Poll::Ready(Err(error.into())),
            },
        }
    })
    .await
}

impl Default for ExecutionControl {
    fn default() -> Self {
        Self::new(None)
    }
}

impl fmt::Debug for ExecutionControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionControl")
            .field("cancelled", &self.is_cancelled())
            .field("deadline", &self.deadline)
            .field(
                "remaining_work",
                &self
                    .remaining_work
                    .as_ref()
                    .map(|value| value.load(Ordering::Acquire)),
            )
            .finish()
    }
}

impl PartialEq for ExecutionControl {
    fn eq(&self, other: &Self) -> bool {
        self.is_cancelled() == other.is_cancelled()
            && self.deadline == other.deadline
            && self
                .remaining_work
                .as_ref()
                .map(|value| value.load(Ordering::Acquire))
                == other
                    .remaining_work
                    .as_ref()
                    .map(|value| value.load(Ordering::Acquire))
    }
}

impl Eq for ExecutionControl {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TemporalRetrievalScope {
    Session(SessionId),
    AllSessionsInAuthorizedRoot,
}

impl TemporalRetrievalScope {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::AllSessionsInAuthorizedRoot => "all_sessions_in_authorized_root",
        }
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Session(session_id) => Some(session_id),
            Self::AllSessionsInAuthorizedRoot => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalAuthorizedRoot {
    profile_id: String,
    project_id: Option<String>,
    store_id: String,
    root_id: String,
}

impl TemporalAuthorizedRoot {
    pub fn profile(
        profile_id: impl Into<String>,
        store_id: impl Into<String>,
        root_id: impl Into<String>,
    ) -> Result<Self, TemporalPortError> {
        Self::new(profile_id.into(), None, store_id.into(), root_id.into())
    }

    pub fn project(
        profile_id: impl Into<String>,
        project_id: impl Into<String>,
        store_id: impl Into<String>,
        root_id: impl Into<String>,
    ) -> Result<Self, TemporalPortError> {
        let project_id = project_id.into();
        if project_id == PROFILE_ROOT_PROJECT_KEY {
            return Err(TemporalPortError::InvalidBinding {
                field: "project_id",
            });
        }
        Self::new(
            profile_id.into(),
            Some(project_id),
            store_id.into(),
            root_id.into(),
        )
    }

    fn new(
        profile_id: String,
        project_id: Option<String>,
        store_id: String,
        root_id: String,
    ) -> Result<Self, TemporalPortError> {
        validate_label("profile_id", &profile_id)?;
        if let Some(project_id) = &project_id {
            validate_label("project_id", project_id)?;
        }
        validate_label("store_id", &store_id)?;
        validate_label("root_id", &root_id)?;
        Ok(Self {
            profile_id,
            project_id,
            store_id,
            root_id,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    pub fn project_key(&self) -> &str {
        self.project_id
            .as_deref()
            .unwrap_or(PROFILE_ROOT_PROJECT_KEY)
    }
}

fn validate_label(field: &'static str, value: &str) -> Result<(), TemporalPortError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(TemporalPortError::InvalidBinding { field });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TemporalSessionScopeFilterV1 {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "parents_only")]
    ParentsOnly,
    #[serde(rename = "subagents_only")]
    SubagentsOnly,
}

impl Default for TemporalSessionScopeFilterV1 {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TemporalMessageTypeFilterV1 {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "direct_user")]
    DirectUser,
    #[serde(rename = "tool_result")]
    ToolResult,
}

impl Default for TemporalMessageTypeFilterV1 {
    fn default() -> Self {
        Self::All
    }
}

/// Canonical semantic eligibility applied by the read port before candidates
/// enter ranking, limiting, record loading, or hydration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TemporalCandidateFilterV1 {
    pub project_key: Option<String>,
    pub parent_session_id: Option<String>,
    pub source: Option<String>,
    pub include_summaries: bool,
    pub session_scope: TemporalSessionScopeFilterV1,
    pub message_type: TemporalMessageTypeFilterV1,
    pub roles: Vec<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub git_branch: Option<String>,
    pub git_worktree: Option<String>,
    pub git_commit: Option<String>,
    pub workflow_run: Option<String>,
    pub workflow_agent: Option<String>,
    pub goals: bool,
}

impl TemporalCandidateFilterV1 {
    pub fn validate(&self) -> Result<(), TemporalPortError> {
        if self
            .start_time
            .zip(self.end_time)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(TemporalPortError::InvalidBinding {
                field: "semantic_time_range",
            });
        }
        if self.workflow_agent.is_some() && self.workflow_run.is_none() {
            return Err(TemporalPortError::InvalidBinding {
                field: "workflow_agent",
            });
        }
        for (field, value) in [
            ("project_key", self.project_key.as_deref()),
            ("parent_session_id", self.parent_session_id.as_deref()),
            ("source", self.source.as_deref()),
            ("git_branch", self.git_branch.as_deref()),
            ("git_worktree", self.git_worktree.as_deref()),
            ("git_commit", self.git_commit.as_deref()),
            ("workflow_run", self.workflow_run.as_deref()),
            ("workflow_agent", self.workflow_agent.as_deref()),
        ] {
            if let Some(value) = value {
                validate_label(field, value)?;
            }
        }
        for role in &self.roles {
            validate_label("role", role)?;
        }
        if self.roles.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(TemporalPortError::InvalidBinding { field: "roles" });
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalSnapshotRequest {
    session_id: SessionId,
    retrieval_scope: TemporalRetrievalScope,
    authorized_root: Option<TemporalAuthorizedRoot>,
    provider_scope: Option<String>,
    root_digest: BindingDigest,
    request_digest: BindingDigest,
    filter_digest: BindingDigest,
    access_digest: BindingDigest,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    semantic_filter: TemporalCandidateFilterV1,
    limits: ExecutionLimits,
    control: ExecutionControl,
}

impl TemporalSnapshotRequest {
    pub fn new(
        session_id: SessionId,
        root_digest: impl Into<String>,
        request_digest: impl Into<String>,
        access_digest: impl Into<String>,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
    ) -> Result<Self, TemporalPortError> {
        let request_digest = BindingDigest::new("request_digest", request_digest)?;
        Ok(Self {
            retrieval_scope: TemporalRetrievalScope::Session(session_id.clone()),
            session_id,
            authorized_root: None,
            provider_scope: None,
            root_digest: BindingDigest::new("root_digest", root_digest)?,
            filter_digest: request_digest.clone(),
            request_digest,
            access_digest: BindingDigest::new("access_digest", access_digest)?,
            temporal_mode,
            grain,
            semantic_filter: TemporalCandidateFilterV1::default(),
            limits: ExecutionLimits::default(),
            control: ExecutionControl::default(),
        })
    }

    #[must_use]
    pub fn with_limits(mut self, limits: ExecutionLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_retrieval_scope(mut self, retrieval_scope: TemporalRetrievalScope) -> Self {
        if let TemporalRetrievalScope::Session(session_id) = &retrieval_scope {
            self.session_id = session_id.clone();
        }
        self.retrieval_scope = retrieval_scope;
        self
    }

    pub fn with_authorized_root(
        mut self,
        authorized_root: TemporalAuthorizedRoot,
    ) -> Result<Self, TemporalPortError> {
        validate_label("profile_id", authorized_root.profile_id())?;
        validate_label("store_id", authorized_root.store_id())?;
        validate_label("root_id", authorized_root.root_id())?;
        self.authorized_root = Some(authorized_root);
        Ok(self)
    }

    pub fn with_filter_digest(
        mut self,
        filter_digest: impl Into<String>,
    ) -> Result<Self, TemporalPortError> {
        self.filter_digest = BindingDigest::new("filter_digest", filter_digest)?;
        Ok(self)
    }

    pub fn with_provider_scope(
        mut self,
        provider_scope: Option<String>,
    ) -> Result<Self, TemporalPortError> {
        if provider_scope.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.trim() != value
                || value.len() > 512
                || value.chars().any(char::is_control)
        }) {
            return Err(TemporalPortError::InvalidBinding {
                field: "provider_scope",
            });
        }
        self.provider_scope = provider_scope;
        Ok(self)
    }

    pub fn with_semantic_filter(
        mut self,
        semantic_filter: TemporalCandidateFilterV1,
    ) -> Result<Self, TemporalPortError> {
        semantic_filter.validate()?;
        self.semantic_filter = semantic_filter;
        Ok(self)
    }

    #[must_use]
    pub fn with_cancellation_requested(self, requested: bool) -> Self {
        if requested {
            self.control.cancel();
        }
        self
    }

    #[must_use]
    pub fn with_execution_control(mut self, control: ExecutionControl) -> Self {
        self.control = control;
        self
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn retrieval_scope(&self) -> &TemporalRetrievalScope {
        &self.retrieval_scope
    }

    pub fn authorized_root(&self) -> Option<&TemporalAuthorizedRoot> {
        self.authorized_root.as_ref()
    }

    pub fn provider_scope(&self) -> Option<&str> {
        self.provider_scope.as_deref()
    }

    pub fn root_digest(&self) -> &BindingDigest {
        &self.root_digest
    }

    pub fn request_digest(&self) -> &BindingDigest {
        &self.request_digest
    }

    pub fn filter_digest(&self) -> &BindingDigest {
        &self.filter_digest
    }

    pub fn access_digest(&self) -> &BindingDigest {
        &self.access_digest
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub fn semantic_filter(&self) -> &TemporalCandidateFilterV1 {
        &self.semantic_filter
    }

    pub const fn limits(&self) -> ExecutionLimits {
        self.limits
    }

    pub fn cancellation_requested(&self) -> bool {
        self.control.is_cancelled()
    }

    pub fn execution_control(&self) -> &ExecutionControl {
        &self.control
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalWatermarks {
    pub generation: u64,
    pub source: u64,
    pub projection: u64,
    pub index: u64,
    pub summary: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelVersions {
    pub schema: u32,
    pub ranking: u32,
    pub configuration_digest: BindingDigest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TemporalParticipantAuthorization {
    #[serde(rename = "a")]
    Authorized,
    #[serde(rename = "n")]
    Denied,
}

impl Default for TemporalParticipantAuthorization {
    fn default() -> Self {
        Self::Denied
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TemporalSourceAccess {
    #[serde(rename = "a")]
    Available,
    #[serde(rename = "u")]
    Unavailable,
    #[serde(rename = "l")]
    Locked,
    #[serde(rename = "r")]
    RetentionWithheld,
    #[serde(rename = "d")]
    Deleted,
    #[serde(rename = "x")]
    Redacted,
    #[serde(rename = "n")]
    LegacyUnauthorized,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalParticipantGeneration {
    #[serde(rename = "s")]
    session_id: SessionId,
    #[serde(rename = "i")]
    source_id: String,
    #[serde(rename = "g")]
    generation: u64,
    #[serde(rename = "w")]
    source_watermark: u64,
    #[serde(rename = "p")]
    projection_watermark: u64,
    #[serde(rename = "r")]
    graph_watermark: u64,
    #[serde(rename = "x")]
    index_watermark: u64,
    #[serde(rename = "m")]
    summary_watermark: u64,
    #[serde(rename = "c")]
    configuration_digest: String,
    #[serde(rename = "a")]
    authorization_digest: String,
    #[serde(default, rename = "q")]
    authorization: TemporalParticipantAuthorization,
    #[serde(rename = "z")]
    access: TemporalSourceAccess,
}

impl TemporalParticipantGeneration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        source_id: impl Into<String>,
        watermarks: TemporalWatermarks,
        graph_watermark: u64,
        configuration_digest: &BindingDigest,
        authorization_digest: &BindingDigest,
        authorization: TemporalParticipantAuthorization,
        access: TemporalSourceAccess,
    ) -> Result<Self, TemporalPortError> {
        let source_id = source_id.into();
        validate_label("source_id", &source_id)?;
        if watermarks.generation == 0 {
            return Err(TemporalPortError::ZeroGeneration);
        }
        Ok(Self {
            session_id,
            source_id,
            generation: watermarks.generation,
            source_watermark: watermarks.source,
            projection_watermark: watermarks.projection,
            graph_watermark,
            index_watermark: watermarks.index,
            summary_watermark: watermarks.summary,
            configuration_digest: configuration_digest.as_str().to_string(),
            authorization_digest: authorization_digest.as_str().to_string(),
            authorization,
            access,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn watermarks(&self) -> TemporalWatermarks {
        TemporalWatermarks {
            generation: self.generation,
            source: self.source_watermark,
            projection: self.projection_watermark,
            index: self.index_watermark,
            summary: self.summary_watermark,
        }
    }

    pub const fn graph_watermark(&self) -> u64 {
        self.graph_watermark
    }

    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }

    pub fn authorization_digest(&self) -> &str {
        &self.authorization_digest
    }

    pub const fn authorization(&self) -> TemporalParticipantAuthorization {
        self.authorization
    }

    /// Snapshot authority is independent from per-source lifecycle state.
    ///
    /// The legacy unauthorized source wire state remains denied for old signed
    /// manifests, while every newly built manifest uses the dedicated,
    /// fail-closed authorization field.
    pub const fn is_authorized_for_snapshot(&self) -> bool {
        matches!(
            self.authorization,
            TemporalParticipantAuthorization::Authorized
        ) && !matches!(self.access, TemporalSourceAccess::LegacyUnauthorized)
    }

    pub const fn access(&self) -> TemporalSourceAccess {
        self.access
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalParticipantManifest {
    #[serde(rename = "p")]
    entries: Vec<TemporalParticipantGeneration>,
    #[serde(rename = "e")]
    epoch_digest: String,
}

impl TemporalParticipantManifest {
    pub fn new(mut entries: Vec<TemporalParticipantGeneration>) -> Result<Self, TemporalPortError> {
        if entries.is_empty() {
            return Err(TemporalPortError::EmptyParticipantManifest);
        }
        if entries.len() > MAX_TEMPORAL_PARTICIPANTS {
            return Err(TemporalPortError::ParticipantLimitExceeded {
                observed: entries.len(),
                maximum: MAX_TEMPORAL_PARTICIPANTS,
            });
        }
        entries.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.source_id.cmp(&right.source_id))
        });
        if entries.windows(2).any(|pair| {
            pair[0].session_id == pair[1].session_id && pair[0].source_id == pair[1].source_id
        }) {
            return Err(TemporalPortError::DuplicateParticipant);
        }
        let canonical = serde_json::to_vec(&entries).map_err(|error| TemporalPortError::Read {
            operation: "encode participant manifest",
            message: error.to_string(),
        })?;
        if canonical.len() > MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES {
            return Err(TemporalPortError::ParticipantManifestBytesExceeded {
                observed: canonical.len(),
                maximum: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES,
            });
        }
        let epoch_digest = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        Ok(Self {
            entries,
            epoch_digest,
        })
    }

    pub fn entries(&self) -> &[TemporalParticipantGeneration] {
        &self.entries
    }

    pub fn epoch_digest(&self) -> &str {
        &self.epoch_digest
    }

    pub fn source_coverage(
        &self,
        mode: TemporalModeV1,
    ) -> Result<SessionSourceCoverageReceiptV1, SessionContractError> {
        let request = SessionTemporalCoverageRequestV1::new(mode);
        let sources = self
            .entries
            .iter()
            .map(|entry| {
                let source_id = SessionSourceIdV1::new(format!(
                    "{}:{}",
                    entry.session_id.as_str(),
                    entry.source_id
                ))?;
                let observed = SessionSourceFrontierV1::new(entry.source_watermark);
                let committed = SessionSourceFrontierV1::new(entry.projection_watermark);
                if entry.is_authorized_for_snapshot()
                    && entry.access == TemporalSourceAccess::Available
                {
                    return SessionSourceCoverageV1::new(
                        source_id,
                        observed,
                        committed,
                        observed,
                        request.clone(),
                        Vec::new(),
                        Vec::new(),
                        if committed == observed {
                            SessionSourceCoverageStateV1::Fresh
                        } else {
                            SessionSourceCoverageStateV1::Stale
                        },
                        if committed == observed {
                            SessionSourceCoverageReasonV1::CaughtUp
                        } else {
                            SessionSourceCoverageReasonV1::ProjectionBehindSource {
                                lag: committed.lag_from(observed),
                            }
                        },
                    );
                }
                let (state, reason) = match entry.access {
                    TemporalSourceAccess::Locked => (
                        SessionSourceCoverageStateV1::Locked,
                        SessionSourceCoverageReasonV1::Locked,
                    ),
                    TemporalSourceAccess::RetentionWithheld | TemporalSourceAccess::Deleted => (
                        SessionSourceCoverageStateV1::RetentionWithheld,
                        SessionSourceCoverageReasonV1::RetentionWithheld,
                    ),
                    TemporalSourceAccess::Redacted => (
                        SessionSourceCoverageStateV1::Redacted,
                        SessionSourceCoverageReasonV1::Redacted,
                    ),
                    TemporalSourceAccess::Unavailable
                    | TemporalSourceAccess::LegacyUnauthorized => (
                        SessionSourceCoverageStateV1::Unavailable,
                        SessionSourceCoverageReasonV1::Unavailable,
                    ),
                    TemporalSourceAccess::Available => unreachable!(),
                };
                SessionSourceCoverageV1::new(
                    source_id,
                    observed,
                    committed,
                    observed,
                    request.clone(),
                    Vec::new(),
                    Vec::new(),
                    state,
                    reason,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        SessionSourceCoverageReceiptV1::new(request, sources)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalExecutionSnapshot {
    request: TemporalSnapshotRequest,
    watermarks: TemporalWatermarks,
    versions: KernelVersions,
    cursor_key: Option<SignedCursorKeyRefV1>,
    authorization: ValidatedAuthorization,
    participants: TemporalParticipantManifest,
    participant_manifest_authoritative: bool,
}

impl TemporalExecutionSnapshot {
    pub fn new_authorized(
        request: TemporalSnapshotRequest,
        watermarks: TemporalWatermarks,
        versions: KernelVersions,
        cursor_key: Option<SignedCursorKeyRefV1>,
        authorization: ValidatedAuthorization,
    ) -> Result<Self, TemporalPortError> {
        if !authorization.is_authorized() {
            return Err(TemporalPortError::UnauthorizedSnapshot);
        }
        request.limits().validate()?;
        if watermarks.generation == 0 {
            return Err(TemporalPortError::ZeroGeneration);
        }
        if versions.schema == 0 {
            return Err(TemporalPortError::ZeroVersion { field: "schema" });
        }
        if versions.ranking == 0 {
            return Err(TemporalPortError::ZeroVersion { field: "ranking" });
        }
        let participants =
            TemporalParticipantManifest::new(vec![TemporalParticipantGeneration::new(
                request.session_id().clone(),
                request.provider_scope().unwrap_or("all"),
                watermarks,
                watermarks.projection,
                &versions.configuration_digest,
                request.access_digest(),
                TemporalParticipantAuthorization::Authorized,
                TemporalSourceAccess::Available,
            )?])?;
        Ok(Self {
            request,
            watermarks,
            versions,
            cursor_key,
            authorization,
            participants,
            participant_manifest_authoritative: false,
        })
    }

    #[cfg(test)]
    pub fn new(
        request: TemporalSnapshotRequest,
        watermarks: TemporalWatermarks,
        versions: KernelVersions,
        cursor_key: Option<SignedCursorKeyRefV1>,
    ) -> Result<Self, TemporalPortError> {
        Self::new_authorized(
            request,
            watermarks,
            versions,
            cursor_key,
            ValidatedAuthorization::Authorized,
        )
    }

    pub fn request(&self) -> &TemporalSnapshotRequest {
        &self.request
    }

    pub fn with_limits(
        mut self,
        limits: ExecutionLimits,
    ) -> Result<Self, ExecutionLimitTighteningError> {
        let authorized = self.request.limits();
        // Keep this guard exhaustive so adding a limit field forces the
        // monotonic comparison and its parameterized tests to be updated.
        let ExecutionLimits {
            candidate_limit: _,
            candidate_total_bytes: _,
            candidate_item_bytes: _,
            candidate_key_bytes: _,
            candidate_stable_id_bytes: _,
            candidate_anchor_id_bytes: _,
            candidate_metadata_field_bytes: _,
            record_limit: _,
            record_total_bytes: _,
            record_item_bytes: _,
            record_key_bytes: _,
            hydration_limit: _,
            hydration_total_bytes: _,
            hydration_payload_bytes: _,
            hydration_chunk_bytes: _,
        } = authorized;
        for (field, authorized, requested) in [
            (
                "candidate_limit",
                authorized.candidate_limit,
                limits.candidate_limit,
            ),
            (
                "candidate_total_bytes",
                authorized.candidate_total_bytes,
                limits.candidate_total_bytes,
            ),
            (
                "candidate_item_bytes",
                authorized.candidate_item_bytes,
                limits.candidate_item_bytes,
            ),
            (
                "candidate_key_bytes",
                authorized.candidate_key_bytes,
                limits.candidate_key_bytes,
            ),
            (
                "candidate_stable_id_bytes",
                authorized.candidate_stable_id_bytes,
                limits.candidate_stable_id_bytes,
            ),
            (
                "candidate_anchor_id_bytes",
                authorized.candidate_anchor_id_bytes,
                limits.candidate_anchor_id_bytes,
            ),
            (
                "candidate_metadata_field_bytes",
                authorized.candidate_metadata_field_bytes,
                limits.candidate_metadata_field_bytes,
            ),
            ("record_limit", authorized.record_limit, limits.record_limit),
            (
                "record_total_bytes",
                authorized.record_total_bytes,
                limits.record_total_bytes,
            ),
            (
                "record_item_bytes",
                authorized.record_item_bytes,
                limits.record_item_bytes,
            ),
            (
                "record_key_bytes",
                authorized.record_key_bytes,
                limits.record_key_bytes,
            ),
            (
                "hydration_limit",
                authorized.hydration_limit,
                limits.hydration_limit,
            ),
            (
                "hydration_total_bytes",
                authorized.hydration_total_bytes,
                limits.hydration_total_bytes,
            ),
            (
                "hydration_payload_bytes",
                authorized.hydration_payload_bytes,
                limits.hydration_payload_bytes,
            ),
            (
                "hydration_chunk_bytes",
                authorized.hydration_chunk_bytes,
                limits.hydration_chunk_bytes,
            ),
        ] {
            if requested > authorized {
                return Err(ExecutionLimitTighteningError::WouldLoosen {
                    field,
                    authorized,
                    requested,
                });
            }
        }
        self.request = self.request.with_limits(limits.validate()?);
        Ok(self)
    }

    pub const fn authorization(&self) -> ValidatedAuthorization {
        self.authorization
    }

    pub fn with_participant_manifest(
        mut self,
        participants: TemporalParticipantManifest,
    ) -> Result<Self, TemporalPortError> {
        if matches!(
            self.request.retrieval_scope(),
            TemporalRetrievalScope::Session(session_id)
                if participants.entries().iter().any(|entry| entry.session_id() != session_id)
        ) {
            return Err(TemporalPortError::UnauthorizedSnapshot);
        }
        self.participants = participants;
        self.participant_manifest_authoritative = true;
        Ok(self)
    }

    pub fn participant_manifest(&self) -> &TemporalParticipantManifest {
        &self.participants
    }

    pub fn source_coverage(&self) -> Result<SessionSourceCoverageReceiptV1, SessionContractError> {
        self.participants.source_coverage(self.temporal_mode())
    }

    pub const fn has_authoritative_participant_manifest(&self) -> bool {
        self.participant_manifest_authoritative
    }

    pub fn root_digest(&self) -> &BindingDigest {
        self.request.root_digest()
    }

    pub fn request_digest(&self) -> &BindingDigest {
        self.request.request_digest()
    }

    pub fn filter_digest(&self) -> &BindingDigest {
        self.request.filter_digest()
    }

    pub fn provider_scope(&self) -> Option<&str> {
        self.request.provider_scope()
    }

    pub fn retrieval_scope(&self) -> &TemporalRetrievalScope {
        self.request.retrieval_scope()
    }

    pub fn access_digest(&self) -> &BindingDigest {
        self.request.access_digest()
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.request.temporal_mode()
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.request.grain()
    }

    pub const fn watermarks(&self) -> TemporalWatermarks {
        self.watermarks
    }

    pub fn versions(&self) -> &KernelVersions {
        &self.versions
    }

    pub fn cursor_key(&self) -> Option<&SignedCursorKeyRefV1> {
        self.cursor_key.as_ref()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemporalRecordBatch {
    pub occurrences: Vec<ResolutionOccurrence>,
    pub copies: Vec<LogicalCopyRecordV1>,
    pub assertions: Vec<ResolutionAssertion>,
    pub summaries: Vec<SessionSummaryRecordV1>,
    pub summary_sources: BTreeMap<tracedecay_domain::RetrievalAnchorId, SummarySourceState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummarySourceRecord {
    pub anchor_id: tracedecay_domain::RetrievalAnchorId,
    pub state: SummarySourceState,
}

pub enum TemporalRecord {
    Occurrence(ResolutionOccurrence),
    Copy(LogicalCopyRecordV1),
    Assertion(ResolutionAssertion),
    Summary(SessionSummaryRecordV1),
    SummarySource(SummarySourceRecord),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageLimits {
    max_items: usize,
    max_total_bytes: usize,
    max_item_bytes: usize,
    max_page_items: usize,
}

impl PageLimits {
    pub fn new(
        max_items: usize,
        max_total_bytes: usize,
        max_item_bytes: usize,
        max_page_items: usize,
    ) -> Result<Self, TemporalPortError> {
        if max_items == 0 || max_items > MAX_READ_ITEMS {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "item count",
            });
        }
        if max_total_bytes == 0 || max_total_bytes > MAX_READ_TOTAL_BYTES {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "total bytes",
            });
        }
        if max_item_bytes == 0 || max_item_bytes > MAX_READ_ITEM_BYTES {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "item bytes",
            });
        }
        if max_page_items == 0 || max_page_items > max_items || max_page_items > MAX_PAGE_ITEMS_CAP
        {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "page item count",
            });
        }
        Ok(Self {
            max_items,
            max_total_bytes,
            max_item_bytes,
            max_page_items,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateFieldCaps {
    stable_id_bytes: usize,
    anchor_id_bytes: usize,
    metadata_field_bytes: usize,
}

impl CandidateFieldCaps {
    pub const fn stable_id_bytes(self) -> usize {
        self.stable_id_bytes
    }

    pub const fn metadata_field_bytes(self) -> usize {
        self.metadata_field_bytes
    }

    pub const fn anchor_id_bytes(self) -> usize {
        self.anchor_id_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageKey(String);

impl PageKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    page_index: usize,
    keyset: Option<PageKey>,
    remaining_items: usize,
    remaining_total_bytes: usize,
    max_item_bytes: usize,
    page_item_limit: usize,
    page_total_byte_limit: usize,
    max_key_bytes: usize,
    candidate_field_caps: Option<CandidateFieldCaps>,
}

impl PageRequest {
    #[cfg(any(test, feature = "test-helpers"))]
    pub const fn for_test(
        remaining_items: usize,
        remaining_total_bytes: usize,
        max_item_bytes: usize,
        page_item_limit: usize,
        max_key_bytes: usize,
    ) -> Self {
        Self {
            page_index: 0,
            keyset: None,
            remaining_items,
            remaining_total_bytes,
            max_item_bytes,
            page_item_limit,
            page_total_byte_limit: remaining_total_bytes,
            max_key_bytes,
            candidate_field_caps: None,
        }
    }

    pub const fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn keyset(&self) -> Option<&PageKey> {
        self.keyset.as_ref()
    }

    pub const fn remaining_items(&self) -> usize {
        self.remaining_items
    }

    pub const fn remaining_total_bytes(&self) -> usize {
        self.remaining_total_bytes
    }

    pub const fn max_item_bytes(&self) -> usize {
        self.max_item_bytes
    }

    pub const fn page_item_limit(&self) -> usize {
        self.page_item_limit
    }

    pub const fn page_total_byte_limit(&self) -> usize {
        self.page_total_byte_limit
    }

    pub const fn max_key_bytes(&self) -> usize {
        self.max_key_bytes
    }

    pub const fn candidate_field_caps(&self) -> Option<CandidateFieldCaps> {
        self.candidate_field_caps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageStatus {
    More,
    Complete,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BoundedPage<T> {
    items: Vec<T>,
    encoded_bytes: usize,
    status: PageStatus,
    continuation: Option<PageKey>,
}

impl<T> BoundedPage<T> {
    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub const fn status(&self) -> PageStatus {
        self.status
    }

    pub fn continuation(&self) -> Option<&PageKey> {
        self.continuation.as_ref()
    }
}

pub struct ReadState<T> {
    limits: PageLimits,
    consumed_items: usize,
    consumed_bytes: usize,
    page_index: usize,
    keyset: Option<PageKey>,
    marker: PhantomData<fn() -> T>,
}

impl<T> ReadState<T> {
    pub const fn new(limits: PageLimits) -> Self {
        Self {
            limits,
            consumed_items: 0,
            consumed_bytes: 0,
            page_index: 0,
            keyset: None,
            marker: PhantomData,
        }
    }

    pub const fn consumed_items(&self) -> usize {
        self.consumed_items
    }

    pub const fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    fn require_within_limits(
        &self,
        max_items: usize,
        max_total_bytes: usize,
        max_item_bytes: usize,
        resources: ReadBudgetResources,
    ) -> Result<(), TemporalPortError> {
        if self.limits.max_items > max_items {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.item_count,
            });
        }
        if self.limits.max_total_bytes > max_total_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.total_bytes,
            });
        }
        if self.limits.max_item_bytes > max_item_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.item_bytes,
            });
        }
        Ok(())
    }

    fn request(
        &self,
        max_key_bytes: usize,
        candidate_field_caps: Option<CandidateFieldCaps>,
    ) -> PageRequest {
        let remaining_items = self.limits.max_items - self.consumed_items;
        let page_item_limit = remaining_items.min(self.limits.max_page_items);
        let remaining_total_bytes = self.limits.max_total_bytes - self.consumed_bytes;
        PageRequest {
            page_index: self.page_index,
            keyset: self.keyset.clone(),
            remaining_items,
            remaining_total_bytes,
            max_item_bytes: self.limits.max_item_bytes,
            page_item_limit,
            page_total_byte_limit: remaining_total_bytes
                .min(self.limits.max_item_bytes.saturating_mul(page_item_limit)),
            max_key_bytes,
            candidate_field_caps,
        }
    }

    fn is_exhausted(&self) -> bool {
        self.consumed_items == self.limits.max_items
            || self.consumed_bytes == self.limits.max_total_bytes
    }

    fn begin_page<'a>(
        &'a mut self,
        control: &'a ExecutionControl,
        max_key_bytes: usize,
        candidate_field_caps: Option<CandidateFieldCaps>,
        budget_resources: ReadBudgetResources,
    ) -> BoundedPageSink<'a, T> {
        BoundedPageSink {
            max_items: self.limits.max_items,
            max_total_bytes: self.limits.max_total_bytes,
            max_item_bytes: self.limits.max_item_bytes,
            max_page_items: self.limits.max_page_items,
            consumed_items: &mut self.consumed_items,
            consumed_bytes: &mut self.consumed_bytes,
            control,
            max_key_bytes,
            candidate_field_caps,
            budget_resources,
            items: Vec::with_capacity(self.limits.max_page_items.min(MAX_BOUNDED_PAGE_PREALLOC)),
            encoded_bytes: 0,
            continuation: None,
        }
    }

    fn advanced_page(&mut self, continuation: Option<PageKey>) {
        self.page_index += 1;
        self.keyset = continuation;
    }

    fn incomplete_coverage_error(&self, resources: ReadBudgetResources) -> TemporalPortError {
        if self.consumed_items == self.limits.max_items {
            TemporalPortError::BudgetExceeded {
                resource: resources.item_count,
            }
        } else {
            TemporalPortError::BudgetExceeded {
                resource: resources.total_bytes,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadBudgetResources {
    item_count: &'static str,
    item_bytes: &'static str,
    total_bytes: &'static str,
}

const CANDIDATE_READ_BUDGET: ReadBudgetResources = ReadBudgetResources {
    item_count: "candidate item count",
    item_bytes: "candidate item bytes",
    total_bytes: "candidate total bytes",
};

const RECORD_READ_BUDGET: ReadBudgetResources = ReadBudgetResources {
    item_count: "record item count",
    item_bytes: "record item bytes",
    total_bytes: "record total bytes",
};

pub type CandidateReadState = ReadState<RankingCandidate>;
pub type TemporalRecordReadState = ReadState<TemporalRecord>;

pub struct BoundedPageSink<'a, T> {
    max_items: usize,
    max_total_bytes: usize,
    max_item_bytes: usize,
    max_page_items: usize,
    consumed_items: &'a mut usize,
    consumed_bytes: &'a mut usize,
    control: &'a ExecutionControl,
    max_key_bytes: usize,
    candidate_field_caps: Option<CandidateFieldCaps>,
    budget_resources: ReadBudgetResources,
    items: Vec<T>,
    encoded_bytes: usize,
    continuation: Option<PageKey>,
}

// Measurement stays sealed so producers cannot substitute underreported byte counts.
impl<T: MeasuredTemporalValue> BoundedPageSink<'_, T> {
    pub fn push(&mut self, value: T) -> Result<(), TemporalPortError> {
        self.control.checkpoint()?;
        if self.items.len() == self.max_page_items || *self.consumed_items == self.max_items {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.item_count,
            });
        }
        value.validate_candidate_fields(self.candidate_field_caps)?;
        let encoded_bytes = value.measured_encoded_bytes()?;
        if encoded_bytes > self.max_item_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.item_bytes,
            });
        }
        let total_bytes = self.consumed_bytes.checked_add(encoded_bytes).ok_or(
            TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.total_bytes,
            },
        )?;
        if total_bytes > self.max_total_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.total_bytes,
            });
        }
        *self.consumed_items += 1;
        *self.consumed_bytes = total_bytes;
        self.encoded_bytes += encoded_bytes;
        self.items.push(value);
        self.control.checkpoint()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
    fn preallocated_capacity(&self) -> usize {
        self.items.capacity()
    }

    pub fn set_continuation_key(&mut self, key: PageKey) -> Result<(), TemporalPortError> {
        let key_cap = self.max_key_bytes.min(MAX_CONTINUATION_KEY_BYTES);
        if key.0.len() > key_cap {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key bytes",
            });
        }
        self.continuation = Some(key);
        Ok(())
    }

    fn finish(self, status: PageStatus) -> Result<BoundedPage<T>, TemporalPortError> {
        if status == PageStatus::More && self.items.is_empty() {
            return Err(TemporalPortError::Read {
                operation: "produce bounded page",
                message: "producer returned an empty continuation page".to_string(),
            });
        }
        if status == PageStatus::More && self.continuation.is_none() {
            return Err(TemporalPortError::Read {
                operation: "produce bounded page",
                message: "producer omitted the continuation key".to_string(),
            });
        }
        Ok(BoundedPage {
            items: self.items,
            encoded_bytes: self.encoded_bytes,
            status,
            continuation: self.continuation,
        })
    }
}

pub type CandidatePageSink<'a> = BoundedPageSink<'a, RankingCandidate>;
pub type TemporalRecordPageSink<'a> = BoundedPageSink<'a, TemporalRecord>;
pub type PortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TemporalPortError>> + Send + 'a>>;

pub trait TemporalReadPort: Send + Sync {
    fn produce_candidate_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus>;

    fn produce_candidate_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        match scope {
            TemporalRetrievalScope::Session(_) => {
                self.produce_candidate_page(snapshot, plan, request, sink)
            }
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "produce candidate page for scope",
                    message:
                        "root-wide retrieval requires an explicit scope-aware port implementation"
                            .to_string(),
                })
            }),
        }
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus>;

    fn produce_temporal_record_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        match scope {
            TemporalRetrievalScope::Session(_) => {
                self.produce_temporal_record_page(snapshot, candidates, request, sink)
            }
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "produce temporal record page for scope",
                    message:
                        "root-wide retrieval requires an explicit scope-aware port implementation"
                            .to_string(),
                })
            }),
        }
    }
}

pub async fn pull_candidate_page(
    port: &impl TemporalReadPort,
    snapshot: &TemporalExecutionSnapshot,
    plan: &CandidatePlan,
    state: &mut CandidateReadState,
) -> Result<BoundedPage<RankingCandidate>, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let limits = snapshot.request().limits().validate()?;
    state.require_within_limits(
        limits.candidate_limit,
        limits.candidate_total_bytes,
        limits.candidate_item_bytes,
        CANDIDATE_READ_BUDGET,
    )?;
    if state.is_exhausted() {
        // Caps exhausted with unread producer work must not synthesize Complete.
        return Err(state.incomplete_coverage_error(CANDIDATE_READ_BUDGET));
    }
    let control = snapshot.request().execution_control();
    let field_caps = CandidateFieldCaps {
        stable_id_bytes: limits.candidate_stable_id_bytes,
        anchor_id_bytes: limits.candidate_anchor_id_bytes,
        metadata_field_bytes: limits.candidate_metadata_field_bytes,
    };
    let request = state.request(limits.candidate_key_bytes, Some(field_caps));
    let mut sink = state.begin_page(
        control,
        limits.candidate_key_bytes,
        Some(field_caps),
        CANDIDATE_READ_BUDGET,
    );
    let status = await_controlled(
        control,
        port.produce_candidate_page_for_scope(
            snapshot.request().retrieval_scope(),
            snapshot,
            plan,
            request,
            &mut sink,
        ),
    )
    .await?;
    let page = sink.finish(status)?;
    commit_pulled_page(state, page, CANDIDATE_READ_BUDGET)
}

pub async fn pull_temporal_record_page(
    port: &impl TemporalReadPort,
    snapshot: &TemporalExecutionSnapshot,
    candidates: &[RankingCandidate],
    state: &mut TemporalRecordReadState,
) -> Result<BoundedPage<TemporalRecord>, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let limits = snapshot.request().limits().validate()?;
    state.require_within_limits(
        limits.record_limit,
        limits.record_total_bytes,
        limits.record_item_bytes,
        RECORD_READ_BUDGET,
    )?;
    if state.is_exhausted() {
        // Caps exhausted with unread producer work must not synthesize Complete.
        return Err(state.incomplete_coverage_error(RECORD_READ_BUDGET));
    }
    let control = snapshot.request().execution_control();
    let request = state.request(limits.record_key_bytes, None);
    let mut sink = state.begin_page(control, limits.record_key_bytes, None, RECORD_READ_BUDGET);
    let status = await_controlled(
        control,
        port.produce_temporal_record_page_for_scope(
            snapshot.request().retrieval_scope(),
            snapshot,
            candidates,
            request,
            &mut sink,
        ),
    )
    .await?;
    let page = sink.finish(status)?;
    commit_pulled_page(state, page, RECORD_READ_BUDGET)
}

fn commit_pulled_page<T>(
    state: &mut ReadState<T>,
    page: BoundedPage<T>,
    resources: ReadBudgetResources,
) -> Result<BoundedPage<T>, TemporalPortError> {
    if page.status() == PageStatus::More && state.is_exhausted() {
        // Producer still has pages, but item/total caps already consumed the
        // read budget. Propagate incomplete coverage — never downgrade to Complete.
        return Err(state.incomplete_coverage_error(resources));
    }
    state.advanced_page(page.continuation.clone());
    Ok(page)
}

pub trait MeasuredTemporalValue {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError>;

    fn validate_candidate_fields(
        &self,
        _caps: Option<CandidateFieldCaps>,
    ) -> Result<(), TemporalPortError> {
        Ok(())
    }
}

#[derive(Serialize)]
struct CandidateWire<'a> {
    stable_id: &'a str,
    anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    retriever_record_id: &'a str,
    channel: &'static str,
    raw_score: i64,
    knowledge_at_micros: i64,
    logical_message: &'a Option<String>,
    turn: &'a Option<String>,
    session: &'a Option<String>,
    source: &'a Option<String>,
    evidence_role: &'a Option<String>,
    exact_ranges: &'a [tracedecay_domain::ByteRangeV1],
}

impl MeasuredTemporalValue for RankingCandidate {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError> {
        let channel = match self.channel {
            super::candidates::CandidateChannel::Scope => "scope",
            super::candidates::CandidateChannel::Anchor => "anchor",
            super::candidates::CandidateChannel::ExactMessage => "exact_message",
            super::candidates::CandidateChannel::Phrase => "phrase",
            super::candidates::CandidateChannel::Entity => "entity",
            super::candidates::CandidateChannel::Time => "time",
            super::candidates::CandidateChannel::Lexical => "lexical",
            super::candidates::CandidateChannel::Summary => "summary",
            super::candidates::CandidateChannel::Span => "span",
            super::candidates::CandidateChannel::Burst => "burst",
        };
        measured_json_bytes(
            "encode candidate",
            &CandidateWire {
                stable_id: &self.stable_id,
                anchor_id: &self.anchor_id,
                retriever_record_id: &self.retriever_record_id,
                channel,
                raw_score: self.raw_score,
                knowledge_at_micros: self.knowledge_at_micros,
                logical_message: &self.logical_message,
                turn: &self.turn,
                session: &self.session,
                source: &self.source,
                evidence_role: &self.evidence_role,
                exact_ranges: &self.exact_ranges,
            },
        )
    }

    fn validate_candidate_fields(
        &self,
        caps: Option<CandidateFieldCaps>,
    ) -> Result<(), TemporalPortError> {
        let Some(caps) = caps else {
            return Ok(());
        };
        if self.stable_id.len() > caps.stable_id_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate stable id bytes",
            });
        }
        if self.anchor_id.to_string().len() > caps.anchor_id_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate anchor id bytes",
            });
        }
        if self.retriever_record_id.len() > caps.metadata_field_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate retriever record id bytes",
            });
        }
        for field in [
            &self.logical_message,
            &self.turn,
            &self.session,
            &self.source,
            &self.evidence_role,
        ] {
            if field
                .as_ref()
                .is_some_and(|value| value.len() > caps.metadata_field_bytes)
            {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate metadata field bytes",
                });
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct EvidenceWire<'a> {
    authority: tracedecay_domain::SessionAuthorityClassV1,
    authorized: bool,
    supporting_anchor_ids: &'a std::collections::BTreeSet<tracedecay_domain::RetrievalAnchorId>,
}

#[derive(Serialize)]
struct OccurrenceWire<'a> {
    kind: &'static str,
    occurrence_id: &'a tracedecay_domain::MessageOccurrenceIdV1,
    anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    knowledge_at: tracedecay_domain::UtcMicros,
    valid_time: tracedecay_domain::TemporalValidityV1,
    evidence: EvidenceWire<'a>,
}

#[derive(Serialize)]
struct AssertionWire<'a> {
    kind: &'static str,
    assertion_kind: tracedecay_domain::TemporalAssertionKindV1,
    subject_anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    object_anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    knowledge_at: tracedecay_domain::UtcMicros,
    valid_time: tracedecay_domain::TemporalValidityV1,
    evidence: EvidenceWire<'a>,
}

impl MeasuredTemporalValue for TemporalRecord {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError> {
        match self {
            Self::Occurrence(value) => measured_json_bytes(
                "encode occurrence",
                &OccurrenceWire {
                    kind: "occurrence",
                    occurrence_id: &value.occurrence_id,
                    anchor_id: &value.anchor_id,
                    knowledge_at: value.knowledge_at,
                    valid_time: value.valid_time,
                    evidence: EvidenceWire {
                        authority: value.evidence.authority,
                        authorized: value.evidence.is_authorized(),
                        supporting_anchor_ids: &value.evidence.supporting_anchor_ids,
                    },
                },
            ),
            Self::Copy(value) => measured_json_bytes("encode copy", &("copy", value)),
            Self::Assertion(value) => measured_json_bytes(
                "encode assertion",
                &AssertionWire {
                    kind: "assertion",
                    assertion_kind: value.kind,
                    subject_anchor_id: &value.subject_anchor_id,
                    object_anchor_id: &value.object_anchor_id,
                    knowledge_at: value.knowledge_at,
                    valid_time: value.valid_time,
                    evidence: EvidenceWire {
                        authority: value.evidence.authority,
                        authorized: value.evidence.is_authorized(),
                        supporting_anchor_ids: &value.evidence.supporting_anchor_ids,
                    },
                },
            ),
            Self::Summary(value) => measured_json_bytes("encode summary", &("summary", value)),
            Self::SummarySource(value) => measured_json_bytes(
                "encode summary source",
                &("summary_source", value.anchor_id.clone(), value.state),
            ),
        }
    }
}

struct BoundedByteCounter {
    count: usize,
    stop_after: usize,
}

impl Write for BoundedByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.count = self.count.saturating_add(buf.len());
        if self.count > self.stop_after {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded item exceeds absolute measurement ceiling",
            ));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn measured_json_bytes(
    operation: &'static str,
    value: &impl Serialize,
) -> Result<usize, TemporalPortError> {
    let mut counter = BoundedByteCounter {
        count: 0,
        stop_after: MAX_READ_ITEM_BYTES,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(counter.count),
        Err(_) if counter.count > MAX_READ_ITEM_BYTES => Err(TemporalPortError::BudgetExceeded {
            resource: "encoded item bytes",
        }),
        Err(error) => Err(TemporalPortError::Read {
            operation,
            message: error.to_string(),
        }),
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CursorKeyError {
    #[error("cursor authentication key is unavailable")]
    Unavailable,
    #[error("cursor authentication key material is invalid")]
    InvalidMaterial,
    #[error("cursor authentication failed")]
    AuthenticationFailed,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CursorSignature([u8; 32]);

impl CursorSignature {
    pub(crate) fn from_hex(encoded: &str) -> Result<Self, CursorKeyError> {
        let decoded = hex::decode(encoded).map_err(|_| CursorKeyError::AuthenticationFailed)?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| CursorKeyError::AuthenticationFailed)?;
        Ok(Self(bytes))
    }

    pub(crate) fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

pub trait SessionCursorAuthenticator: Send + Sync {
    fn sign(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
    ) -> Result<CursorSignature, CursorKeyError>;

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError>;
}

pub struct InMemoryCursorAuthenticator {
    key: SignedCursorKeyRefV1,
    secret: Zeroizing<Vec<u8>>,
}

impl InMemoryCursorAuthenticator {
    pub fn new(
        key: SignedCursorKeyRefV1,
        secret: impl Into<Vec<u8>>,
    ) -> Result<Self, CursorKeyError> {
        let secret = Zeroizing::new(secret.into());
        if secret.len() < 32 || secret.len() > MAX_CURSOR_SECRET_BYTES {
            return Err(CursorKeyError::InvalidMaterial);
        }
        Ok(Self { key, secret })
    }

    fn mac(&self) -> Result<Hmac<Sha256>, CursorKeyError> {
        <Hmac<Sha256> as KeyInit>::new_from_slice(&self.secret)
            .map_err(|_| CursorKeyError::InvalidMaterial)
    }
}

impl fmt::Debug for InMemoryCursorAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryCursorAuthenticator")
            .field("key", &self.key)
            .field("secret", &"REDACTED")
            .finish()
    }
}

impl SessionCursorAuthenticator for InMemoryCursorAuthenticator {
    fn sign(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
    ) -> Result<CursorSignature, CursorKeyError> {
        if key != &self.key {
            return Err(CursorKeyError::Unavailable);
        }
        let mut mac = self.mac()?;
        mac.update(authenticated);
        Ok(CursorSignature(mac.finalize().into_bytes().into()))
    }

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError> {
        if key != &self.key {
            return Err(CursorKeyError::Unavailable);
        }
        let mut mac = self.mac()?;
        mac.update(authenticated);
        mac.verify_slice(&signature.0)
            .map_err(|_| CursorKeyError::AuthenticationFailed)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    use tracedecay_domain::{
        RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSourceCoverageStateV1,
        TemporalModeV1,
    };

    use super::*;
    use crate::temporal::candidates::{CandidateChannel, CandidatePlan};
    use crate::temporal::test_support::block_on;

    fn session_id() -> SessionId {
        serde_json::from_str("\"session-1\"").expect("valid session id")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn participant(session: &str, source: &str, generation: u64) -> TemporalParticipantGeneration {
        TemporalParticipantGeneration::new(
            SessionId::new(session).expect("session"),
            source,
            TemporalWatermarks {
                generation,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            6,
            &BindingDigest::new("configuration", digest('7')).expect("configuration"),
            &BindingDigest::new("authorization", digest('8')).expect("authorization"),
            TemporalParticipantAuthorization::Authorized,
            TemporalSourceAccess::Available,
        )
        .expect("participant")
    }

    #[test]
    fn execution_control_deadlines_have_no_scheduler_state() {
        let source = fs::read_to_string("crates/tracedecay-query/src/temporal/ports.rs")
            .expect("read the temporal ports source");
        let (production_source, _) = source
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("ports module has an inline test boundary");
        for forbidden in [
            "use std::thread;",
            "std::thread::",
            "thread::spawn",
            "thread::sleep",
            "Waker",
            "wake_waiters",
            "fn register(",
            "tokio",
            "rusqlite",
            "sqlx",
            "diesel",
            "sqlite",
            "SELECT ",
            "async_std",
            "smol::",
        ] {
            assert!(
                !production_source.contains(forbidden),
                "runtime-bound deadline implementation contains forbidden `{forbidden}`"
            );
        }

        let deadline = Instant::now() + Duration::from_mins(1);
        let controls: Vec<_> = (0..64)
            .map(|_| ExecutionControl::new(Some(deadline)))
            .collect();
        assert_eq!(controls.len(), 64);
        for control in &controls {
            let ExecutionControl {
                cancellation,
                deadline: stored_deadline,
                remaining_work,
            } = control;
            assert_eq!(*stored_deadline, Some(deadline));
            assert_eq!(Arc::strong_count(cancellation), 1);
            assert!(remaining_work.is_none());
        }
        drop(controls);
    }

    #[test]
    fn expired_deadline_fails_at_checkpoint() {
        let control = ExecutionControl::new(Some(Instant::now()));

        assert_eq!(
            control.checkpoint(),
            Err(TemporalPortError::DeadlineExceeded)
        );
    }

    #[test]
    fn snapshot_request_requires_canonical_bindings() {
        let error = TemporalSnapshotRequest::new(
            session_id(),
            "",
            digest('a'),
            digest('b'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect_err("empty root digest must fail closed");

        assert_eq!(
            error,
            TemporalPortError::InvalidBinding {
                field: "root_digest"
            }
        );
    }

    #[test]
    fn snapshot_request_freezes_optional_exact_provider_scope() {
        let all_providers = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid all-provider request");
        assert_eq!(all_providers.provider_scope(), None);

        let scoped = all_providers
            .with_provider_scope(Some("claude".to_string()))
            .expect("canonical provider");
        assert_eq!(scoped.provider_scope(), Some("claude"));
    }

    #[test]
    fn snapshot_request_freezes_validated_semantic_filter_before_reads() {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request");
        let filter = TemporalCandidateFilterV1 {
            git_branch: Some("feature/filters".to_string()),
            workflow_run: Some("wf_filters".to_string()),
            roles: vec!["assistant".to_string(), "user".to_string()],
            goals: true,
            ..TemporalCandidateFilterV1::default()
        };

        let request = request
            .with_semantic_filter(filter.clone())
            .expect("canonical semantic filter");

        assert_eq!(request.semantic_filter(), &filter);
    }

    #[test]
    fn semantic_filter_rejects_ambiguous_or_unstable_bindings() {
        let unsorted = TemporalCandidateFilterV1 {
            roles: vec!["user".to_string(), "assistant".to_string()],
            ..TemporalCandidateFilterV1::default()
        };
        assert_eq!(
            unsorted.validate(),
            Err(TemporalPortError::InvalidBinding { field: "roles" })
        );
        let orphan_agent = TemporalCandidateFilterV1 {
            workflow_agent: Some("worker".to_string()),
            ..TemporalCandidateFilterV1::default()
        };
        assert_eq!(
            orphan_agent.validate(),
            Err(TemporalPortError::InvalidBinding {
                field: "workflow_agent"
            })
        );
    }

    #[test]
    fn snapshot_request_freezes_typed_retrieval_scope_additively() {
        let session = session_id();
        let authorized_root =
            TemporalAuthorizedRoot::project("profile-1", "project-1", "store-1", "root-1")
                .expect("typed root");
        let session_request = TemporalSnapshotRequest::new(
            session.clone(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid session request");
        assert_eq!(
            session_request.retrieval_scope(),
            &TemporalRetrievalScope::Session(session)
        );

        let root_request = session_request
            .with_authorized_root(authorized_root.clone())
            .expect("authorized root")
            .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
        assert_eq!(
            root_request.retrieval_scope(),
            &TemporalRetrievalScope::AllSessionsInAuthorizedRoot
        );
        assert_eq!(root_request.retrieval_scope().session_id(), None);
        assert_eq!(root_request.authorized_root(), Some(&authorized_root));
        assert_eq!(
            root_request
                .authorized_root()
                .expect("root authority")
                .project_key(),
            "project-1"
        );
    }

    #[test]
    fn participant_manifest_is_sorted_unique_bounded_and_epoch_bound() {
        let manifest = TemporalParticipantManifest::new(vec![
            participant("session-2", "source-b", 2),
            participant("session-1", "source-a", 1),
        ])
        .expect("manifest");
        assert_eq!(
            manifest
                .entries()
                .iter()
                .map(|entry| (entry.session_id().as_str(), entry.source_id()))
                .collect::<Vec<_>>(),
            [("session-1", "source-a"), ("session-2", "source-b")]
        );

        let changed = TemporalParticipantManifest::new(vec![
            participant("session-1", "source-a", 1),
            participant("session-2", "source-b", 3),
        ])
        .expect("changed manifest");
        assert_ne!(manifest.epoch_digest(), changed.epoch_digest());

        assert_eq!(
            TemporalParticipantManifest::new(vec![
                participant("session-1", "source-a", 1),
                participant("session-1", "source-a", 1),
            ]),
            Err(TemporalPortError::DuplicateParticipant)
        );

        let accepted = (0..MAX_TEMPORAL_PARTICIPANTS)
            .map(|index| participant("session-1", &format!("s{index:03}"), 1))
            .collect();
        assert!(TemporalParticipantManifest::new(accepted).is_ok());
        let rejected = (0..=MAX_TEMPORAL_PARTICIPANTS)
            .map(|index| participant("session-1", &format!("s{index:03}"), 1))
            .collect();
        assert!(matches!(
            TemporalParticipantManifest::new(rejected),
            Err(TemporalPortError::ParticipantLimitExceeded {
                observed,
                maximum: MAX_TEMPORAL_PARTICIPANTS,
            }) if observed == MAX_TEMPORAL_PARTICIPANTS + 1
        ));
    }

    fn participant_entries_with_canonical_bytes(
        target_bytes: usize,
    ) -> Vec<TemporalParticipantGeneration> {
        let mut entries = (0..128)
            .map(|index| participant("session-1", &format!("s{index:03}"), 1))
            .collect::<Vec<_>>();
        let base_bytes = serde_json::to_vec(&entries).unwrap().len();
        assert!(base_bytes <= target_bytes);
        let mut remaining = target_bytes - base_bytes;
        for entry in &mut entries {
            let available = 512_usize.saturating_sub(entry.source_id.len());
            let add = available.min(remaining);
            entry.source_id.push_str(&"x".repeat(add));
            remaining -= add;
            if remaining == 0 {
                break;
            }
        }
        assert_eq!(remaining, 0, "test entries could not reach target size");
        assert_eq!(serde_json::to_vec(&entries).unwrap().len(), target_bytes);
        entries
    }

    #[test]
    fn participant_manifest_accepts_exact_canonical_byte_limit() {
        let entries =
            participant_entries_with_canonical_bytes(MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES);
        assert!(TemporalParticipantManifest::new(entries).is_ok());
    }

    #[test]
    fn participant_manifest_rejects_one_byte_over_canonical_limit() {
        let entries =
            participant_entries_with_canonical_bytes(MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES + 1);
        assert_eq!(
            TemporalParticipantManifest::new(entries),
            Err(TemporalPortError::ParticipantManifestBytesExceeded {
                observed: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES + 1,
                maximum: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES,
            })
        );
    }

    struct ScopeObservingPort {
        observed: Mutex<Vec<TemporalRetrievalScope>>,
    }

    impl TemporalReadPort for ScopeObservingPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "legacy candidate entry point",
                    message: "scope-aware kernel must not call the legacy entry point".to_string(),
                })
            })
        }

        fn produce_candidate_page_for_scope<'a>(
            &'a self,
            scope: &'a TemporalRetrievalScope,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                self.observed
                    .lock()
                    .expect("observed lock")
                    .push(scope.clone());
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "legacy record entry point",
                    message: "scope-aware kernel must not call the legacy entry point".to_string(),
                })
            })
        }

        fn produce_temporal_record_page_for_scope<'a>(
            &'a self,
            scope: &'a TemporalRetrievalScope,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                self.observed
                    .lock()
                    .expect("observed lock")
                    .push(scope.clone());
                Ok(PageStatus::Complete)
            })
        }
    }

    #[test]
    fn candidate_record_and_summary_provider_path_observes_frozen_root_scope() {
        block_on(async {
            let port = ScopeObservingPort {
                observed: Mutex::new(Vec::new()),
            };
            let request = TemporalSnapshotRequest::new(
                session_id(),
                digest('0'),
                digest('1'),
                digest('2'),
                TemporalModeV1::Current,
                RetrievalGrainV1::LogicalMessage,
            )
            .expect("valid request")
            .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
            let snapshot = TemporalExecutionSnapshot::new(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: 2,
                    projection: 3,
                    index: 4,
                    summary: 5,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                        .expect("valid digest"),
                },
                None,
            )
            .expect("valid snapshot");
            let mut candidate_state = CandidateReadState::new(
                PageLimits::new(1, 1024, 1024, 1).expect("candidate limits"),
            );
            let mut record_state = TemporalRecordReadState::new(
                PageLimits::new(1, 1024, 1024, 1).expect("record limits"),
            );

            pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect("candidate scope");
            pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
                .await
                .expect("record and summary scope");

            assert_eq!(
                *port.observed.lock().expect("observed lock"),
                [
                    TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
                    TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
                ]
            );
        });
    }

    #[test]
    fn snapshot_request_rejects_noncanonical_provider_scope() {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request");

        assert_eq!(
            request.with_provider_scope(Some(" claude".to_string())),
            Err(TemporalPortError::InvalidBinding {
                field: "provider_scope"
            })
        );
    }

    #[test]
    fn execution_snapshot_is_bound_to_one_root_and_frozen_versions() {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(42),
            },
            RetrievalGrainV1::Turn,
        )
        .expect("valid request");
        let snapshot = TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 7,
                source: 11,
                projection: 13,
                index: 17,
                summary: 19,
            },
            KernelVersions {
                schema: 3,
                ranking: 5,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("valid snapshot");

        assert_eq!(snapshot.root_digest().as_str(), digest('0'));
        assert_eq!(snapshot.watermarks().generation, 7);
        assert_eq!(snapshot.versions().ranking, 5);
        assert_eq!(snapshot.authorization(), ValidatedAuthorization::Authorized);
    }

    #[test]
    fn execution_snapshot_requires_explicit_validated_authorization() {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request");

        assert_eq!(
            TemporalExecutionSnapshot::new_authorized(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: 2,
                    projection: 3,
                    index: 4,
                    summary: 5,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new("configuration_digest", digest('3'),)
                        .expect("valid digest"),
                },
                None,
                ValidatedAuthorization::Unauthorized,
            ),
            Err(TemporalPortError::UnauthorizedSnapshot)
        );
    }

    #[test]
    fn cursor_key_provider_requires_at_least_256_bits_and_redacts_debug() {
        let key_ref = SignedCursorKeyRefV1 {
            key_id: tracedecay_domain::SessionCursorKeyIdV1::new("key-1").expect("valid key id"),
            version: tracedecay_domain::SessionCursorVersionV1::new(1).expect("valid key version"),
        };
        assert!(matches!(
            InMemoryCursorAuthenticator::new(key_ref.clone(), vec![7; 31]),
            Err(CursorKeyError::InvalidMaterial)
        ));
        assert!(matches!(
            InMemoryCursorAuthenticator::new(key_ref.clone(), vec![7; MAX_CURSOR_SECRET_BYTES + 1]),
            Err(CursorKeyError::InvalidMaterial)
        ));
        let provider =
            InMemoryCursorAuthenticator::new(key_ref, vec![7; 32]).expect("256-bit key is valid");
        let debug = format!("{provider:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("[7, 7"));
    }

    fn anchor(value: &str) -> RetrievalAnchorId {
        serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
    }

    fn candidate(stable_id: impl Into<String>) -> RankingCandidate {
        RankingCandidate {
            stable_id: stable_id.into(),
            anchor_id: anchor("anchor-1"),
            retriever_record_id: "record-1".to_string(),
            channel: CandidateChannel::Phrase,
            raw_score: 10,
            knowledge_at_micros: 7,
            logical_message: Some("logical-1".to_string()),
            turn: Some("turn-1".to_string()),
            session: Some("session-1".to_string()),
            source: Some("source-1".to_string()),
            evidence_role: Some("message".to_string()),
            exact_ranges: Vec::new(),
        }
    }

    fn snapshot_with_control(control: ExecutionControl) -> TemporalExecutionSnapshot {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request")
        .with_execution_control(control);
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("valid snapshot")
    }

    struct PagingPort {
        calls: AtomicUsize,
    }

    impl TemporalReadPort for PagingPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let all = ["candidate-0", "candidate-1", "candidate-2"];
                let start = request
                    .keyset()
                    .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
                for stable_id in all.iter().skip(start).take(request.page_item_limit()) {
                    sink.push(candidate(*stable_id))?;
                }
                Ok(if start + sink.len() < all.len() {
                    sink.set_continuation_key(PageKey::new((start + sink.len()).to_string()))?;
                    PageStatus::More
                } else {
                    PageStatus::Complete
                })
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    #[test]
    fn bounded_async_pull_streams_multiple_pages_without_preloaded_vecs() {
        block_on(async {
            let port = PagingPort {
                calls: AtomicUsize::new(0),
            };
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let limits = PageLimits::new(3, 16 * 1024, 4 * 1024, 1).expect("valid limits");
            let mut state = CandidateReadState::new(limits);
            let plan = CandidatePlan::default();
            let mut stable_ids = Vec::new();

            loop {
                let page = pull_candidate_page(&port, &snapshot, &plan, &mut state)
                    .await
                    .expect("bounded page");
                let status = page.status();
                stable_ids.extend(page.into_items().into_iter().map(|value| value.stable_id));
                if status == PageStatus::Complete {
                    break;
                }
            }

            assert_eq!(stable_ids, ["candidate-0", "candidate-1", "candidate-2"]);
            assert_eq!(port.calls.load(Ordering::SeqCst), 3);
        });
    }

    struct OversizedPort;

    impl TemporalReadPort for OversizedPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                sink.push(candidate("x".repeat(1024)))?;
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    #[test]
    fn producer_cannot_underreport_private_measured_item_size() {
        block_on(async {
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                CandidateReadState::new(PageLimits::new(1, 128, 128, 1).expect("valid limits"));

            assert_eq!(
                pull_candidate_page(
                    &OversizedPort,
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate item bytes"
                })
            );
        });
    }

    #[test]
    fn private_measurement_enforces_total_byte_limit() {
        block_on(async {
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                CandidateReadState::new(PageLimits::new(1, 128, 4096, 1).expect("valid limits"));

            assert_eq!(
                pull_candidate_page(
                    &OversizedPort,
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate total bytes"
                })
            );
        });
    }

    struct OverproducingPort;

    impl TemporalReadPort for OverproducingPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                sink.push(candidate("first"))?;
                sink.push(candidate("second"))?;
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    #[test]
    fn sink_rejects_producer_that_ignores_item_and_page_limits() {
        block_on(async {
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                CandidateReadState::new(PageLimits::new(1, 4096, 4096, 1).expect("valid limits"));

            assert_eq!(
                pull_candidate_page(
                    &OverproducingPort,
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate item count"
                })
            );
        });
    }

    struct CancellingPort {
        control: ExecutionControl,
        entered: Arc<AtomicBool>,
    }

    impl TemporalReadPort for CancellingPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            let control = self.control.clone();
            let entered = Arc::clone(&self.entered);
            Box::pin(async move {
                entered.store(true, Ordering::Release);
                control.cancel();
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    struct DeadlineCrossingPort {
        deadline: Instant,
        entered: Arc<AtomicBool>,
    }

    impl TemporalReadPort for DeadlineCrossingPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            let deadline = self.deadline;
            let entered = Arc::clone(&self.entered);
            Box::pin(async move {
                entered.store(true, Ordering::Release);
                while Instant::now() < deadline {
                    std::hint::spin_loop();
                }
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    #[test]
    fn async_pull_observes_live_cancellation_midstream() {
        block_on(async {
            let control = ExecutionControl::default();
            let snapshot = snapshot_with_control(control.clone());
            let mut state =
                CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("valid limits"));
            let entered = Arc::new(AtomicBool::new(false));
            let port = CancellingPort {
                control,
                entered: Arc::clone(&entered),
            };

            let result =
                pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state).await;

            assert!(entered.load(Ordering::Acquire));
            assert_eq!(result, Err(TemporalPortError::Cancelled));
        });
    }

    #[test]
    fn async_pull_observes_deadline_after_live_producer_work() {
        block_on(async {
            let deadline = Instant::now() + Duration::from_millis(100);
            let snapshot = snapshot_with_control(ExecutionControl::new(Some(deadline)));
            let mut state =
                CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("valid limits"));
            let entered = Arc::new(AtomicBool::new(false));
            let port = DeadlineCrossingPort {
                deadline,
                entered: Arc::clone(&entered),
            };
            let result =
                pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state).await;

            assert!(entered.load(Ordering::Acquire));
            assert_eq!(result, Err(TemporalPortError::DeadlineExceeded));
        });
    }

    fn summary_record(anchor_id: &str) -> TemporalRecord {
        TemporalRecord::SummarySource(SummarySourceRecord {
            anchor_id: anchor(anchor_id),
            state: SummarySourceState::Missing,
        })
    }

    /// Producer that always reports More after filling at most one item, with a
    /// stable continuation — used to prove caps cannot downgrade More → Complete.
    struct AlwaysMorePort {
        candidate_ids: Vec<&'static str>,
        record_anchors: Vec<&'static str>,
    }

    impl AlwaysMorePort {
        fn new(candidate_ids: Vec<&'static str>, record_anchors: Vec<&'static str>) -> Self {
            Self {
                candidate_ids,
                record_anchors,
            }
        }
    }

    impl TemporalReadPort for AlwaysMorePort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                let start = request
                    .keyset()
                    .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
                if let Some(stable_id) = self.candidate_ids.get(start) {
                    sink.push(candidate(*stable_id))?;
                }
                sink.set_continuation_key(PageKey::new((start + 1).to_string()))?;
                Ok(PageStatus::More)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            request: PageRequest,
            sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                let start = request
                    .keyset()
                    .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
                if let Some(anchor_id) = self.record_anchors.get(start) {
                    sink.push(summary_record(anchor_id))?;
                }
                sink.set_continuation_key(PageKey::new((start + 1).to_string()))?;
                Ok(PageStatus::More)
            })
        }
    }

    struct ExactCompletePort {
        candidates: Vec<&'static str>,
        records: Vec<&'static str>,
    }

    impl TemporalReadPort for ExactCompletePort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                let start = request
                    .keyset()
                    .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
                let end = (start + request.page_item_limit()).min(self.candidates.len());
                for stable_id in &self.candidates[start..end] {
                    sink.push(candidate(*stable_id))?;
                }
                Ok(if end < self.candidates.len() {
                    sink.set_continuation_key(PageKey::new(end.to_string()))?;
                    PageStatus::More
                } else {
                    PageStatus::Complete
                })
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            request: PageRequest,
            sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                let start = request
                    .keyset()
                    .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
                let end = (start + request.page_item_limit()).min(self.records.len());
                for anchor_id in &self.records[start..end] {
                    sink.push(summary_record(anchor_id))?;
                }
                Ok(if end < self.records.len() {
                    sink.set_continuation_key(PageKey::new(end.to_string()))?;
                    PageStatus::More
                } else {
                    PageStatus::Complete
                })
            })
        }
    }

    struct OversizedRecordPort;

    impl TemporalReadPort for OversizedRecordPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                // Inflate measured JSON size via a long anchor id.
                sink.push(summary_record(&"r".repeat(512)))?;
                Ok(PageStatus::Complete)
            })
        }
    }

    #[test]
    fn candidate_item_cap_with_producer_more_is_incomplete_coverage() {
        block_on(async {
            let port = AlwaysMorePort::new(vec!["c0", "c1"], Vec::new());
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state = CandidateReadState::new(
                PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );

            assert_eq!(
                pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state).await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate item count"
                })
            );
            assert_eq!(state.consumed_items(), 1);
        });
    }

    #[test]
    fn candidate_total_bytes_cap_with_producer_more_is_incomplete_coverage() {
        block_on(async {
            let first = candidate("c0");
            let encoded = first.measured_encoded_bytes().expect("measured");
            let port = AlwaysMorePort::new(vec!["c0", "c1"], Vec::new());
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                CandidateReadState::new(PageLimits::new(8, encoded, encoded, 1).expect("limits"));

            assert_eq!(
                pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state).await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate total bytes"
                })
            );
            assert_eq!(state.consumed_bytes(), encoded);
            assert!(state.consumed_items() < 8);
        });
    }

    #[test]
    fn candidate_item_bytes_cap_fails_closed_without_complete() {
        block_on(async {
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                CandidateReadState::new(PageLimits::new(2, 16 * 1024, 128, 2).expect("limits"));

            assert_eq!(
                pull_candidate_page(
                    &OversizedPort,
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate item bytes"
                })
            );
            assert_eq!(state.consumed_items(), 0);
        });
    }

    #[test]
    fn record_item_cap_with_producer_more_is_incomplete_coverage() {
        block_on(async {
            let port = AlwaysMorePort::new(Vec::new(), vec!["r0", "r1"]);
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state = TemporalRecordReadState::new(
                PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );

            match pull_temporal_record_page(&port, &snapshot, &[], &mut state).await {
                Err(error) => assert_eq!(
                    error,
                    TemporalPortError::BudgetExceeded {
                        resource: "record item count"
                    }
                ),
                Ok(_) => panic!("More + record item cap must be incomplete coverage"),
            }
            assert_eq!(state.consumed_items(), 1);
        });
    }

    #[test]
    fn record_total_bytes_cap_with_producer_more_is_incomplete_coverage() {
        block_on(async {
            let first = summary_record("r0");
            let encoded = first.measured_encoded_bytes().expect("measured");
            let port = AlwaysMorePort::new(Vec::new(), vec!["r0", "r1"]);
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state = TemporalRecordReadState::new(
                PageLimits::new(8, encoded, encoded, 1).expect("limits"),
            );

            match pull_temporal_record_page(&port, &snapshot, &[], &mut state).await {
                Err(error) => assert_eq!(
                    error,
                    TemporalPortError::BudgetExceeded {
                        resource: "record total bytes"
                    }
                ),
                Ok(_) => panic!("More + record total-byte cap must be incomplete coverage"),
            }
            assert_eq!(state.consumed_bytes(), encoded);
            assert!(state.consumed_items() < 8);
        });
    }

    #[test]
    fn record_item_bytes_cap_fails_closed_without_complete() {
        block_on(async {
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut state =
                TemporalRecordReadState::new(PageLimits::new(2, 16 * 1024, 64, 2).expect("limits"));

            match pull_temporal_record_page(&OversizedRecordPort, &snapshot, &[], &mut state).await
            {
                Err(error) => assert_eq!(
                    error,
                    TemporalPortError::BudgetExceeded {
                        resource: "record item bytes"
                    }
                ),
                Ok(_) => panic!("oversized record must fail closed"),
            }
            assert_eq!(state.consumed_items(), 0);
        });
    }

    #[test]
    fn producer_complete_at_exact_item_cap_remains_complete_for_candidates_and_records() {
        block_on(async {
            let port = ExactCompletePort {
                candidates: vec!["c0", "c1"],
                records: vec!["r0", "r1"],
            };
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut candidate_state = CandidateReadState::new(
                PageLimits::new(2, 16 * 1024, 4 * 1024, 2).expect("limits"),
            );
            let mut record_state = TemporalRecordReadState::new(
                PageLimits::new(2, 16 * 1024, 4 * 1024, 2).expect("limits"),
            );

            let candidates = pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect("exact candidate page");
            assert_eq!(candidates.status(), PageStatus::Complete);
            assert_eq!(candidates.continuation(), None);
            assert_eq!(candidates.items().len(), 2);

            let records = pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
                .await
                .expect("exact record page");
            assert_eq!(records.status(), PageStatus::Complete);
            assert_eq!(records.continuation(), None);
            assert_eq!(records.items().len(), 2);
        });
    }

    #[test]
    fn more_under_non_exhausted_limits_preserves_continuation() {
        block_on(async {
            let port = ExactCompletePort {
                candidates: vec!["c0", "c1", "c2"],
                records: vec!["r0", "r1", "r2"],
            };
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut candidate_state = CandidateReadState::new(
                PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );
            let mut record_state = TemporalRecordReadState::new(
                PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );

            let first_candidates = pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect("first candidate page");
            assert_eq!(first_candidates.status(), PageStatus::More);
            assert_eq!(
                first_candidates.continuation().map(PageKey::as_str),
                Some("1")
            );

            let second_candidates = pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect("second candidate page");
            assert_eq!(second_candidates.status(), PageStatus::More);
            assert_eq!(
                second_candidates.items()[0].stable_id.as_str(),
                "c1",
                "continuation must not skip or drop candidates"
            );

            let first_records = pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
                .await
                .expect("first record page");
            assert_eq!(first_records.status(), PageStatus::More);
            assert_eq!(first_records.continuation().map(PageKey::as_str), Some("1"));

            let second_records =
                pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
                    .await
                    .expect("second record page");
            assert_eq!(second_records.status(), PageStatus::More);
            match &second_records.items()[0] {
                TemporalRecord::SummarySource(record) => {
                    assert_eq!(
                        record.anchor_id.to_string(),
                        "r1",
                        "continuation must not skip or drop records"
                    );
                }
                _ => panic!("expected summary source record"),
            }
        });
    }

    #[test]
    fn exhausted_caps_never_synthesize_complete_or_silently_drop_unread_work() {
        block_on(async {
            let port = AlwaysMorePort::new(vec!["c0", "c1", "c2"], vec!["r0", "r1", "r2"]);
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut candidate_state = CandidateReadState::new(
                PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );
            let mut record_state = TemporalRecordReadState::new(
                PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );

            let candidate_err = pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect_err("More + candidate cap must not complete");
            assert_eq!(
                candidate_err,
                TemporalPortError::BudgetExceeded {
                    resource: "candidate item count"
                }
            );
            // A follow-up pull must keep failing closed — never empty Complete.
            let candidate_follow_up = pull_candidate_page(
                &port,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect_err("exhausted candidate state must not synthesize Complete");
            assert_eq!(
                candidate_follow_up,
                TemporalPortError::BudgetExceeded {
                    resource: "candidate item count"
                }
            );
            assert_ne!(
                candidate_follow_up,
                TemporalPortError::Read {
                    operation: "produce bounded page",
                    message: "producer returned an empty continuation page".to_string(),
                }
            );

            let Err(record_err) =
                pull_temporal_record_page(&port, &snapshot, &[], &mut record_state).await
            else {
                panic!("More + record cap must not complete");
            };
            assert_eq!(
                record_err,
                TemporalPortError::BudgetExceeded {
                    resource: "record item count"
                }
            );
            let Err(record_follow_up) =
                pull_temporal_record_page(&port, &snapshot, &[], &mut record_state).await
            else {
                panic!("exhausted record state must not synthesize Complete");
            };
            assert_eq!(
                record_follow_up,
                TemporalPortError::BudgetExceeded {
                    resource: "record item count"
                }
            );
        });
    }

    #[test]
    fn page_limits_reject_zero_inverted_and_absolute_ceilings() {
        assert_eq!(
            PageLimits::new(0, 1024, 1024, 1),
            Err(TemporalPortError::BudgetExceeded {
                resource: "item count"
            })
        );
        assert_eq!(
            PageLimits::new(1, 0, 1024, 1),
            Err(TemporalPortError::BudgetExceeded {
                resource: "total bytes"
            })
        );
        assert_eq!(
            PageLimits::new(1, 1024, 0, 1),
            Err(TemporalPortError::BudgetExceeded {
                resource: "item bytes"
            })
        );
        assert_eq!(
            PageLimits::new(1, 1024, 1024, 2),
            Err(TemporalPortError::BudgetExceeded {
                resource: "page item count"
            })
        );
        assert_eq!(
            PageLimits::new(usize::MAX, 1024, 1024, 1),
            Err(TemporalPortError::BudgetExceeded {
                resource: "item count"
            })
        );
        assert_eq!(
            PageLimits::new(MAX_READ_ITEMS, MAX_READ_TOTAL_BYTES + 1, 1024, 1),
            Err(TemporalPortError::BudgetExceeded {
                resource: "total bytes"
            })
        );
        assert!(
            PageLimits::new(1, 1024, 1024, 1).is_ok(),
            "canonical small limits must remain accepted"
        );
    }

    #[test]
    fn execution_limits_reject_zero_and_absolute_ceilings() {
        let oversize = ExecutionLimits {
            candidate_limit: MAX_READ_ITEMS + 1,
            ..ExecutionLimits::default()
        };
        assert_eq!(
            oversize.validate(),
            Err(TemporalPortError::BudgetExceeded {
                resource: "candidate item count"
            })
        );
        let zero = ExecutionLimits {
            record_item_bytes: 0,
            ..ExecutionLimits::default()
        };
        assert_eq!(
            zero.validate(),
            Err(TemporalPortError::BudgetExceeded {
                resource: "record item bytes"
            })
        );
        assert!(ExecutionLimits::default().validate().is_ok());
    }

    #[test]
    fn execution_snapshot_rejects_oversize_execution_limits() {
        let limits = ExecutionLimits {
            candidate_limit: MAX_READ_ITEMS + 1,
            ..ExecutionLimits::default()
        };
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request")
        .with_limits(limits);
        assert_eq!(
            TemporalExecutionSnapshot::new_authorized(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: 0,
                    projection: 0,
                    index: 0,
                    summary: 0,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                        .expect("valid digest"),
                },
                None,
                ValidatedAuthorization::Authorized,
            ),
            Err(TemporalPortError::BudgetExceeded {
                resource: "candidate item count"
            })
        );
    }

    type LimitGetter = fn(&ExecutionLimits) -> usize;
    type LimitSetter = fn(&mut ExecutionLimits, usize);

    fn execution_limit_fields() -> [(&'static str, LimitGetter, LimitSetter); 15] {
        [
            (
                "candidate_limit",
                |limits| limits.candidate_limit,
                |limits, value| limits.candidate_limit = value,
            ),
            (
                "candidate_total_bytes",
                |limits| limits.candidate_total_bytes,
                |limits, value| limits.candidate_total_bytes = value,
            ),
            (
                "candidate_item_bytes",
                |limits| limits.candidate_item_bytes,
                |limits, value| limits.candidate_item_bytes = value,
            ),
            (
                "candidate_key_bytes",
                |limits| limits.candidate_key_bytes,
                |limits, value| limits.candidate_key_bytes = value,
            ),
            (
                "candidate_stable_id_bytes",
                |limits| limits.candidate_stable_id_bytes,
                |limits, value| limits.candidate_stable_id_bytes = value,
            ),
            (
                "candidate_anchor_id_bytes",
                |limits| limits.candidate_anchor_id_bytes,
                |limits, value| limits.candidate_anchor_id_bytes = value,
            ),
            (
                "candidate_metadata_field_bytes",
                |limits| limits.candidate_metadata_field_bytes,
                |limits, value| limits.candidate_metadata_field_bytes = value,
            ),
            (
                "record_limit",
                |limits| limits.record_limit,
                |limits, value| limits.record_limit = value,
            ),
            (
                "record_total_bytes",
                |limits| limits.record_total_bytes,
                |limits, value| limits.record_total_bytes = value,
            ),
            (
                "record_item_bytes",
                |limits| limits.record_item_bytes,
                |limits, value| limits.record_item_bytes = value,
            ),
            (
                "record_key_bytes",
                |limits| limits.record_key_bytes,
                |limits, value| limits.record_key_bytes = value,
            ),
            (
                "hydration_limit",
                |limits| limits.hydration_limit,
                |limits, value| limits.hydration_limit = value,
            ),
            (
                "hydration_total_bytes",
                |limits| limits.hydration_total_bytes,
                |limits, value| limits.hydration_total_bytes = value,
            ),
            (
                "hydration_payload_bytes",
                |limits| limits.hydration_payload_bytes,
                |limits, value| limits.hydration_payload_bytes = value,
            ),
            (
                "hydration_chunk_bytes",
                |limits| limits.hydration_chunk_bytes,
                |limits, value| limits.hydration_chunk_bytes = value,
            ),
        ]
    }

    fn snapshot_with_limits(limits: ExecutionLimits) -> TemporalExecutionSnapshot {
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request")
        .with_limits(limits);
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("valid authorized snapshot")
    }

    #[test]
    fn snapshot_limit_tightening_is_monotonic_for_every_field() {
        let authorized = ExecutionLimits::default();

        for (field, get, set) in execution_limit_fields() {
            let authorized_value = get(&authorized);

            let mut tighter = authorized;
            set(&mut tighter, authorized_value - 1);
            let tightened = snapshot_with_limits(authorized)
                .with_limits(tighter)
                .expect("a valid component-wise decrease must succeed");
            assert_eq!(
                tightened.request().limits(),
                tighter,
                "tightening `{field}` must preserve the requested lower value"
            );
            assert_eq!(
                tightened.authorization(),
                ValidatedAuthorization::Authorized,
                "tightening `{field}` must preserve authorization"
            );

            let mut looser = authorized;
            set(&mut looser, authorized_value + 1);
            assert_eq!(
                snapshot_with_limits(authorized)
                    .with_limits(looser)
                    .expect_err("a component-wise increase must fail"),
                ExecutionLimitTighteningError::WouldLoosen {
                    field,
                    authorized: authorized_value,
                    requested: authorized_value + 1,
                }
            );
        }
    }

    #[test]
    fn snapshot_limit_tightening_accepts_equal_limits() {
        let limits = ExecutionLimits::default();
        let snapshot = snapshot_with_limits(limits)
            .with_limits(limits)
            .expect("equal limits are monotonic");

        assert_eq!(snapshot.request().limits(), limits);
        assert_eq!(snapshot.authorization(), ValidatedAuthorization::Authorized);
    }

    struct StableIdPort {
        stable_id: &'static str,
    }

    impl TemporalReadPort for StableIdPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async move {
                sink.push(candidate(self.stable_id))?;
                Ok(PageStatus::Complete)
            })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { Ok(PageStatus::Complete) })
        }
    }

    #[test]
    fn candidate_pull_observes_post_authorization_tightening() {
        block_on(async {
            let authorized = ExecutionLimits::default();
            let mut tighter = authorized;
            tighter.candidate_stable_id_bytes = 4;
            let snapshot = snapshot_with_limits(authorized)
                .with_limits(tighter)
                .expect("valid tightening");
            let mut state = CandidateReadState::new(
                PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );

            assert_eq!(
                pull_candidate_page(
                    &StableIdPort { stable_id: "12345" },
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate stable id bytes"
                })
            );
        });
    }

    struct UnreachableReadPort;

    impl TemporalReadPort for UnreachableReadPort {
        fn produce_candidate_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _plan: &'a CandidatePlan,
            _request: PageRequest,
            _sink: &'a mut CandidatePageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { panic!("looser candidate read state reached the producer") })
        }

        fn produce_temporal_record_page<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _candidates: &'a [RankingCandidate],
            _request: PageRequest,
            _sink: &'a mut TemporalRecordPageSink<'_>,
        ) -> PortFuture<'a, PageStatus> {
            Box::pin(async { panic!("looser record read state reached the producer") })
        }
    }

    #[test]
    fn pull_rejects_read_state_looser_than_tightened_snapshot() {
        block_on(async {
            let authorized = ExecutionLimits::default();
            let mut tighter = authorized;
            tighter.candidate_limit = 1;
            tighter.candidate_total_bytes = 128;
            tighter.candidate_item_bytes = 64;
            tighter.record_limit = 1;
            tighter.record_total_bytes = 128;
            tighter.record_item_bytes = 64;
            let snapshot = snapshot_with_limits(authorized)
                .with_limits(tighter)
                .expect("valid tightening");

            for (limits, resource) in [
                (
                    PageLimits::new(2, 128, 64, 1).expect("candidate count"),
                    "candidate item count",
                ),
                (
                    PageLimits::new(1, 129, 64, 1).expect("candidate total bytes"),
                    "candidate total bytes",
                ),
                (
                    PageLimits::new(1, 128, 65, 1).expect("candidate item bytes"),
                    "candidate item bytes",
                ),
            ] {
                let mut state = CandidateReadState::new(limits);
                assert_eq!(
                    pull_candidate_page(
                        &UnreachableReadPort,
                        &snapshot,
                        &CandidatePlan::default(),
                        &mut state,
                    )
                    .await,
                    Err(TemporalPortError::BudgetExceeded { resource })
                );
            }

            for (limits, resource) in [
                (
                    PageLimits::new(2, 128, 64, 1).expect("record count"),
                    "record item count",
                ),
                (
                    PageLimits::new(1, 129, 64, 1).expect("record total bytes"),
                    "record total bytes",
                ),
                (
                    PageLimits::new(1, 128, 65, 1).expect("record item bytes"),
                    "record item bytes",
                ),
            ] {
                let mut state = TemporalRecordReadState::new(limits);
                let Err(error) =
                    pull_temporal_record_page(&UnreachableReadPort, &snapshot, &[], &mut state)
                        .await
                else {
                    panic!("looser record state must fail before producer entry");
                };
                assert_eq!(error, TemporalPortError::BudgetExceeded { resource });
            }
        });
    }

    #[test]
    fn hydration_limits_cannot_be_replaced_or_loosened_after_authorization() {
        let authorized = ExecutionLimits::default();
        let mut tighter = authorized;
        tighter.hydration_limit -= 1;
        tighter.hydration_total_bytes -= 1;
        tighter.hydration_payload_bytes -= 1;
        tighter.hydration_chunk_bytes -= 1;
        let tightened = snapshot_with_limits(authorized)
            .with_limits(tighter)
            .expect("valid hydration tightening");

        assert_eq!(tightened.request().limits(), tighter);
        assert_eq!(
            tightened
                .clone()
                .with_limits(authorized)
                .expect_err("hydration limits cannot be restored to looser authorized values"),
            ExecutionLimitTighteningError::WouldLoosen {
                field: "hydration_limit",
                authorized: tighter.hydration_limit,
                requested: authorized.hydration_limit,
            }
        );
        assert_eq!(tightened.request().limits(), tighter);
        assert_eq!(
            tightened.authorization(),
            ValidatedAuthorization::Authorized
        );
    }

    #[test]
    fn bounded_page_sink_caps_initial_capacity_for_attacker_limits() {
        let limits =
            PageLimits::new(MAX_PAGE_ITEMS_CAP, 1024, 1024, MAX_PAGE_ITEMS_CAP).expect("limits");
        let mut state = CandidateReadState::new(limits);
        let control = ExecutionControl::default();
        let sink = state.begin_page(&control, 256, None, CANDIDATE_READ_BUDGET);
        assert!(sink.preallocated_capacity() <= MAX_BOUNDED_PAGE_PREALLOC);
        assert!(sink.preallocated_capacity() <= MAX_PAGE_ITEMS_CAP);
    }

    #[test]
    fn continuation_key_enforces_exact_byte_cap() {
        block_on(async {
            struct ContinuationPort {
                key_len: usize,
            }
            impl TemporalReadPort for ContinuationPort {
                fn produce_candidate_page<'a>(
                    &'a self,
                    _snapshot: &'a TemporalExecutionSnapshot,
                    _plan: &'a CandidatePlan,
                    _request: PageRequest,
                    sink: &'a mut CandidatePageSink<'_>,
                ) -> PortFuture<'a, PageStatus> {
                    Box::pin(async move {
                        sink.push(candidate("c0"))?;
                        sink.set_continuation_key(PageKey::new("k".repeat(self.key_len)))?;
                        Ok(PageStatus::More)
                    })
                }
                fn produce_temporal_record_page<'a>(
                    &'a self,
                    _snapshot: &'a TemporalExecutionSnapshot,
                    _candidates: &'a [RankingCandidate],
                    _request: PageRequest,
                    _sink: &'a mut TemporalRecordPageSink<'_>,
                ) -> PortFuture<'a, PageStatus> {
                    Box::pin(async { Ok(PageStatus::Complete) })
                }
            }
            let snapshot = snapshot_with_control(ExecutionControl::default());
            let mut ok_state = CandidateReadState::new(
                PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );
            pull_candidate_page(
                &ContinuationPort { key_len: 256 },
                &snapshot,
                &CandidatePlan::default(),
                &mut ok_state,
            )
            .await
            .expect("key at default cap");

            let mut over_state = CandidateReadState::new(
                PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"),
            );
            assert_eq!(
                pull_candidate_page(
                    &ContinuationPort { key_len: 257 },
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut over_state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource: "continuation key bytes"
                })
            );
        });
    }

    #[test]
    fn legacy_only_port_fails_closed_for_root_wide_scope() {
        block_on(async {
            struct LegacyOnlyPort;
            impl TemporalReadPort for LegacyOnlyPort {
                fn produce_candidate_page<'a>(
                    &'a self,
                    _snapshot: &'a TemporalExecutionSnapshot,
                    _plan: &'a CandidatePlan,
                    _request: PageRequest,
                    _sink: &'a mut CandidatePageSink<'_>,
                ) -> PortFuture<'a, PageStatus> {
                    Box::pin(async { Ok(PageStatus::Complete) })
                }
                fn produce_temporal_record_page<'a>(
                    &'a self,
                    _snapshot: &'a TemporalExecutionSnapshot,
                    _candidates: &'a [RankingCandidate],
                    _request: PageRequest,
                    _sink: &'a mut TemporalRecordPageSink<'_>,
                ) -> PortFuture<'a, PageStatus> {
                    Box::pin(async { Ok(PageStatus::Complete) })
                }
            }
            let request = TemporalSnapshotRequest::new(
                session_id(),
                digest('0'),
                digest('1'),
                digest('2'),
                TemporalModeV1::Current,
                RetrievalGrainV1::LogicalMessage,
            )
            .expect("valid request")
            .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
            let snapshot = TemporalExecutionSnapshot::new(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: 0,
                    projection: 0,
                    index: 0,
                    summary: 0,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                        .expect("valid digest"),
                },
                None,
            )
            .expect("valid snapshot");
            let mut candidate_state =
                CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("limits"));
            let err = pull_candidate_page(
                &LegacyOnlyPort,
                &snapshot,
                &CandidatePlan::default(),
                &mut candidate_state,
            )
            .await
            .expect_err("root-wide must not use silent legacy default");
            assert!(matches!(
                err,
                TemporalPortError::Read {
                    operation: "produce candidate page for scope",
                    ..
                }
            ));
        });
    }

    #[test]
    fn temporal_ports_and_cursor_are_runtime_and_sql_free() {
        let ports = fs::read_to_string("crates/tracedecay-query/src/temporal/ports.rs")
            .expect("ports");
        let cursor = fs::read_to_string("crates/tracedecay-query/src/temporal/cursor.rs")
            .expect("cursor");
        let (ports_prod, _) = ports
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("ports tests");
        let (cursor_prod, _) = cursor
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("cursor tests");
        for (label, source) in [("ports", ports_prod), ("cursor", cursor_prod)] {
            for forbidden in [
                "rusqlite",
                "sqlx",
                "diesel",
                "tokio::",
                "async_std",
                "std::thread::",
                "thread::spawn",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{label} production source must remain SQL/runtime-free; found `{forbidden}`"
                );
            }
        }
    }

    #[test]
    fn participant_manifest_reports_mixed_source_freshness_from_real_frontiers() {
        let configuration =
            BindingDigest::new("configuration_digest", digest('3')).expect("digest");
        let authorization =
            BindingDigest::new("authorization_digest", digest('4')).expect("digest");
        let participant = |source: &str, source_watermark, projection_watermark| {
            TemporalParticipantGeneration::new(
                SessionId::new(format!("session.{source}")).unwrap(),
                source,
                TemporalWatermarks {
                    generation: 1,
                    source: source_watermark,
                    projection: projection_watermark,
                    index: projection_watermark,
                    summary: 0,
                },
                projection_watermark,
                &configuration,
                &authorization,
                TemporalParticipantAuthorization::Authorized,
                TemporalSourceAccess::Available,
            )
            .unwrap()
        };
        let manifest = TemporalParticipantManifest::new(vec![
            participant("cursor", 10, 10),
            participant("claude", 10, 7),
        ])
        .unwrap();

        let receipt = manifest
            .source_coverage(TemporalModeV1::Current)
            .expect("source coverage");
        assert_eq!(receipt.sources().len(), 2);
        assert_eq!(
            receipt.aggregate_state(),
            tracedecay_domain::SessionSourceCoverageAggregateStateV1::Partial
        );
        assert_eq!(receipt.max_frontier_lag(), 3);
    }

    #[test]
    fn authorized_lifecycle_states_do_not_become_snapshot_denials() {
        let configuration =
            BindingDigest::new("configuration_digest", digest('3')).expect("digest");
        let authorization =
            BindingDigest::new("authorization_digest", digest('4')).expect("digest");
        for (access, expected_coverage) in [
            (
                TemporalSourceAccess::Locked,
                SessionSourceCoverageStateV1::Locked,
            ),
            (
                TemporalSourceAccess::RetentionWithheld,
                SessionSourceCoverageStateV1::RetentionWithheld,
            ),
            (
                TemporalSourceAccess::Deleted,
                SessionSourceCoverageStateV1::RetentionWithheld,
            ),
            (
                TemporalSourceAccess::Redacted,
                SessionSourceCoverageStateV1::Redacted,
            ),
            (
                TemporalSourceAccess::Unavailable,
                SessionSourceCoverageStateV1::Unavailable,
            ),
        ] {
            let participant = TemporalParticipantGeneration::new(
                SessionId::new("session.lifecycle").unwrap(),
                "claude",
                TemporalWatermarks {
                    generation: 1,
                    source: 10,
                    projection: 10,
                    index: 10,
                    summary: 10,
                },
                10,
                &configuration,
                &authorization,
                TemporalParticipantAuthorization::Authorized,
                access,
            )
            .unwrap();
            assert!(participant.is_authorized_for_snapshot());
            let coverage = TemporalParticipantManifest::new(vec![participant])
                .unwrap()
                .source_coverage(TemporalModeV1::Current)
                .unwrap();
            assert_eq!(coverage.sources()[0].state(), expected_coverage);
        }
    }

    #[test]
    fn manifests_without_explicit_authorization_fail_closed() {
        let participant = participant("session.stale", "claude", 1);
        let mut wire = serde_json::to_value(participant).unwrap();
        wire.as_object_mut().unwrap().remove("q");
        let stale: TemporalParticipantGeneration = serde_json::from_value(wire).unwrap();

        assert_eq!(
            stale.authorization(),
            TemporalParticipantAuthorization::Denied
        );
        assert!(!stale.is_authorized_for_snapshot());
    }
}
