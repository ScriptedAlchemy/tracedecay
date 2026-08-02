//! Daemon-owned `FastEmbed` model acquisition lifecycle.
//!
//! Settings select a cataloged model (default [`DEFAULT_FASTEMBED_MODEL_ID`]).
//! Installation stays offline-safe; after startup, the daemon may acquire the
//! immutable catalog revision in the background. Search never discovers an
//! ambient hub/cache or downloads model bytes at query time.
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(feature = "semantic-fastembed")]
use hf_hub::{Cache, Repo, RepoType, api::sync::ApiBuilder};

use super::artifact_store::{
    ArtifactImportErrorV1, ArtifactInventoryRecordV1, ArtifactLeaseKindV1, ArtifactLeaseV1,
    ConfiguredHttpsArtifactSourceV1, ExplicitHttpsArtifactTransportV1, GcReceiptV1,
    ModelArtifactStore, RetentionPolicyV1, RuntimeEnvironmentV1,
};
use super::manifest::{ArtifactMemberRoleV1, ModelArtifactManifestV1};
use super::model_catalog::{
    CatalogErrorV1, CatalogedFastEmbedModelV1, FastEmbedModelCatalogV1, catalog_package_digest,
};
use crate::{DEFAULT_FASTEMBED_MODEL_ID, RerankCompatibilityPinsV1};

const LIFECYCLE_SCHEMA_V1: &str = "tracedecay.fastembed.model-lifecycle.v1";
const INSTALL_META_SCHEMA_V1: &str = "tracedecay.fastembed.model-install.v1";
const ARTIFACT_GC_LEASE_SECONDS: u64 = 5 * 60;
const HF_HUB_CACHE_DIRECTORY_V1: &str = "hf-hub-cache";
const RERANKER_ACTIVE_LEASE_ID_V1: &str = "reranker:active:v1";
const RERANKER_ROLLBACK_LEASE_ID_V1: &str = "reranker:rollback:v1";
static SHARED_LIFECYCLE_OWNER: std::sync::OnceLock<Option<Arc<SemanticModelLifecycleOwnerV1>>> =
    std::sync::OnceLock::new();

/// Doctor/status lifecycle states for the selected `FastEmbed` model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticModelLifecycleStateV1 {
    SelectedNotDownloaded {
        model_id: String,
        revision: String,
        artifact_digest: String,
    },
    Downloading {
        model_id: String,
        revision: String,
        artifact_digest: String,
        bytes_received: u64,
        bytes_total: u64,
    },
    Verifying {
        model_id: String,
        revision: String,
        artifact_digest: String,
    },
    Installed {
        model_id: String,
        revision: String,
        artifact_digest: String,
        install_path: PathBuf,
    },
    Loading {
        model_id: String,
        revision: String,
        artifact_digest: String,
        install_path: PathBuf,
    },
    Indexing {
        model_id: String,
        revision: String,
        artifact_digest: String,
        install_path: PathBuf,
        completed_units: u64,
        total_units: u64,
    },
    Ready {
        model_id: String,
        revision: String,
        artifact_digest: String,
        install_path: PathBuf,
    },
    Failed {
        model_id: String,
        revision: String,
        artifact_digest: String,
        detail: String,
        retryable: bool,
    },
}

impl SemanticModelLifecycleStateV1 {
    pub fn model_id(&self) -> &str {
        match self {
            Self::SelectedNotDownloaded { model_id, .. }
            | Self::Downloading { model_id, .. }
            | Self::Verifying { model_id, .. }
            | Self::Installed { model_id, .. }
            | Self::Loading { model_id, .. }
            | Self::Indexing { model_id, .. }
            | Self::Ready { model_id, .. }
            | Self::Failed { model_id, .. } => model_id,
        }
    }

    pub fn artifact_digest(&self) -> &str {
        match self {
            Self::SelectedNotDownloaded {
                artifact_digest, ..
            }
            | Self::Downloading {
                artifact_digest, ..
            }
            | Self::Verifying {
                artifact_digest, ..
            }
            | Self::Installed {
                artifact_digest, ..
            }
            | Self::Loading {
                artifact_digest, ..
            }
            | Self::Indexing {
                artifact_digest, ..
            }
            | Self::Ready {
                artifact_digest, ..
            }
            | Self::Failed {
                artifact_digest, ..
            } => artifact_digest,
        }
    }

    /// Semantics are omitted while acquisition/load/index is incomplete.
    pub fn omits_semantics(&self) -> bool {
        !matches!(self, Self::Ready { .. })
    }

    pub fn remediation(&self) -> SemanticModelRemediationV1 {
        match self {
            Self::Failed {
                retryable: true, ..
            }
            | Self::SelectedNotDownloaded { .. } => SemanticModelRemediationV1 {
                retry: true,
                remove: matches!(self, Self::Failed { .. }),
                rollback: false,
            },
            Self::Installed { .. }
            | Self::Loading { .. }
            | Self::Indexing { .. }
            | Self::Ready { .. }
            | Self::Failed {
                retryable: false, ..
            } => SemanticModelRemediationV1 {
                retry: matches!(self, Self::Failed { .. }),
                remove: true,
                rollback: true,
            },
            Self::Downloading { .. } | Self::Verifying { .. } => SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            },
        }
    }
}

/// Safe remediation actions Doctor/status may expose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticModelRemediationV1 {
    pub retry: bool,
    pub remove: bool,
    pub rollback: bool,
}

/// Public status envelope for Doctor and daemon runtime status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticModelLifecycleStatusV1 {
    pub selected_model: Option<String>,
    pub auto_download: bool,
    pub catalog_model_ids: Vec<String>,
    pub state: Option<SemanticModelLifecycleStateV1>,
    pub remediation: SemanticModelRemediationV1,
    pub semantics_omitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RerankerArtifactLifecycleStatusV1 {
    pub active_artifact_digest: Option<super::manifest::Sha256DigestHex>,
    pub rollback_artifact_digest: Option<super::manifest::Sha256DigestHex>,
}

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
    #[error("semantic model install failed")]
    InstallFailed,
    #[error(transparent)]
    ArtifactImport(#[from] ArtifactImportErrorV1),
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

#[cfg(feature = "semantic-fastembed")]
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
        Some(path) => path,
        None if offline => {
            return Err(ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                "member '{upstream_path}' is absent from the private cache while offline mode is enabled"
            )));
        }
        None => {
            let mut builder = ApiBuilder::from_cache(cache)
                .with_token(None)
                .with_progress(false)
                .with_retries(3);
            if let Some(endpoint) = endpoint {
                builder = builder.with_endpoint(endpoint.to_owned());
            }
            builder
                .build()
                .map_err(|error| {
                    ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                        "cannot initialize the Hugging Face client for '{}': {error}",
                        model.model_code
                    ))
                })?
                .repo(repository)
                .get(upstream_path)
                .map_err(|error| {
                    ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
                        "cannot acquire '{}@{}/{}': {error}",
                        model.model_code, model.source.revision, upstream_path
                    ))
                })?
        }
    };
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
    }
    fs::copy(source, destination).map(|_| ()).map_err(|error| {
        ModelLifecycleErrorV1::DownloadFailedWithReason(format!(
            "cannot copy cached member '{upstream_path}' into staging: {error}"
        ))
    })
}

fn hf_hub_offline() -> bool {
    std::env::var("HF_HUB_OFFLINE")
        .is_ok_and(|value| !value.is_empty() && !matches!(value.as_str(), "0" | "false" | "FALSE"))
}

#[cfg(not(feature = "semantic-fastembed"))]
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
    Err(ModelLifecycleErrorV1::Rejected)
}

/// Owns selection, background acquisition, and remediation for one data root.
pub struct SemanticModelLifecycleOwnerV1 {
    root: PathBuf,
    catalog: FastEmbedModelCatalogV1,
    source: Arc<dyn ModelMemberSourceV1>,
    artifact_store: ModelArtifactStore,
    inner: Arc<Mutex<LifecycleInner>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
}

