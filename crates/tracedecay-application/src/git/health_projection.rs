//! Application boundary for daemon-owned Git health projections.
//!
//! Native Git remains the source of repository state. The application service
//! pins one admitted project/repository/worktree/ref scope and exposes only the
//! latest complete, immutable projection owned by the daemon.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{GitOidV1, ManifestDigest};

use crate::{ApplicationContractError, ResolvedScope};

/// Exact native source and projection generation for one health snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionSourceV1 {
    pub scope: ResolvedScope,
    pub commit: GitOidV1,
    pub tree: GitOidV1,
    pub projection_generation: ManifestDigest,
    pub window_start_epoch_secs: i64,
    pub window_end_epoch_secs: i64,
}

/// One complete Git churn projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionSnapshotV1 {
    pub source: GitHealthProjectionSourceV1,
    pub commits_projected: usize,
    pub batches_completed: u64,
    pub file_churn: BTreeMap<String, usize>,
    pub coverage: GitHealthProjectionCoverageV1,
}

/// Whether the bounded projection represents the whole requested Git window.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitHealthProjectionCoverageV1 {
    Complete,
    Partial {
        reason: GitHealthProjectionPartialReasonV1,
    },
}

/// Stable bound that stopped a projection before the whole window was covered.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHealthProjectionPartialReasonV1 {
    CommitLimit,
    FrontierLimit,
    UniquePathLimit,
    ChangedPathLimit,
    PathBytesLimit,
    RelationLimit,
    CommitPathLimit,
}

/// Stable reason a Git health projection cannot currently be used.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHealthProjectionUnavailableReasonV1 {
    NotMounted,
    ScopeDrift,
    NativeGitUnavailable,
    ProjectionStoreUnavailable,
    ResetRequired,
    CorruptProjection,
}

/// Truthful availability of the daemon-owned Git health projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitHealthProjectionAvailabilityV1 {
    Ready {
        snapshot: GitHealthProjectionSnapshotV1,
    },
    Refreshing {
        snapshot: GitHealthProjectionSnapshotV1,
        target: GitHealthProjectionSourceV1,
    },
    Warming {
        target: Option<GitHealthProjectionSourceV1>,
    },
    Stale {
        snapshot: GitHealthProjectionSnapshotV1,
        reason: GitHealthProjectionUnavailableReasonV1,
    },
    Unavailable {
        reason: GitHealthProjectionUnavailableReasonV1,
    },
}

/// Daemon adapter used by the application read service.
pub trait GitHealthProjectionReadPortV1: Send + Sync {
    fn read_projection(&self, scope: &ResolvedScope) -> GitHealthProjectionAvailabilityV1;
}

/// Scope-pinned application reader used by health surfaces.
#[derive(Clone)]
pub struct GitHealthProjectionReadServiceV1 {
    scope: ResolvedScope,
    port: Arc<dyn GitHealthProjectionReadPortV1>,
}

impl GitHealthProjectionReadServiceV1 {
    pub fn new(
        scope: ResolvedScope,
        port: Arc<dyn GitHealthProjectionReadPortV1>,
    ) -> Result<Self, ApplicationContractError> {
        scope.validate()?;
        Ok(Self { scope, port })
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn read(&self) -> GitHealthProjectionAvailabilityV1 {
        self.port.read_projection(&self.scope)
    }
}
