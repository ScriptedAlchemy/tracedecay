use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::manifest::Sha256DigestHex;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticLifecycleVerifiedReadyEventV1 {
    pub epoch: u64,
    pub artifact_digest: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticModelRemediationV1 {
    pub retry: bool,
    pub remove: bool,
    pub rollback: bool,
}

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
    pub active_artifact_digest: Option<Sha256DigestHex>,
    pub rollback_artifact_digest: Option<Sha256DigestHex>,
}