struct LifecycleInner {
    durable: DurableLifecycleV1,
}

impl SemanticModelLifecycleOwnerV1 {
    pub fn open(
        root: impl Into<PathBuf>,
        catalog: FastEmbedModelCatalogV1,
        source: Arc<dyn ModelMemberSourceV1>,
    ) -> Result<Self, ModelLifecycleErrorV1> {
        catalog.validate()?;
        let root = root.into();
        fs::create_dir_all(root.join("staging"))
            .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        fs::create_dir_all(root.join("installs"))
            .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        let artifact_store = ModelArtifactStore::open(
            root.join("verified-artifacts"),
            RetentionPolicyV1 {
                grace_seconds: 7 * 24 * 60 * 60,
            },
        )?;
        let durable = load_or_default_durable(&root, &catalog)?;
        Ok(Self {
            root,
            catalog,
            source,
            artifact_store,
            inner: Arc::new(Mutex::new(LifecycleInner { durable })),
            worker: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn open_default(root: impl Into<PathBuf>) -> Result<Self, ModelLifecycleErrorV1> {
        let root = root.into();
        let source = Arc::new(HfHubModelMemberSourceV1::new(
            root.join(HF_HUB_CACHE_DIRECTORY_V1),
        ));
        Self::open(root, FastEmbedModelCatalogV1::production(), source)
    }

    pub fn catalog(&self) -> &FastEmbedModelCatalogV1 {
        &self.catalog
    }

    /// Re-admit the independently evaluated reranker selected by exact
    /// compatibility pins. No catalog lookup, network access, or ambient
    /// cache participates in this mount.
    pub fn mount_reranker(
        &self,
        pins: RerankCompatibilityPinsV1,
    ) -> Result<super::rerank_adapter::ProductionCodeRerankAuthorityV1, ModelLifecycleErrorV1> {
        let digest = pins
            .artifact_manifest_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(ModelLifecycleErrorV1::VerificationFailed)
            .and_then(|digest| {
                super::manifest::Sha256DigestHex::new(digest.to_owned())
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)
            })?;
        let artifact = self
            .artifact_store
            .admit_leased_for_runtime_by_digest(
                &digest,
                &RuntimeEnvironmentV1::detect_fastembed_process()
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?,
                RERANKER_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                current_unix_seconds()?,
            )
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        super::rerank_adapter::ProductionCodeRerankAuthorityV1::from_admitted(artifact, pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)
    }

    pub fn import_local_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        super::rerank_adapter::validate_reranker_manifest_pins(manifest, &pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let record = self
            .artifact_store
            .import_local_directory(manifest, source, now_unix)?;
        self.publish_reranker_artifact(pins, record, now_unix)
    }

    pub fn import_configured_https_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        super::rerank_adapter::validate_reranker_manifest_pins(manifest, &pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let record = self.artifact_store.import_configured_https(
            manifest,
            source,
            transport,
            resume_staging_id,
            now_unix,
        )?;
        self.publish_reranker_artifact(pins, record, now_unix)
    }

    fn publish_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        record: ArtifactInventoryRecordV1,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let environment = RuntimeEnvironmentV1::detect_fastembed_process()
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let admitted = self
            .artifact_store
            .admit_for_runtime_by_digest(&record.artifact_digest, &environment)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        super::rerank_adapter::ProductionCodeRerankAuthorityV1::from_admitted(admitted, pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        self.artifact_store.activate_artifact_with_rollback(
            &record.artifact_digest,
            RERANKER_ACTIVE_LEASE_ID_V1,
            RERANKER_ROLLBACK_LEASE_ID_V1,
            now_unix,
        )?;
        self.reranker_artifact_status()
    }

    pub fn reranker_artifact_status(
        &self,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let now_unix = current_unix_seconds()?;
        Ok(RerankerArtifactLifecycleStatusV1 {
            active_artifact_digest: self.artifact_store.artifact_digest_for_lease(
                RERANKER_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                now_unix,
            )?,
            rollback_artifact_digest: self.artifact_store.artifact_digest_for_lease(
                RERANKER_ROLLBACK_LEASE_ID_V1,
                ArtifactLeaseKindV1::Rollback,
                now_unix,
            )?,
        })
    }

    pub fn rollback_reranker_artifact(
        &self,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let rollback = self
            .reranker_artifact_status()?
            .rollback_artifact_digest
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        self.artifact_store.activate_artifact_with_rollback(
            &rollback,
            RERANKER_ACTIVE_LEASE_ID_V1,
            RERANKER_ROLLBACK_LEASE_ID_V1,
            now_unix,
        )?;
        self.reranker_artifact_status()
    }

    pub fn mounted_shared() -> Option<Arc<Self>> {
        SHARED_LIFECYCLE_OWNER.get().cloned().flatten()
    }

    pub fn run_daemon_artifact_gc(
        &self,
        now_unix: u64,
    ) -> Result<Vec<GcReceiptV1>, ModelLifecycleErrorV1> {
        let expires_at_unix = now_unix
            .checked_add(ARTIFACT_GC_LEASE_SECONDS)
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        let lease = self.artifact_store.acquire_daemon_gc_lease(
            format!("daemon:{}:{now_unix}", std::process::id()),
            expires_at_unix,
            now_unix,
        )?;
        self.artifact_store
            .gc_with_daemon_lease(&lease, now_unix)
            .map_err(Into::into)
    }

    /// Explicitly import a complete local package through the verified store.
    /// Selection alone never invokes this operation.
    pub fn import_local_artifact(
        &self,
        model_id: &str,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let model = self
            .catalog
            .get(model_id)
            .ok_or(CatalogErrorV1::UnknownModel)?;
        verify_catalog_manifest(model, manifest)?;
        let record = self
            .artifact_store
            .import_local_directory(manifest, source, now_unix)?;
        self.publish_explicit_import(model, record, now_unix)
    }

    /// Explicitly import or resume from a configured immutable HTTPS source.
    /// The typed transport is supplied by the user-action boundary and is not
    /// retained by startup, query, or runtime paths.
    pub fn import_configured_https_artifact(
        &self,
        model_id: &str,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let model = self
            .catalog
            .get(model_id)
            .ok_or(CatalogErrorV1::UnknownModel)?;
        verify_catalog_manifest(model, manifest)?;
        let record = self.artifact_store.import_configured_https(
            manifest,
            source,
            transport,
            resume_staging_id,
            now_unix,
        )?;
        self.publish_explicit_import(model, record, now_unix)
    }

    fn publish_explicit_import(
        &self,
        model: &CatalogedFastEmbedModelV1,
        record: ArtifactInventoryRecordV1,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let lease_id = format!("active:{}:{}", model.model_id, model.source.revision);
        self.artifact_store.acquire_artifact_lease(
            &record.artifact_digest,
            ArtifactLeaseV1 {
                lease_id,
                kind: ArtifactLeaseKindV1::Active,
                expires_at_unix: u64::MAX,
            },
            now_unix,
        )?;
        let install_path = self
            .artifact_store
            .installed_directory(&record.artifact_digest);
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous @ SemanticModelLifecycleStateV1::Ready { .. }) =
            guard.durable.state.clone()
        {
            if let Ok(previous_digest) =
                super::manifest::Sha256DigestHex::new(previous.artifact_digest().to_owned())
            {
                let _ = self.artifact_store.acquire_artifact_lease(
                    &previous_digest,
                    ArtifactLeaseV1 {
                        lease_id: format!("rollback:{}", previous.model_id()),
                        kind: ArtifactLeaseKindV1::Rollback,
                        expires_at_unix: u64::MAX,
                    },
                    now_unix,
                );
            }
            guard.durable.previous_ready = Some(previous);
        }
        guard.durable.selected_model = Some(model.model_id.clone());
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: record.artifact_digest.to_string(),
            install_path,
        });
        persist_durable(&self.root, &guard.durable)?;
        drop(guard);
        Ok(self.status())
    }

    pub fn status(&self) -> SemanticModelLifecycleStatusV1 {
        let guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let mut remediation = guard.durable.state.as_ref().map_or(
            SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            },
            SemanticModelLifecycleStateV1::remediation,
        );
        if matches!(
            guard.durable.previous_ready.as_ref(),
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ) {
            remediation.rollback = true;
        }
        let semantics_omitted = guard
            .durable
            .state
            .as_ref()
            .is_none_or(SemanticModelLifecycleStateV1::omits_semantics);
        SemanticModelLifecycleStatusV1 {
            selected_model: guard.durable.selected_model.clone(),
            auto_download: guard.durable.auto_download,
            catalog_model_ids: self.catalog.model_ids().map(str::to_owned).collect(),
            state: guard.durable.state.clone(),
            remediation,
            semantics_omitted,
        }
    }

    /// Apply a settings selection. `None` disables semantics without download.
    pub fn select_model(
        &self,
        model_id: Option<&str>,
        auto_download: bool,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let selected = match model_id {
            Some(model_id) => {
                let model = self
                    .catalog
                    .get(model_id)
                    .ok_or(CatalogErrorV1::UnknownModel)?;
                Some((model, self.re_admit_durable_selection(model)?))
            }
            None => None,
        };
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.durable.auto_download = auto_download;
        match selected {
            None => {
                guard.durable.selected_model = None;
                guard.durable.state = None;
            }
            Some((model, durable_selection)) => {
                let digest = catalog_package_digest(model);
                guard.durable.selected_model = Some(model.model_id.clone());
                if let Some(state) = durable_selection {
                    guard.durable.state = Some(state);
                } else if let Some(path) = existing_install_path(&self.root, model, &digest) {
                    guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
                        model_id: model.model_id.clone(),
                        revision: model.source.revision.clone(),
                        artifact_digest: digest,
                        install_path: path,
                    });
                } else {
                    guard.durable.state =
                        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
                            model_id: model.model_id.clone(),
                            revision: model.source.revision.clone(),
                            artifact_digest: digest,
                        });
                }
            }
        }
        persist_durable(&self.root, &guard.durable)?;
        drop(guard);
        Ok(self.status())
    }

    fn re_admit_durable_selection(
        &self,
        model: &CatalogedFastEmbedModelV1,
    ) -> Result<Option<SemanticModelLifecycleStateV1>, ModelLifecycleErrorV1> {
        let state = {
            let guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            guard.durable.state.clone()
        };
        let (was_ready, artifact_digest) = match state {
            Some(SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                ..
            }) if model_id == model.model_id && revision == model.source.revision => {
                (false, artifact_digest)
            }
            Some(SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                ..
            }) if model_id == model.model_id && revision == model.source.revision => {
                (true, artifact_digest)
            }
            _ => return Ok(None),
        };
        let digest = super::manifest::Sha256DigestHex::new(artifact_digest.clone())
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let environment = RuntimeEnvironmentV1::detect_fastembed_process()
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let lease_id = format!("active:{}:{}", model.model_id, model.source.revision);
        let admitted = self
            .artifact_store
            .admit_leased_for_runtime_by_digest(
                &digest,
                &environment,
                &lease_id,
                ArtifactLeaseKindV1::Active,
                current_unix_seconds()?,
            )
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        verify_catalog_manifest(model, admitted.manifest())?;
        let install_path = self.artifact_store.installed_directory(&digest);
        Ok(Some(if was_ready {
            SemanticModelLifecycleStateV1::Ready {
                model_id: model.model_id.clone(),
                revision: model.source.revision.clone(),
                artifact_digest,
                install_path,
            }
        } else {
            SemanticModelLifecycleStateV1::Installed {
                model_id: model.model_id.clone(),
                revision: model.source.revision.clone(),
                artifact_digest,
                install_path,
            }
        }))
    }

    /// Queue background acquisition when a selected model is not yet installed.
    pub fn enqueue_startup_acquisition_if_needed(&self) -> bool {
        let status = self.status();
        if !status.auto_download {
            return false;
        }
        let Some(state) = status.state else {
            return false;
        };
        if !matches!(
            state,
            SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. }
                | SemanticModelLifecycleStateV1::Failed {
                    retryable: true,
                    ..
                }
        ) {
            return false;
        }
        self.spawn_acquire()
    }

    pub fn retry(&self) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let status = self.status();
        if !status.remediation.retry {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        let model_id = status
            .selected_model
            .clone()
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        self.select_model(Some(&model_id), status.auto_download)?;
        let _ = self.spawn_acquire();
        Ok(self.status())
    }

    pub fn remove_install(&self) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let status = self.status();
        if !status.remediation.remove {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(state) = &status.state
            && let Ok(digest) =
                super::manifest::Sha256DigestHex::new(state.artifact_digest().to_owned())
        {
            let _ = self.artifact_store.release_artifact_lease(
                &digest,
                &format!("active:{}:{}", state.model_id(), state_revision(state)),
                ArtifactLeaseKindV1::Active,
            );
        }
        if let Some(state) = &status.state
            && let Some(path) = install_path_of(state)
        {
            if path.starts_with(self.root.join("verified-artifacts").join("artifacts")) {
                // Store bytes remain inventory-owned and become eligible only
                // under a later daemon GC lease.
            } else {
                let _ = fs::remove_dir_all(path);
            }
        }
        {
            let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            guard.durable.state = None;
            persist_durable(&self.root, &guard.durable)?;
        }
        let model_id = status.selected_model.clone();
        self.select_model(model_id.as_deref(), status.auto_download)
    }

    pub fn rollback_to_previous(
        &self,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let previous = guard
            .durable
            .previous_ready
            .clone()
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        if !matches!(previous, SemanticModelLifecycleStateV1::Ready { .. }) {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        if let Some(current) = &guard.durable.state
            && let Ok(digest) =
                super::manifest::Sha256DigestHex::new(current.artifact_digest().to_owned())
        {
            let _ = self.artifact_store.release_artifact_lease(
                &digest,
                &format!("active:{}:{}", current.model_id(), state_revision(current)),
                ArtifactLeaseKindV1::Active,
            );
        }
        if install_path_of(&previous).is_some_and(|path| {
            path.starts_with(self.root.join("verified-artifacts").join("artifacts"))
        }) && let Ok(digest) =
            super::manifest::Sha256DigestHex::new(previous.artifact_digest().to_owned())
        {
            self.artifact_store.acquire_artifact_lease(
                &digest,
                ArtifactLeaseV1 {
                    lease_id: format!(
                        "active:{}:{}",
                        previous.model_id(),
                        state_revision(&previous)
                    ),
                    kind: ArtifactLeaseKindV1::Active,
                    expires_at_unix: u64::MAX,
                },
                0,
            )?;
            let _ = self.artifact_store.release_artifact_lease(
                &digest,
                &format!("rollback:{}", previous.model_id()),
                ArtifactLeaseKindV1::Rollback,
            );
        }
        if let Some(SemanticModelLifecycleStateV1::Ready { .. }) = &guard.durable.state {
            let ready_state = guard.durable.state.clone();
            guard.durable.previous_ready = ready_state;
        }
        guard.durable.selected_model = Some(previous.model_id().to_owned());
        guard.durable.state = Some(previous);
        persist_durable(&self.root, &guard.durable)?;
        drop(guard);
        Ok(self.status())
    }

    pub fn mark_loading(&self) -> Result<(), ModelLifecycleErrorV1> {
        self.transition_installed_like(|model_id, revision, digest, path| {
            SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest: digest,
                install_path: path,
            }
        })
    }

    pub fn mark_indexing(
        &self,
        completed_units: u64,
        total_units: u64,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let (SemanticModelLifecycleStateV1::Installed {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }
        | SemanticModelLifecycleStateV1::Loading {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }) = state
        else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        if total_units == 0 || completed_units > total_units {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
            completed_units,
            total_units,
        });
        persist_durable(&self.root, &guard.durable)
    }

    pub fn mark_ready(&self) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let ready = match state {
            SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Indexing {
                model_id,
                revision,
                artifact_digest,
                install_path,
                ..
            } => SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                install_path,
            },
            SemanticModelLifecycleStateV1::Ready { .. } => state,
            _ => return Err(ModelLifecycleErrorV1::Rejected),
        };
        if let Some(previous) = guard.durable.state.clone()
            && matches!(previous, SemanticModelLifecycleStateV1::Ready { .. })
            && previous.artifact_digest() != ready.artifact_digest()
        {
            guard.durable.previous_ready = Some(previous);
        }
        guard.durable.state = Some(ready);
        persist_durable(&self.root, &guard.durable)
    }

    pub fn mark_runtime_failed(
        &self,
        detail: impl Into<String>,
        retryable: bool,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let (SemanticModelLifecycleStateV1::Installed {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Loading {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            revision,
            artifact_digest,
            ..
        }) = state
        else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
            model_id,
            revision,
            artifact_digest,
            detail: detail.into(),
            retryable,
        });
        persist_durable(&self.root, &guard.durable)
    }

    fn transition_installed_like(
        &self,
        build: impl FnOnce(String, String, String, PathBuf) -> SemanticModelLifecycleStateV1,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        if matches!(state, SemanticModelLifecycleStateV1::Ready { .. }) {
            guard.durable.previous_ready = Some(state.clone());
        }
        let next = match state {
            SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                install_path,
            } => build(model_id, revision, artifact_digest, install_path),
            _ => return Err(ModelLifecycleErrorV1::Rejected),
        };
        guard.durable.state = Some(next);
        persist_durable(&self.root, &guard.durable)
    }

    fn spawn_acquire(&self) -> bool {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return false;
        }
        self.cancel.store(false, Ordering::SeqCst);
        let root = self.root.clone();
        let catalog = self.catalog.clone();
        let source = Arc::clone(&self.source);
        let cancel = Arc::clone(&self.cancel);
        let inner = Arc::clone(&self.inner);
        let selected = {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
            guard.durable.selected_model.clone()
        };
        let Some(model_id) = selected else {
            return false;
        };
        let worker_root = root.clone();
        let worker_catalog = catalog.clone();
        let worker_model_id = model_id.clone();
        let worker_inner = Arc::clone(&inner);
        let handle = thread::Builder::new()
            .name("tracedecay-fastembed-acquire".to_owned())
            .spawn(move || {
                let _ = run_acquisition(
                    &worker_root,
                    &worker_catalog,
                    source.as_ref(),
                    &worker_model_id,
                    &cancel,
                    &worker_inner,
                );
            });
        match handle {
            Ok(join) => {
                *worker = Some(join);
                true
            }
            Err(error) => {
                if let Some(model) = catalog.get(&model_id) {
                    let _ = set_failed_state(
                        &root,
                        &inner,
                        model,
                        &catalog_package_digest(model),
                        &format!("cannot start background acquisition worker: {error}"),
                        true,
                    );
                }
                false
            }
        }
    }

    /// Synchronously acquire for tests and focused integration.
    pub fn acquire_blocking_for_tests(&self) -> Result<(), ModelLifecycleErrorV1> {
        let model_id = self
            .status()
            .selected_model
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        // Mirror `spawn_acquire`: clear any stale cancel flag (e.g. left set by a
        // prior `remove_install`) so a fresh blocking acquisition is not aborted
        // by the in-loop cancellation check.
        self.cancel.store(false, Ordering::SeqCst);
        run_acquisition(
            &self.root,
            &self.catalog,
            self.source.as_ref(),
            &model_id,
            &self.cancel,
            &self.inner,
        )
    }
}

