use serde::Serialize;
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};

use super::{BindingDigest, ExecutionControl, ExecutionLimits, TemporalPortError};

const PROFILE_ROOT_PROJECT_KEY: &str = "user";

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

pub(super) fn validate_label(
    field: &'static str,
    value: &str,
) -> Result<(), TemporalPortError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(TemporalPortError::InvalidBinding { field });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum TemporalSessionScopeFilterV1 {
    #[default]
    #[serde(rename = "all")]
    All,
    #[serde(rename = "parents_only")]
    ParentsOnly,
    #[serde(rename = "subagents_only")]
    SubagentsOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum TemporalMessageTypeFilterV1 {
    #[default]
    #[serde(rename = "all")]
    All,
    #[serde(rename = "direct_user")]
    DirectUser,
    #[serde(rename = "tool_result")]
    ToolResult,
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
