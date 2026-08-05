//! Application boundary for daemon-owned Git health projections.

use std::collections::BTreeMap;
use std::sync::Arc;

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
    pub file_churn: BTreeMap<String, usize>,
    pub coverage: GitHealthProjectionCoverageV1,
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
}

#[derive(Clone)]
pub struct GitHealthProjectionReadServiceV1 {
    binding: GitHealthProjectionBindingV1,
    port: Arc<dyn GitHealthProjectionReadPortV1>,
}

impl GitHealthProjectionReadServiceV1 {
    pub fn new(
        binding: GitHealthProjectionBindingV1,
        port: Arc<dyn GitHealthProjectionReadPortV1>,
    ) -> Result<Self, ApplicationContractError> {
        binding.validate()?;
        Ok(Self { binding, port })
    }

    pub fn binding(&self) -> &GitHealthProjectionBindingV1 {
        &self.binding
    }

    pub fn read(&self) -> GitHealthProjectionAvailabilityV1 {
        self.port.read_projection(&self.binding)
    }
}