fn current_unix_seconds() -> Result<u64, ModelLifecycleErrorV1> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)
}

fn run_acquisition(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
    source: &dyn ModelMemberSourceV1,
    model_id: &str,
    cancel: &AtomicBool,
    inner: &Mutex<LifecycleInner>,
) -> Result<(), ModelLifecycleErrorV1> {
    let result = run_acquisition_inner(root, catalog, source, model_id, cancel, inner);
    if let Err(error) = &result
        && let Some(model) = catalog.get(model_id)
    {
        let already_failed = {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
            matches!(
                guard.durable.state.as_ref(),
                Some(SemanticModelLifecycleStateV1::Failed { .. })
            )
        };
        if !already_failed {
            let retryable = matches!(
                error,
                ModelLifecycleErrorV1::StoreUnavailable
                    | ModelLifecycleErrorV1::DownloadFailed
                    | ModelLifecycleErrorV1::DownloadFailedWithReason(_)
                    | ModelLifecycleErrorV1::InstallFailed
            );
            let _ = set_failed_state(
                root,
                inner,
                model,
                &catalog_package_digest(model),
                &error.to_string(),
                retryable,
            );
        }
    }
    result
}

fn run_acquisition_inner(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
    source: &dyn ModelMemberSourceV1,
    model_id: &str,
    cancel: &AtomicBool,
    inner: &Mutex<LifecycleInner>,
) -> Result<(), ModelLifecycleErrorV1> {
    let model = catalog
        .get(model_id)
        .ok_or(CatalogErrorV1::UnknownModel)?
        .clone();
    let digest = catalog_package_digest(&model);
    let bytes_total: u64 = model.members.values().map(|member| member.length).sum();

    {
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.durable.selected_model = Some(model.model_id.clone());
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Downloading {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
            bytes_received: 0,
            bytes_total,
        });
        persist_durable(root, &guard.durable)?;
    }

    let staging = root.join("staging").join(format!(
        "{}-{}",
        model.model_id,
        &digest[..16.min(digest.len())]
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;

    let mut bytes_received = 0_u64;
    for member in model.members.values() {
        if cancel.load(Ordering::SeqCst) {
            return fail_state(root, inner, &model, &digest, "acquisition cancelled", true);
        }
        let destination = staging.join(&member.path);
        if let Err(error) = source.fetch_member(&model, &member.upstream_path, &destination) {
            return fail_state(root, inner, &model, &digest, &error.to_string(), true);
        }
        bytes_received = bytes_received.saturating_add(member.length);
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Downloading {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
            bytes_received,
            bytes_total,
        });
        persist_durable(root, &guard.durable)?;
    }

    {
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Verifying {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
        });
        persist_durable(root, &guard.durable)?;
    }

    for member in model.members.values() {
        let path = staging.join(&member.path);
        if !verify_member_file(&path, member.length, &member.sha256) {
            let _ = fs::remove_dir_all(&staging);
            return fail_state(
                root,
                inner,
                &model,
                &digest,
                "member length or sha256 mismatch",
                true,
            );
        }
    }

    let install_path = install_path_for(root, &model.model_id, &model.source.revision, &digest);
    if let Some(parent) = install_path.parent() {
        fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
    }
    if install_path.exists() {
        fs::remove_dir_all(&install_path).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
    }
    // Atomic publish: rename fully verified staging directory into place.
    fs::rename(&staging, &install_path).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
    let meta = InstallMetaV1 {
        schema: INSTALL_META_SCHEMA_V1.to_owned(),
        model_id: model.model_id.clone(),
        revision: model.source.revision.clone(),
        artifact_digest: digest.clone(),
    };
    write_json_atomic(&install_path.join("install.json"), &meta)
        .map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;

    let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
    guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
        model_id: model.model_id.clone(),
        revision: model.source.revision.clone(),
        artifact_digest: digest,
        install_path,
    });
    persist_durable(root, &guard.durable)
}

