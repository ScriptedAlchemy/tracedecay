//! Daemon-owned `FastEmbed` model acquisition lifecycle.
//!
//! Settings select a cataloged model (default [`DEFAULT_FASTEMBED_MODEL_ID`]).
//! Installation stays offline-safe. Project open only records the catalog
//! selection; strict semantic demand may queue acquisition of the immutable
//! revision in the background. The request never waits for model bytes and no
//! ambient hub or cache becomes serving authority.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_semantic_contracts::{
    ArtifactMemberPinV1, ArtifactMemberRoleV1, ArtifactPackageMemberV1, ArtifactProfileKindV1,
    DEFAULT_FASTEMBED_MODEL_ID, MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ModelArtifactManifestPayloadV1,
    ModelArtifactManifestV1, PlatformTargetV1, RerankCompatibilityPinsV1,
    RerankerArtifactLifecycleStatusV1, ResourceCeilingV1, RuntimeCompatibilityV1,
    SemanticLifecycleVerifiedReadyEventV1, SemanticModelLifecycleStateV1,
    SemanticModelLifecycleStatusV1, SemanticModelRemediationV1, SemanticResourceCeilings,
    Sha256DigestHex, TruncationPolicyV1, UpstreamSourceV1,
};

#[cfg(any(feature = "semantic-fastembed", feature = "semantic-model2vec"))]
use hf_hub::{Cache, Repo, RepoType, api::sync::ApiBuilder};

use super::artifact_store::{
    ArtifactImportErrorV1, ArtifactInventoryRecordV1, ArtifactLeaseKindV1, ArtifactLeaseV1,
    ConfiguredHttpsArtifactSourceV1, ExplicitHttpsArtifactTransportV1, GcReceiptV1,
    ModelArtifactStore, RetentionPolicyV1, RuntimeEnvironmentV1,
};
use super::model_catalog::{
    CatalogErrorV1, CatalogedFastEmbedModelV1, FastEmbedModelCatalogV1, catalog_package_digest,
};

