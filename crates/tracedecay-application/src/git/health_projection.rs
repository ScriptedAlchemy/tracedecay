//! Application boundary for daemon-owned Git health projections.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{GitOidV1, ManifestDigest, SourceStoreId, UserProfileId};

use crate::{ApplicationContractError, ResolvedScope};

/// Exact project/profile/store authority admitted for one projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionBindingV1 {
    pub scope: ResolvedScope,
    pub profile_id: UserProfileId,
    pub store_id: SourceStoreId,
}

impl GitHealthProjectionBindingV1 {
    pub fn new(
        scope: ResolvedScope,
        profile_id: UserProfileId,
        store_id: SourceStoreId,
    ) -> Result<Self, ApplicationContractError> {
        scope.validate()?;
        profile_id.validate()?;
        store_id.validate()?;
        Ok(Self {
            scope,
            profile_id,
            store_id,
        })
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope.validate()?;
        self.profile_id.validate()?;
        self.store_id.validate()?;
        Ok(())
    }
}

/// Exact native source and projection generation for one health snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionSourceV1 {
    pub binding: GitHealthProjectionBindingV1,
    pub commit: GitOidV1,
    pub tree: GitOidV1,
    pub projection_generation: ManifestDigest,
    pub window_start_epoch_secs: i64,
    pub window_end_epoch_secs: i64,
}

/// One complete bounded Git churn projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionSnapshotV1 {
    pub source: GitHealthProjectionSourceV1,
    pub commits_projected: usize,
    pub batches_completed: u64,
    pub churn_entries: usize,
    pub coverage: GitHealthProjectionCoverageV1,
}

pub const GIT_HEALTH_CHURN_PAGE_LIMIT: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionChurnEntryV1 {
    pub path: String,
    pub churn: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHealthProjectionChurnPageV1 {
    pub entries: Vec<GitHealthProjectionChurnEntryV1>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitHealthProjectionCoverageV1 {
    Complete,
    Partial {
        reason: GitHealthProjectionPartialReasonV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHealthProjectionPartialReasonV1 {
    CommitLimit,
    FrontierLimit,
    UniquePathLimit,
    ChangedPathLimit,
    PathBytesLimit,
    CommitPathLimit,
    HistoryTraversalLimit,
}

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

pub trait GitHealthProjectionReadPortV1: Send + Sync {
    fn read_projection(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> GitHealthProjectionAvailabilityV1;

    fn read_churn_page(
        &self,
        binding: &GitHealthProjectionBindingV1,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> Result<GitHealthProjectionChurnPageV1, GitHealthProjectionUnavailableReasonV1>;
}

#[derive(Clone)]
pub struct GitHealthProjectionReadServiceV1 {
    inner: Arc<RwLock<GitHealthProjectionReadBindingV1>>,
}

struct GitHealthProjectionReadBindingV1 {
    binding: GitHealthProjectionBindingV1,
    port: Arc<dyn GitHealthProjectionReadPortV1>,
}

impl GitHealthProjectionReadServiceV1 {
    pub fn new(
        binding: GitHealthProjectionBindingV1,
        port: Arc<dyn GitHealthProjectionReadPortV1>,
    ) -> Result<Self, ApplicationContractError> {
        binding.validate()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(GitHealthProjectionReadBindingV1 {
                binding,
                port,
            })),
        })
    }

    pub fn binding(&self) -> Result<GitHealthProjectionBindingV1, ApplicationContractError> {
        self.inner
            .read()
            .map(|inner| inner.binding.clone())
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "git_health_projection_reader_lock",
            })
    }

    pub fn rebind(
        &self,
        binding: GitHealthProjectionBindingV1,
        port: Arc<dyn GitHealthProjectionReadPortV1>,
    ) -> Result<(), ApplicationContractError> {
        binding.validate()?;
        let mut inner = self
            .inner
            .write()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "git_health_projection_reader_lock",
            })?;
        *inner = GitHealthProjectionReadBindingV1 { binding, port };
        Ok(())
    }

    pub fn read(&self) -> GitHealthProjectionAvailabilityV1 {
        self.inner.read().map_or(
            GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable,
            },
            |inner| inner.port.read_projection(&inner.binding),
        )
    }

    pub fn read_churn_page(
        &self,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> Result<GitHealthProjectionChurnPageV1, GitHealthProjectionUnavailableReasonV1> {
        let inner = self
            .inner
            .read()
            .map_err(|_| GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable)?;
        inner.port.read_churn_page(
            &inner.binding,
            after_cursor,
            limit.clamp(1, GIT_HEALTH_CHURN_PAGE_LIMIT),
        )
    }
}