fn fail_state(
    root: &Path,
    inner: &Mutex<LifecycleInner>,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
    detail: &str,
    retryable: bool,
) -> Result<(), ModelLifecycleErrorV1> {
    set_failed_state(root, inner, model, digest, detail, retryable)?;
    Err(if retryable {
        ModelLifecycleErrorV1::DownloadFailed
    } else {
        ModelLifecycleErrorV1::VerificationFailed
    })
}

fn set_failed_state(
    root: &Path,
    inner: &Mutex<LifecycleInner>,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
    detail: &str,
    retryable: bool,
) -> Result<(), ModelLifecycleErrorV1> {
    let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
    guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
        model_id: model.model_id.clone(),
        revision: model.source.revision.clone(),
        artifact_digest: digest.to_owned(),
        detail: detail.to_owned(),
        retryable,
    });
    persist_durable(root, &guard.durable)
}

fn verify_catalog_manifest(
    model: &CatalogedFastEmbedModelV1,
    manifest: &ModelArtifactManifestV1,
) -> Result<(), ModelLifecycleErrorV1> {
    manifest
        .validate()
        .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
    if manifest.payload.artifact_id != model.model_id
        || manifest.payload.dimensions != model.expected_dimensions
        || manifest.payload.truncation.max_length != model.max_length
        || manifest.payload.spdx_license != model.source.license
        || manifest.payload.upstream.revision != model.source.revision
    {
        return Err(ModelLifecycleErrorV1::VerificationFailed);
    }
    for (role_name, catalog_member) in &model.members {
        let role = match role_name.as_str() {
            "model" => ArtifactMemberRoleV1::Model,
            "tokenizer" => ArtifactMemberRoleV1::Tokenizer,
            "config" => ArtifactMemberRoleV1::Config,
            "special_tokens_map" => ArtifactMemberRoleV1::SpecialTokensMap,
            "tokenizer_config" => ArtifactMemberRoleV1::TokenizerConfig,
            _ => return Err(ModelLifecycleErrorV1::VerificationFailed),
        };
        let member = manifest
            .package_member(role)
            .ok_or(ModelLifecycleErrorV1::VerificationFailed)?;
        if member.path != catalog_member.path
            || member.byte_length != catalog_member.length
            || member.digest.as_str() != catalog_member.sha256
        {
            return Err(ModelLifecycleErrorV1::VerificationFailed);
        }
    }
    Ok(())
}