const LIFECYCLE_SCHEMA_V1: &str = "tracedecay.fastembed.model-lifecycle.v1";
const INSTALL_META_SCHEMA_V1: &str = "tracedecay.fastembed.model-install.v1";
const ARTIFACT_GC_LEASE_SECONDS: u64 = 5 * 60;
const HF_HUB_CACHE_DIRECTORY_V1: &str = "hf-hub-cache";
const RERANKER_ACTIVE_LEASE_ID_V1: &str = "reranker:active:v1";
const RERANKER_ROLLBACK_LEASE_ID_V1: &str = "reranker:rollback:v1";
const EMBEDDING_ACTIVE_LEASE_ID_V1: &str = "embedding:active:v1";
const EMBEDDING_ROLLBACK_LEASE_ID_V1: &str = "embedding:rollback:v1";
static SHARED_LIFECYCLE_OWNER: std::sync::OnceLock<Option<Arc<SemanticModelLifecycleOwnerV1>>> =
    std::sync::OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableLifecycleV1 {
    schema: String,
    selected_model: Option<String>,
    auto_download: bool,
    state: Option<SemanticModelLifecycleStateV1>,
    previous_ready: Option<SemanticModelLifecycleStateV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallMetaV1 {
    schema: String,
    model_id: String,
    revision: String,
    artifact_digest: String,
}

/// Errors from lifecycle ownership operations.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelLifecycleErrorV1 {
    #[error(transparent)]
    Catalog(#[from] CatalogErrorV1),
    #[error("semantic model lifecycle store is unavailable")]
    StoreUnavailable,
    #[error("semantic model lifecycle operation rejected")]
    Rejected,
    #[error("semantic model download failed")]
    DownloadFailed,
    #[error("semantic model download failed: {0}")]
    DownloadFailedWithReason(String),
    #[error("semantic model verification failed")]
    VerificationFailed,
    #[error("semantic reranker warm-up is unavailable")]
    RerankerUnavailable,
    #[error("semantic model install failed")]
    InstallFailed,
    #[error("semantic model acquisition worker failed while joining")]
    WorkerJoinFailed,
    #[error("semantic model acquisition was cancelled")]
    Cancelled,
    #[error("cancelled semantic model acquisition was quarantined at {0}")]
    CancellationCleanupQuarantined(PathBuf),
    #[error("cancelled semantic model acquisition cleanup failed for {0}")]
    CancellationCleanupFailed(PathBuf),
    #[error(transparent)]
    ArtifactImport(#[from] ArtifactImportErrorV1),
}

#[derive(Default)]
struct AcquisitionControlV1 {
    state: Mutex<AcquisitionControlStateV1>,
}

#[derive(Default)]
struct AcquisitionControlStateV1 {
    epoch: u64,
    cancelled: bool,
}

#[derive(Clone)]
struct AcquisitionEpochV1 {
    control: Arc<AcquisitionControlV1>,
    epoch: u64,
}

impl AcquisitionControlV1 {
    fn begin_epoch(self: &Arc<Self>) -> AcquisitionEpochV1 {
        let epoch = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.epoch = state.epoch.wrapping_add(1);
            state.cancelled = false;
            state.epoch
        };
        AcquisitionEpochV1 {
            control: Arc::clone(self),
            epoch,
        }
    }

    fn cancel_current(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled = true;
    }
}

impl AcquisitionEpochV1 {
    fn ensure_active(&self) -> Result<(), ModelLifecycleErrorV1> {
        let state = self
            .control
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.epoch == self.epoch && !state.cancelled {
            Ok(())
        } else {
            Err(ModelLifecycleErrorV1::Cancelled)
        }
    }

    fn while_active<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ModelLifecycleErrorV1>,
    ) -> Result<T, ModelLifecycleErrorV1> {
        let state = self
            .control
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.epoch != self.epoch || state.cancelled {
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        operation()
    }

    fn while_current<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ModelLifecycleErrorV1>,
    ) -> Result<T, ModelLifecycleErrorV1> {
        let state = self
            .control
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.epoch != self.epoch {
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        operation()
    }
}

/// Supplies package member bytes for a cataloged model.
///
/// Production uses the daemon-owned hub source against the catalog's immutable
/// repository revision. Tests may inject a fixture source through this port.
pub trait ModelMemberSourceV1: Send + Sync {
    fn fetch_member(
        &self,
        model: &CatalogedFastEmbedModelV1,
        upstream_path: &str,
        destination: &Path,
    ) -> Result<(), ModelLifecycleErrorV1>;
}

/// Daemon-owned Hugging Face source scoped to the lifecycle root.
///
/// The client never uses `FastEmbed`'s ambient cache discovery: it resolves the
/// cataloged repository and immutable revision into this explicit cache, then
/// the lifecycle independently checks every member's length and SHA-256 before
/// atomically publishing an install.
#[derive(Debug)]
pub struct HfHubModelMemberSourceV1 {
    cache_dir: PathBuf,
    endpoint: Option<String>,
    offline: bool,
}

impl HfHubModelMemberSourceV1 {
    fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            endpoint: None,
            offline: hf_hub_offline(),
        }
    }

    #[cfg(all(test, feature = "semantic-fastembed"))]
    fn new_for_tests(cache_dir: PathBuf, endpoint: Option<String>, offline: bool) -> Self {
        Self {
            cache_dir,
            endpoint,
            offline,
        }
    }
}

impl ModelMemberSourceV1 for HfHubModelMemberSourceV1 {
    #[hotpath::measure(label = "semantic.model_lifecycle.fetch_member")]
    fn fetch_member(
        &self,
        model: &CatalogedFastEmbedModelV1,
        upstream_path: &str,
        destination: &Path,
    ) -> Result<(), ModelLifecycleErrorV1> {
        fetch_hf_hub_member(
            &self.cache_dir,
            self.endpoint.as_deref(),
            self.offline,
            model,
            upstream_path,
            destination,
        )
    }
}

