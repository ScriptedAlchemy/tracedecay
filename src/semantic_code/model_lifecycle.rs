//! Daemon-owned FastEmbed model acquisition lifecycle.
//!
//! Settings select a cataloged model (default [`DEFAULT_FASTEMBED_MODEL_ID`]).
//! Install stays offline-safe: first daemon startup (or a settings change)
//! queues bounded background download via maintained hf-hub/FastEmbed
//! capability, verifies immutable length+SHA-256 pins, and atomically installs
//! into a TraceDecay-owned store. Search never downloads or waits.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::model_catalog::{
    CatalogErrorV1, CatalogedFastEmbedModelV1, DEFAULT_FASTEMBED_MODEL_ID,
    FastEmbedModelCatalogV1, catalog_package_digest,
};

const LIFECYCLE_SCHEMA_V1: &str = "tracedecay.fastembed.model-lifecycle.v1";
const INSTALL_META_SCHEMA_V1: &str = "tracedecay.fastembed.model-install.v1";

/// Doctor/status lifecycle states for the selected FastEmbed model.
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
            Self::SelectedNotDownloaded { artifact_digest, .. }
            | Self::Downloading { artifact_digest, .. }
            | Self::Verifying { artifact_digest, .. }
            | Self::Installed { artifact_digest, .. }
            | Self::Loading { artifact_digest, .. }
            | Self::Indexing { artifact_digest, .. }
            | Self::Ready { artifact_digest, .. }
            | Self::Failed { artifact_digest, .. } => artifact_digest,
        }
    }

    /// Semantics are omitted while acquisition/load/index is incomplete.
    pub fn omits_semantics(&self) -> bool {
        !matches!(self, Self::Ready { .. })
    }

    pub fn remediation(&self) -> SemanticModelRemediationV1 {
        match self {
            Self::Failed { retryable: true, .. } | Self::SelectedNotDownloaded { .. } => {
                SemanticModelRemediationV1 {
                    retry: true,
                    remove: matches!(self, Self::Failed { .. }),
                    rollback: false,
                }
            }
            Self::Installed { .. }
            | Self::Loading { .. }
            | Self::Indexing { .. }
            | Self::Ready { .. }
            | Self::Failed { retryable: false, .. } => SemanticModelRemediationV1 {
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
    #[error("semantic model verification failed")]
    VerificationFailed,
    #[error("semantic model install failed")]
    InstallFailed,
}

/// Supplies package member bytes for a cataloged model.
///
/// Production uses hf-hub/FastEmbed; tests inject local fixture bytes.
pub trait ModelMemberSourceV1: Send + Sync {
    fn fetch_member(
        &self,
        model: &CatalogedFastEmbedModelV1,
        upstream_path: &str,
        destination: &Path,
    ) -> Result<(), ModelLifecycleErrorV1>;
}

/// hf-hub backed source that downloads into a caller-owned staging cache.
#[derive(Debug, Default)]
pub struct HfHubModelMemberSourceV1;

impl ModelMemberSourceV1 for HfHubModelMemberSourceV1 {
    fn fetch_member(
        &self,
        model: &CatalogedFastEmbedModelV1,
        upstream_path: &str,
        destination: &Path,
    ) -> Result<(), ModelLifecycleErrorV1> {
        fetch_member_with_hf_hub(model, upstream_path, destination)
    }
}

fn fetch_member_with_hf_hub(
    model: &CatalogedFastEmbedModelV1,
    upstream_path: &str,
    destination: &Path,
) -> Result<(), ModelLifecycleErrorV1> {
    #[cfg(feature = "semantic-fastembed")]
    {
        use hf_hub::api::sync::ApiBuilder;
        use hf_hub::{Repo, RepoType};

        let cache_dir = destination
            .parent()
            .ok_or(ModelLifecycleErrorV1::DownloadFailed)?
            .join(".hf-cache");
        fs::create_dir_all(&cache_dir).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
        let api = ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .with_progress(false)
            .build()
            .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
        let repo = api.repo(Repo::with_revision(
            model.model_code.clone(),
            RepoType::Model,
            model.source.revision.clone(),
        ));
        let fetched = repo
            .get(upstream_path)
            .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
        }
        fs::copy(&fetched, destination).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
        Ok(())
    }
    #[cfg(not(feature = "semantic-fastembed"))]
    {
        let _ = (model, upstream_path, destination);
        Err(ModelLifecycleErrorV1::DownloadFailed)
    }
}

/// Owns selection, background acquisition, and remediation for one data root.
pub struct SemanticModelLifecycleOwnerV1 {
    root: PathBuf,
    catalog: FastEmbedModelCatalogV1,
    source: Arc<dyn ModelMemberSourceV1>,
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
        fs::create_dir_all(root.join("staging")).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        fs::create_dir_all(root.join("installs"))
            .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        let durable = load_or_default_durable(&root, &catalog)?;
        Ok(Self {
            root,
            catalog,
            source,
            inner: Arc::new(Mutex::new(LifecycleInner { durable })),
            worker: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn open_default(root: impl Into<PathBuf>) -> Result<Self, ModelLifecycleErrorV1> {
        Self::open(
            root,
            FastEmbedModelCatalogV1::production(),
            Arc::new(HfHubModelMemberSourceV1),
        )
    }

    pub fn catalog(&self) -> &FastEmbedModelCatalogV1 {
        &self.catalog
    }

    pub fn status(&self) -> SemanticModelLifecycleStatusV1 {
        let guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let remediation = guard
            .durable
            .state
            .as_ref()
            .map(SemanticModelLifecycleStateV1::remediation)
            .unwrap_or(SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            });
        let semantics_omitted = guard
            .durable
            .state
            .as_ref()
            .map(SemanticModelLifecycleStateV1::omits_semantics)
            .unwrap_or(true);
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
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        guard.durable.auto_download = auto_download;
        match model_id {
            None => {
                guard.durable.selected_model = None;
                guard.durable.state = None;
            }
            Some(model_id) => {
                let model = self
                    .catalog
                    .get(model_id)
                    .ok_or(CatalogErrorV1::UnknownModel)?;
                let digest = catalog_package_digest(model);
                guard.durable.selected_model = Some(model.model_id.clone());
                if let Some(path) = existing_install_path(&self.root, model, &digest) {
                    guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
                        model_id: model.model_id.clone(),
                        revision: model.source.revision.clone(),
                        artifact_digest: digest,
                        install_path: path,
                    });
                } else {
                    guard.durable.state = Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
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
        if let Some(state) = &status.state {
            if let Some(path) = install_path_of(state) {
                let _ = fs::remove_dir_all(path);
            }
        }
        let model_id = status.selected_model.clone();
        self.select_model(model_id.as_deref(), status.auto_download)
    }

    pub fn rollback_to_previous(
        &self,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let previous = guard
            .durable
            .previous_ready
            .clone()
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        if !matches!(previous, SemanticModelLifecycleStateV1::Ready { .. }) {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        if let Some(SemanticModelLifecycleStateV1::Ready { .. }) = &guard.durable.state {
            guard.durable.previous_ready = guard.durable.state.clone();
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
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let (model_id, revision, digest, install_path) = match state {
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
            }
            | SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                install_path,
            } => (model_id, revision, artifact_digest, install_path),
            _ => return Err(ModelLifecycleErrorV1::Rejected),
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
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
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

    fn transition_installed_like(
        &self,
        build: impl FnOnce(String, String, String, PathBuf) -> SemanticModelLifecycleStateV1,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
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
        let mut worker = self.worker.lock().unwrap_or_else(|error| error.into_inner());
        if worker
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return false;
        }
        self.cancel.store(false, Ordering::SeqCst);
        let root = self.root.clone();
        let catalog = self.catalog.clone();
        let source = Arc::clone(&self.source);
        let cancel = Arc::clone(&self.cancel);
        let inner = Arc::clone(&self.inner);
        let selected = {
            let guard = inner.lock().unwrap_or_else(|error| error.into_inner());
            guard.durable.selected_model.clone()
        };
        let Some(model_id) = selected else {
            return false;
        };
        let handle = thread::Builder::new()
            .name("tracedecay-fastembed-acquire".to_owned())
            .spawn(move || {
                let _ = run_acquisition(
                    &root,
                    &catalog,
                    source.as_ref(),
                    &model_id,
                    &cancel,
                    &inner,
                );
            });
        match handle {
            Ok(join) => {
                *worker = Some(join);
                true
            }
            Err(_) => false,
        }
    }

    /// Synchronously acquire for tests and focused integration.
    pub fn acquire_blocking_for_tests(&self) -> Result<(), ModelLifecycleErrorV1> {
        let model_id = self
            .status()
            .selected_model
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
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

fn run_acquisition(
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
        let mut guard = inner.lock().unwrap_or_else(|error| error.into_inner());
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
            return fail_state(
                root,
                inner,
                &model,
                &digest,
                "acquisition cancelled",
                true,
            );
        }
        let destination = staging.join(&member.path);
        if let Err(error) = source.fetch_member(&model, &member.upstream_path, &destination) {
            return fail_state(
                root,
                inner,
                &model,
                &digest,
                &error.to_string(),
                true,
            );
        }
        bytes_received = bytes_received.saturating_add(member.length);
        let mut guard = inner.lock().unwrap_or_else(|error| error.into_inner());
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
        let mut guard = inner.lock().unwrap_or_else(|error| error.into_inner());
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

    let mut guard = inner.lock().unwrap_or_else(|error| error.into_inner());
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
    let mut guard = inner.lock().unwrap_or_else(|error| error.into_inner());
    guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
        model_id: model.model_id.clone(),
        revision: model.source.revision.clone(),
        artifact_digest: digest.to_owned(),
        detail: detail.to_owned(),
        retryable,
    });
    persist_durable(root, &guard.durable)?;
    Err(if retryable {
        ModelLifecycleErrorV1::DownloadFailed
    } else {
        ModelLifecycleErrorV1::VerificationFailed
    })
}

fn load_or_default_durable(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
) -> Result<DurableLifecycleV1, ModelLifecycleErrorV1> {
    let path = root.join("lifecycle.json");
    if path.is_file() {
        let bytes = fs::read(&path).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        if let Ok(durable) = serde_json::from_slice::<DurableLifecycleV1>(&bytes) {
            if durable.schema == LIFECYCLE_SCHEMA_V1 {
                return Ok(durable);
            }
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
        auto_download: true,
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
    let mut buffer = [0_u8; 1024 * 1024];
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

/// Resolve the lifecycle store root under the user data directory.
pub fn default_lifecycle_root() -> Option<PathBuf> {
    crate::config::user_data_dir().map(|root| root.join("semantic-models"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

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
                br#"{}"#.as_slice(),
            ),
            (
                "tokenizer_config",
                "tokenizer_config.json",
                br#"{}"#.as_slice(),
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

    #[test]
    fn default_selection_is_selected_not_downloaded_and_offline_safe() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        let status = owner.status();
        assert_eq!(
            status.selected_model.as_deref(),
            Some(DEFAULT_FASTEMBED_MODEL_ID)
        );
        assert!(status.auto_download);
        assert!(status.semantics_omitted);
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        assert!(status.remediation.retry);
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
    fn retry_remove_and_rollback_remediation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
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