fn load_or_default_durable(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
) -> Result<DurableLifecycleV1, ModelLifecycleErrorV1> {
    let path = root.join("lifecycle.json");
    if path.is_file() {
        let bytes = fs::read(&path).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        if let Ok(durable) = serde_json::from_slice::<DurableLifecycleV1>(&bytes)
            && durable.schema == LIFECYCLE_SCHEMA_V1
        {
            return Ok(durable);
        }
    }
    let model = catalog
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .ok_or(CatalogErrorV1::MissingDefault)?;
    let digest = catalog_package_digest(model);
    let state = if let Some(path) = existing_install_path(root, model, &digest) {
        Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
            install_path: path,
        })
    } else {
        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
        })
    };
    let durable = DurableLifecycleV1 {
        schema: LIFECYCLE_SCHEMA_V1.to_owned(),
        selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
        auto_download: false,
        state,
        previous_ready: None,
    };
    persist_durable(root, &durable)?;
    Ok(durable)
}

fn persist_durable(root: &Path, durable: &DurableLifecycleV1) -> Result<(), ModelLifecycleErrorV1> {
    write_json_atomic(&root.join("lifecycle.json"), durable)
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn install_path_for(root: &Path, model_id: &str, revision: &str, digest: &str) -> PathBuf {
    root.join("installs")
        .join(model_id)
        .join(revision)
        .join(&digest[..16.min(digest.len())])
}

fn existing_install_path(
    root: &Path,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
) -> Option<PathBuf> {
    let path = install_path_for(root, &model.model_id, &model.source.revision, digest);
    let meta_path = path.join("install.json");
    if !meta_path.is_file() {
        return None;
    }
    let bytes = fs::read(&meta_path).ok()?;
    let meta: InstallMetaV1 = serde_json::from_slice(&bytes).ok()?;
    if meta.schema != INSTALL_META_SCHEMA_V1
        || meta.model_id != model.model_id
        || meta.revision != model.source.revision
        || meta.artifact_digest != digest
    {
        return None;
    }
    for member in model.members.values() {
        if !verify_member_file(&path.join(&member.path), member.length, &member.sha256) {
            return None;
        }
    }
    Some(path)
}

fn install_path_of(state: &SemanticModelLifecycleStateV1) -> Option<&Path> {
    match state {
        SemanticModelLifecycleStateV1::Installed { install_path, .. }
        | SemanticModelLifecycleStateV1::Loading { install_path, .. }
        | SemanticModelLifecycleStateV1::Indexing { install_path, .. }
        | SemanticModelLifecycleStateV1::Ready { install_path, .. } => Some(install_path),
        _ => None,
    }
}

fn state_revision(state: &SemanticModelLifecycleStateV1) -> &str {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded { revision, .. }
        | SemanticModelLifecycleStateV1::Downloading { revision, .. }
        | SemanticModelLifecycleStateV1::Verifying { revision, .. }
        | SemanticModelLifecycleStateV1::Installed { revision, .. }
        | SemanticModelLifecycleStateV1::Loading { revision, .. }
        | SemanticModelLifecycleStateV1::Indexing { revision, .. }
        | SemanticModelLifecycleStateV1::Ready { revision, .. }
        | SemanticModelLifecycleStateV1::Failed { revision, .. } => revision,
    }
}