// Acquisition is backend-independent: any compiled embedding runtime needs
// the daemon-owned hub source, so it compiles with either backend feature.
#[cfg(any(feature = "semantic-fastembed", feature = "semantic-model2vec"))]
fn fetch_hf_hub_member(
    cache_dir: &Path,
    endpoint: Option<&str>,
    offline: bool,
    model: &CatalogedFastEmbedModelV1,
    upstream_path: &str,
    destination: &Path,
) -> Result<(), ModelLifecycleErrorV1> {
    let cache = Cache::new(cache_dir.to_path_buf());
    let repository = Repo::with_revision(
        model.model_code.clone(),
        RepoType::Model,
        model.source.revision.clone(),
    );
    let cached = cache.repo(repository.clone()).get(upstream_path);
    let source = match cached {
        Some(path) => {
            crate::hotpath_observe::record_artifact_cache(true);
            path
        }
        None if offline => {
            crate::hotpath_observe::record_artifact_cache(false);
            crate::hotpath_observe::record_remote_failure("offline_cache_miss");
            return Err(ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                "member '{upstream_path}' is absent from the private cache while offline mode is enabled"
            )));
        }
        None => {
            crate::hotpath_observe::record_artifact_cache(false);
            let mut builder = ApiBuilder::from_cache(cache)
                .with_token(None)
                .with_progress(false)
                .with_retries(3);
            if let Some(endpoint) = endpoint {
                builder = builder.with_endpoint(endpoint.to_owned());
            }
            hotpath::measure_block!("semantic.hf_hub.download", {
                builder
                    .build()
                    .map_err(|error| {
                        crate::hotpath_observe::record_remote_failure("download_failed");
                        ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                            "cannot initialize the Hugging Face client for '{}': {error}",
                            model.model_code
                        ))
                    })?
                    .repo(repository)
                    .get(upstream_path)
                    .map_err(|error| {
                        crate::hotpath_observe::record_remote_failure("download_failed");
                        ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                            "cannot acquire '{}@{}/{}': {error}",
                            model.model_code, model.source.revision, upstream_path
                        ))
                    })?
            })
        }
    };
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
    }
    hotpath::measure_block!("semantic.hf_hub.decode", {
        fs::copy(source, destination).map(|_| ()).map_err(|error| {
            crate::hotpath_observe::record_remote_failure("download_failed");
            ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                "cannot copy cached member '{upstream_path}' into staging: {error}"
            ))
        })
    })
}

fn hf_hub_offline() -> bool {
    std::env::var("HF_HUB_OFFLINE")
        .is_ok_and(|value| !value.is_empty() && !matches!(value.as_str(), "0" | "false" | "FALSE"))
}

#[cfg(not(any(feature = "semantic-fastembed", feature = "semantic-model2vec")))]
fn fetch_hf_hub_member(
    cache_dir: &Path,
    endpoint: Option<&str>,
    offline: bool,
    model: &CatalogedFastEmbedModelV1,
    upstream_path: &str,
    destination: &Path,
) -> Result<(), ModelLifecycleErrorV1> {
    let _ = (
        cache_dir,
        endpoint,
        offline,
        model,
        upstream_path,
        destination,
    );
    crate::hotpath_observe::record_remote_failure("rejected");
    Err(ModelLifecycleErrorV1::Rejected)
}

include!("model_lifecycle/owner.rs");
include!("model_lifecycle/reconciliation.rs");
include!("model_lifecycle/acquisition.rs");
include!("model_lifecycle/persistence.rs");
include!("model_lifecycle/local_evaluation.rs");
include!("model_lifecycle/shared.rs");

#[cfg(all(test, feature = "semantic-fastembed"))]
#[path = "model_lifecycle/distribution_acquisition_acceptance.rs"]
mod distribution_acquisition_acceptance;

#[cfg(test)]
mod tests {
    include!("model_lifecycle/tests_first.rs");
    include!("model_lifecycle/tests_second.rs");
}