fn verify_member_file(path: &Path, length: u64, sha256: &str) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != length {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let Ok(read) = file.read(&mut buffer) else {
            return false;
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hex::encode(hasher.finalize()) == sha256
}

/// Process-wide lifecycle owner beneath the caller-resolved semantic-models
/// root. The root binary owns user-data-directory discovery and passes the
/// already-resolved root in; the first successful call wins for the process.
pub fn shared_lifecycle_owner(lifecycle_root: &Path) -> Option<Arc<SemanticModelLifecycleOwnerV1>> {
    SHARED_LIFECYCLE_OWNER
        .get_or_init(|| {
            SemanticModelLifecycleOwnerV1::open_default(lifecycle_root.to_path_buf())
                .ok()
                .map(Arc::new)
        })
        .clone()
}

/// Apply config selection and queue explicitly enabled background acquisition.
pub fn apply_config_and_queue_startup(
    lifecycle_root: &Path,
    selected_model: Option<&str>,
    auto_download: bool,
) -> Option<SemanticModelLifecycleStatusV1> {
    let owner = shared_lifecycle_owner(lifecycle_root)?;
    let _ = owner.select_model(selected_model, auto_download);
    let _ = owner.enqueue_startup_acquisition_if_needed();
    Some(owner.status())
}

#[cfg(all(test, feature = "semantic-fastembed"))]
#[path = "model_lifecycle/distribution_acquisition_acceptance.rs"]
mod distribution_acquisition_acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[cfg(feature = "semantic-fastembed")]
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    #[cfg(feature = "semantic-fastembed")]
    use std::time::{Duration, Instant};

    use super::super::manifest::{
        ArtifactMemberPinV1, ArtifactPackageMemberV1, ArtifactProfileKindV1, DeviceClassV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ModelArtifactManifestPayloadV1, PlatformTargetV1,
        ResourceCeilingV1, RuntimeCompatibilityV1, SemanticMetricV1, Sha256DigestHex,
        TruncationPolicyV1, TruncationSideV1, UpstreamSourceV1,
    };
    use super::super::model_catalog::{CatalogMemberPinV1, CatalogSourceV1};

    struct FixtureSource {
        root: PathBuf,
        calls: AtomicUsize,
    }

    impl ModelMemberSourceV1 for FixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let source = self.root.join(upstream_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            fs::copy(&source, destination).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            Ok(())
        }
    }

    struct BlockingFixtureSource {
        root: PathBuf,
        calls: AtomicUsize,
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl ModelMemberSourceV1 for BlockingFixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered
                    .send(())
                    .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
                self.release
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv()
                    .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            let source = self.root.join(upstream_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            fs::copy(source, destination)
                .map(|_| ())
                .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)
        }
    }

    #[cfg(feature = "semantic-fastembed")]
    struct FixtureHub {
        endpoint: String,
        requests: Arc<AtomicUsize>,
        worker: Option<JoinHandle<()>>,
    }

    #[cfg(feature = "semantic-fastembed")]
    impl FixtureHub {
        fn start(model: &CatalogedFastEmbedModelV1, fixture: &Path) -> Self {
            let members = model
                .members
                .values()
                .map(|member| {
                    (
                        member.upstream_path.clone(),
                        fs::read(fixture.join(&member.path)).unwrap(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let revision = model.source.revision.clone();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(AtomicUsize::new(0));
            let request_counter = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                let expected_requests = members.len() * 2;
                let deadline = Instant::now() + Duration::from_secs(5);
                while request_counter.load(Ordering::SeqCst) < expected_requests
                    && Instant::now() < deadline
                {
                    match listener.accept() {
                        Ok((mut stream, _)) => serve_fixture_hub_request(
                            &mut stream,
                            &members,
                            &revision,
                            &request_counter,
                        ),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture hub accept failed: {error}"),
                    }
                }
            });
            Self {
                endpoint,
                requests,
                worker: Some(worker),
            }
        }

        fn finish(mut self) -> usize {
            self.worker.take().unwrap().join().unwrap();
            self.requests.load(Ordering::SeqCst)
        }
    }

    #[cfg(feature = "semantic-fastembed")]
    fn serve_fixture_hub_request(
        stream: &mut TcpStream,
        members: &BTreeMap<String, Vec<u8>>,
        revision: &str,
        requests: &AtomicUsize,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::with_capacity(1024);
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        let request_line = request.lines().next().unwrap();
        let path = request_line.split_whitespace().nth(1).unwrap();
        let resolve_marker = format!("/resolve/{revision}/");
        let upstream_path = path.split_once(&resolve_marker).map_or_else(
            || panic!("unexpected fixture hub request path: {path}"),
            |(_, upstream_path)| upstream_path,
        );
        let body = members
            .get(upstream_path)
            .unwrap_or_else(|| panic!("unexpected fixture hub request path: {path}"));
        let metadata_request = request.to_ascii_lowercase().contains("range: bytes=0-0");
        let response_body = if metadata_request { &body[..1] } else { body };
        let end = if metadata_request { 0 } else { body.len() - 1 };
        let etag = hex::encode(Sha256::digest(body));
        write!(
            stream,
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: {}\r\n\
             Content-Range: bytes 0-{end}/{}\r\n\
             ETag: \"{etag}\"\r\n\
             X-Repo-Commit: {revision}\r\n\
             Connection: close\r\n\r\n",
            response_body.len(),
            body.len(),
        )
        .unwrap();
        stream.write_all(response_body).unwrap();
        stream.flush().unwrap();
        requests.fetch_add(1, Ordering::SeqCst);
    }

    fn join_background_acquisition(owner: &SemanticModelLifecycleOwnerV1) {
        owner
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("background acquisition worker")
            .join()
            .expect("background acquisition worker must not panic");
    }

    fn tiny_catalog(fixture: &Path) -> (FastEmbedModelCatalogV1, String) {
        let members_dir = fixture;
        fs::create_dir_all(members_dir).unwrap();
        let mut members = BTreeMap::new();
        for (role, name, bytes) in [
            ("model", "model.onnx", b"onnx-bytes".as_slice()),
            ("tokenizer", "tokenizer.json", br#"{"ok":true}"#.as_slice()),
            ("config", "config.json", br#"{"dim":8}"#.as_slice()),
            (
                "special_tokens_map",
                "special_tokens_map.json",
                br"{}".as_slice(),
            ),
            (
                "tokenizer_config",
                "tokenizer_config.json",
                br"{}".as_slice(),
            ),
        ] {
            let path = members_dir.join(name);
            fs::write(&path, bytes).unwrap();
            members.insert(
                role.to_owned(),
                CatalogMemberPinV1 {
                    path: name.to_owned(),
                    upstream_path: name.to_owned(),
                    length: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            );
        }
        let model = CatalogedFastEmbedModelV1 {
            model_id: "TinyFixtureModel".to_owned(),
            fastembed_enum: "TinyFixtureModel".to_owned(),
            model_code: "tracedecay/tiny-fixture".to_owned(),
            source: CatalogSourceV1 {
                upstream: "https://example.invalid/tracedecay/tiny-fixture".to_owned(),
                revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                license: "Apache-2.0".to_owned(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".to_owned(),
                provenance: "https://example.invalid/tracedecay/tiny-fixture/tree/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            },
            expected_dimensions: 8,
            max_length: 32,
            members,
        };
        // Production validate requires default Jina; for unit tests build a
        // catalog that includes both the default pin and the tiny fixture.
        let mut catalog = FastEmbedModelCatalogV1::production();
        catalog.models.push(model.clone());
        (catalog, model.model_id)
    }

    fn tiny_manifest(model: &CatalogedFastEmbedModelV1) -> ModelArtifactManifestV1 {
        let role = |name: &str| match name {
            "model" => ArtifactMemberRoleV1::Model,
            "tokenizer" => ArtifactMemberRoleV1::Tokenizer,
            "config" => ArtifactMemberRoleV1::Config,
            "special_tokens_map" => ArtifactMemberRoleV1::SpecialTokensMap,
            "tokenizer_config" => ArtifactMemberRoleV1::TokenizerConfig,
            _ => unreachable!(),
        };
        let members: Vec<_> = model
            .members
            .iter()
            .map(|(name, pin)| ArtifactPackageMemberV1 {
                role: role(name),
                path: pin.path.clone(),
                digest: Sha256DigestHex::new(pin.sha256.clone()).unwrap(),
                byte_length: pin.length,
            })
            .collect();
        let member = |role| members.iter().find(|member| member.role == role).unwrap();
        let model_member = member(ArtifactMemberRoleV1::Model);
        ModelArtifactManifestV1 {
            payload: ModelArtifactManifestPayloadV1 {
                schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
                artifact_id: model.model_id.clone(),
                profile_kind: ArtifactProfileKindV1::Embedding,
                spdx_license: model.source.license.clone(),
                model_member: ArtifactMemberPinV1 {
                    digest: model_member.digest.clone(),
                    byte_length: model_member.byte_length,
                },
                tokenizer_digest: member(ArtifactMemberRoleV1::Tokenizer).digest.clone(),
                config_digest: member(ArtifactMemberRoleV1::Config).digest.clone(),
                query_instruction_digest: None,
                document_instruction_digest: None,
                members,
                dimensions: model.expected_dimensions,
                metric: SemanticMetricV1::Cosine,
                normalization: EmbeddingNormalizationV1::L2,
                pooling: EmbeddingPoolingV1::Mean,
                truncation: TruncationPolicyV1 {
                    side: TruncationSideV1::Right,
                    max_length: model.max_length,
                },
                precision: EmbeddingPrecisionV1::Fp32,
                runtime: RuntimeCompatibilityV1 {
                    runtime: super::super::artifact_store::FASTEMBED_RUNTIME_FAMILY_V1.to_owned(),
                    build_revision:
                        super::super::artifact_store::FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned(),
                    platforms: vec![PlatformTargetV1 {
                        os: std::env::consts::OS.to_owned(),
                        arch: std::env::consts::ARCH.to_owned(),
                    }],
                },
                device: DeviceClassV1::Cpu,
                resource_ceiling: ResourceCeilingV1 {
                    max_model_bytes: 1_024,
                    max_tokenizer_bytes: 1_024,
                    max_resident_bytes: 4_096,
                    max_threads: 1,
                    max_batch_size: 1,
                    max_sequence_length: model.max_length,
                    load_deadline_ms: 1_000,
                },
                upstream: UpstreamSourceV1 {
                    name: model.model_code.clone(),
                    version: "fixture".to_owned(),
                    revision: model.source.revision.clone(),
                },
            },
        }
    }

    fn scoped_hub_source(root: &Path) -> Arc<dyn ModelMemberSourceV1> {
        Arc::new(HfHubModelMemberSourceV1::new(
            root.join(HF_HUB_CACHE_DIRECTORY_V1),
        ))
    }

    #[test]
    fn default_selection_is_selected_not_downloaded_and_offline_safe() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        let status = owner.status();
        assert_eq!(
            status.selected_model.as_deref(),
            Some(DEFAULT_FASTEMBED_MODEL_ID)
        );
        assert!(!status.auto_download);
        assert!(status.semantics_omitted);
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        assert!(status.remediation.retry);
        assert!(!owner.enqueue_startup_acquisition_if_needed());
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn fresh_hub_acquisition_downloads_then_reuses_private_cache_offline() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let online_root = tempfile::tempdir().unwrap();
        let hub = FixtureHub::start(&model, fixture.path());
        let cache = online_root.path().join(HF_HUB_CACHE_DIRECTORY_V1);
        let online_source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(
            cache.clone(),
            Some(hub.endpoint.clone()),
            false,
        ));
        let online =
            SemanticModelLifecycleOwnerV1::open(online_root.path(), catalog.clone(), online_source)
                .unwrap();

        online.select_model(Some(&model_id), true).unwrap();
        assert!(online.enqueue_startup_acquisition_if_needed());
        join_background_acquisition(&online);
        assert!(matches!(
            online.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert_eq!(hub.finish(), model.members.len() * 2);

        let offline_root = tempfile::tempdir().unwrap();
        let offline_source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(cache, None, true));
        let offline =
            SemanticModelLifecycleOwnerV1::open(offline_root.path(), catalog, offline_source)
                .unwrap();
        offline.select_model(Some(&model_id), true).unwrap();
        offline.acquire_blocking_for_tests().unwrap();

        assert!(matches!(
            offline.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn offline_cache_miss_reports_failed_reason_and_omits_semantics() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(
            root.path().join(HF_HUB_CACHE_DIRECTORY_V1),
            None,
            true,
        ));
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();

        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_startup_acquisition_if_needed());
        join_background_acquisition(&owner);

        let status = owner.status();
        let Some(SemanticModelLifecycleStateV1::Failed {
            detail, retryable, ..
        }) = status.state
        else {
            panic!("offline cache miss must report failed acquisition: {status:?}");
        };
        assert!(retryable);
        assert!(detail.contains("offline"));
        assert!(detail.contains("config.json"));
        assert!(status.semantics_omitted);
    }

    #[test]
    fn store_failure_does_not_leave_acquisition_stuck_in_progress() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        fs::remove_dir(root.path().join("staging")).unwrap();
        fs::write(root.path().join("staging"), b"not a directory").unwrap();

        assert_eq!(
            owner.acquire_blocking_for_tests().unwrap_err(),
            ModelLifecycleErrorV1::StoreUnavailable
        );
        let status = owner.status();
        let Some(SemanticModelLifecycleStateV1::Failed {
            detail, retryable, ..
        }) = status.state
        else {
            panic!("store failure must terminate acquisition state: {status:?}");
        };
        assert!(retryable);
        assert_eq!(detail, ModelLifecycleErrorV1::StoreUnavailable.to_string());
        assert!(status.semantics_omitted);
    }

    #[test]
    fn background_acquisition_does_not_block_startup_or_status_reads() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let source = Arc::new(BlockingFixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();

        assert!(owner.enqueue_startup_acquisition_if_needed());
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("background acquisition must start");
        let acquiring = owner.status();
        assert!(matches!(
            acquiring.state,
            Some(SemanticModelLifecycleStateV1::Downloading { .. })
        ));
        assert!(acquiring.semantics_omitted);

        release_tx.send(()).unwrap();
        join_background_acquisition(&owner);
        assert!(matches!(
            owner.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
    }

    #[test]
    fn explicit_local_import_is_verified_before_lifecycle_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let status = owner
            .import_local_artifact(&model_id, &tiny_manifest(&model), fixture.path(), 10)
            .unwrap();
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
    }

    #[test]
    fn restart_re_admits_explicit_import_without_legacy_acquisition() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let imported = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog.clone(),
            scoped_hub_source(root.path()),
        )
        .unwrap()
        .import_local_artifact(&model_id, &tiny_manifest(&model), fixture.path(), 10)
        .unwrap()
        .state
        .unwrap();

        let restarted = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let status = restarted.select_model(Some(&model_id), true).unwrap();

        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert_eq!(
            status.state.as_ref().unwrap().artifact_digest(),
            imported.artifact_digest()
        );
        assert!(!restarted.enqueue_startup_acquisition_if_needed());

        restarted.mark_ready().unwrap();
        let ready_restart = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            tiny_catalog(fixture.path()).0,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let ready = ready_restart.select_model(Some(&model_id), true).unwrap();
        assert!(matches!(
            ready.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
        assert!(!ready_restart.enqueue_startup_acquisition_if_needed());
    }

    #[test]
    fn daemon_artifact_gc_collects_only_unreferenced_installs_with_a_receipt() {
        const IMPORTED_AT: u64 = 10;
        const COLLECTED_AT: u64 = IMPORTED_AT + 7 * 24 * 60 * 60;

        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();

        let active_manifest = tiny_manifest(&model);
        let active = owner
            .import_local_artifact(&model_id, &active_manifest, fixture.path(), IMPORTED_AT)
            .unwrap()
            .state
            .unwrap();

        let mut rollback_manifest = active_manifest.clone();
        rollback_manifest.payload.artifact_id = "rollback-fixture".to_owned();
        let rollback = owner
            .artifact_store
            .import_local_directory(&rollback_manifest, fixture.path(), IMPORTED_AT)
            .unwrap();
        owner
            .artifact_store
            .acquire_artifact_lease(
                &rollback.artifact_digest,
                ArtifactLeaseV1 {
                    lease_id: "rollback-fixture".to_owned(),
                    kind: ArtifactLeaseKindV1::Rollback,
                    expires_at_unix: u64::MAX,
                },
                IMPORTED_AT,
            )
            .unwrap();

        let mut orphan_manifest = active_manifest;
        orphan_manifest.payload.artifact_id = "orphan-fixture".to_owned();
        let orphan = owner
            .artifact_store
            .import_local_directory(&orphan_manifest, fixture.path(), IMPORTED_AT)
            .unwrap();

        let receipts = owner.run_daemon_artifact_gc(COLLECTED_AT).unwrap();

        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.artifact_digest.clone())
                .collect::<Vec<_>>(),
            vec![orphan.artifact_digest.clone()]
        );
        let inventory = owner.artifact_store.inventory().unwrap();
        assert!(inventory.records.contains_key(active.artifact_digest()));
        assert!(
            inventory
                .records
                .contains_key(&rollback.artifact_digest.to_string())
        );
        assert!(
            !inventory
                .records
                .contains_key(&orphan.artifact_digest.to_string())
        );
        let receipt_log =
            fs::read_to_string(root.path().join("verified-artifacts/receipts/gc.jsonl")).unwrap();
        assert_eq!(receipt_log.lines().count(), 1);
    }

    fn reranker_manifest(
        model: &CatalogedFastEmbedModelV1,
        artifact_id: &str,
    ) -> ModelArtifactManifestV1 {
        let mut manifest = tiny_manifest(model);
        manifest.payload.artifact_id = artifact_id.to_owned();
        manifest.payload.profile_kind = ArtifactProfileKindV1::Reranker;
        manifest.payload.runtime.runtime =
            super::super::artifact_store::FASTEMBED_RUNTIME_FAMILY_V1.to_owned();
        manifest.payload.runtime.build_revision =
            super::super::artifact_store::FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned();
        manifest.payload.runtime.platforms = vec![PlatformTargetV1 {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }];
        manifest
    }

    fn reranker_pins(manifest: &ModelArtifactManifestV1) -> RerankCompatibilityPinsV1 {
        use tracedecay_domain::{ComponentRevision, ManifestDigest, canonical_sha256};

        RerankCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new(
                super::super::rerank_adapter::RERANK_IMPLEMENTATION_REVISION_V1,
            )
            .unwrap(),
            artifact_manifest_digest: ManifestDigest::new(format!(
                "sha256:{}",
                manifest.artifact_identity_digest()
            ))
            .unwrap(),
            runtime_compatibility_digest: canonical_sha256(&(
                super::super::rerank_adapter::RERANK_RUNTIME_DIGEST_DOMAIN_V1,
                &manifest.payload.runtime.runtime,
                &manifest.payload.runtime.build_revision,
                manifest.payload.device,
                manifest.payload.precision,
            ))
            .unwrap(),
        }
    }

    #[test]
    fn independent_reranker_import_rotates_active_and_rollback_leases() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let first = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let first_pins = reranker_pins(&first);
        let first_digest = first.artifact_identity_digest();

        let first_status = owner
            .import_local_reranker_artifact(first_pins.clone(), &first, fixture.path(), 10)
            .unwrap();
        assert_eq!(
            first_status.active_artifact_digest,
            Some(first_digest.clone())
        );
        assert_eq!(first_status.rollback_artifact_digest, None);
        assert!(owner.mount_reranker(first_pins.clone()).is_ok());

        let second = reranker_manifest(&model, "jinaai/jina-reranker-v1-turbo-en");
        let second_pins = reranker_pins(&second);
        let second_digest = second.artifact_identity_digest();
        let second_status = owner
            .import_local_reranker_artifact(second_pins.clone(), &second, fixture.path(), 11)
            .unwrap();
        assert_eq!(
            second_status.active_artifact_digest,
            Some(second_digest.clone())
        );
        assert_eq!(
            second_status.rollback_artifact_digest,
            Some(first_digest.clone())
        );
        assert!(owner.mount_reranker(first_pins).is_err());
        assert!(owner.mount_reranker(second_pins).is_ok());

        let rolled_back = owner.rollback_reranker_artifact(12).unwrap();
        assert_eq!(rolled_back.active_artifact_digest, Some(first_digest));
        assert_eq!(rolled_back.rollback_artifact_digest, Some(second_digest));
    }

    struct FixtureRerankerHttpsTransport {
        members: BTreeMap<String, Vec<u8>>,
        revision: String,
    }

    impl ExplicitHttpsArtifactTransportV1 for FixtureRerankerHttpsTransport {
        fn fetch_range(
            &self,
            request: &super::super::artifact_store::HttpsArtifactRangeRequestV1,
        ) -> Result<super::super::artifact_store::HttpsArtifactRangeResponseV1, ArtifactImportErrorV1>
        {
            let bytes = self
                .members
                .iter()
                .find_map(|(path, bytes)| {
                    request.url.ends_with(&format!("/{path}")).then_some(bytes)
                })
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let start = usize::try_from(request.offset)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let count = usize::try_from(request.max_bytes)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let end = start.saturating_add(count).min(bytes.len());
            Ok(super::super::artifact_store::HttpsArtifactRangeResponseV1 {
                offset: request.offset,
                total_length: bytes.len() as u64,
                immutable_revision: self.revision.clone(),
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    #[test]
    fn configured_https_reranker_acquisition_uses_immutable_member_pins() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let manifest = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let pins = reranker_pins(&manifest);
        let transport = FixtureRerankerHttpsTransport {
            members: manifest
                .payload
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.clone(),
                        fs::read(fixture.path().join(&member.path)).unwrap(),
                    )
                })
                .collect(),
            revision: "immutable-reranker-revision".to_owned(),
        };
        let source = ConfiguredHttpsArtifactSourceV1::new(
            "https://models.example.test/reranker",
            transport.revision.clone(),
        )
        .unwrap();

        let status = owner
            .import_configured_https_reranker_artifact(
                pins.clone(),
                &manifest,
                &source,
                &transport,
                None,
                20,
            )
            .unwrap();

        assert_eq!(
            status.active_artifact_digest,
            Some(manifest.artifact_identity_digest())
        );
        assert!(owner.mount_reranker(pins).is_ok());
    }

    #[test]
    fn reranker_import_rejects_unevaluated_pins_before_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let manifest = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let mut pins = reranker_pins(&manifest);
        pins.runtime_compatibility_digest =
            tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).unwrap();

        assert_eq!(
            owner
                .import_local_reranker_artifact(pins, &manifest, fixture.path(), 30)
                .unwrap_err(),
            ModelLifecycleErrorV1::VerificationFailed
        );
        assert_eq!(
            owner.reranker_artifact_status().unwrap(),
            RerankerArtifactLifecycleStatusV1 {
                active_artifact_digest: None,
                rollback_artifact_digest: None,
            }
        );
        assert!(
            !owner
                .artifact_store
                .inventory()
                .unwrap()
                .records
                .contains_key(&manifest.artifact_identity_digest().to_string())
        );
    }

    #[test]
    fn settings_change_schedules_acquire_to_installed_without_blocking_semantics_flag() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source.clone()).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.status().semantics_omitted);
        owner.acquire_blocking_for_tests().unwrap();
        let status = owner.status();
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert!(status.semantics_omitted);
        assert!(source.calls.load(Ordering::SeqCst) >= 5);
        owner.mark_loading().unwrap();
        owner.mark_indexing(1, 2).unwrap();
        owner.mark_ready().unwrap();
        let ready = owner.status();
        assert!(matches!(
            ready.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
        assert!(!ready.semantics_omitted);
    }

    #[test]
    fn runtime_failure_retains_ready_rollback_across_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog.clone(), source.clone())
                .unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        owner.mark_loading().unwrap();
        owner.mark_indexing(1, 2).unwrap();
        owner
            .mark_runtime_failed("projection failed", true)
            .unwrap();
        assert!(owner.status().remediation.rollback);
        drop(owner);

        let restarted = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        assert!(restarted.status().remediation.rollback);
        let rolled_back = restarted.rollback_to_previous().unwrap();
        assert!(matches!(
            rolled_back.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
    }

    #[test]
    fn retry_remove_and_rollback_remediation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        let removed = owner.remove_install().unwrap();
        assert!(matches!(
            removed.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        // Corrupt to Failed then retry.
        {
            let mut guard = owner.inner.lock().unwrap();
            if let Some(SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                ..
            }) = guard.durable.state.clone()
            {
                guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
                    model_id,
                    revision,
                    artifact_digest,
                    detail: "injected".to_owned(),
                    retryable: true,
                });
                persist_durable(&owner.root, &guard.durable).unwrap();
            }
        }
        let retried = owner.retry().unwrap();
        assert!(retried.remediation.retry || retried.state.is_some());
    }

    #[test]
    fn disabling_semantics_skips_startup_queue() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        owner.select_model(None, false).unwrap();
        assert!(!owner.enqueue_startup_acquisition_if_needed());
        assert!(owner.status().selected_model.is_none());
    }
}
