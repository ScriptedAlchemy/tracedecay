//! Public configuration-surface contracts for explicit semantic artifact control.
//!
//! The semantic crate retains storage and runtime ownership. These request and
//! redacted result types are the one transport-neutral lifecycle projection;
//! root composition maps every owner status exhaustively into this module.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::configuration::ConfigurationIdempotencyKey;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticLifecycleControlRequestV1 {
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfiguredHttpsSourceRequestV1 {
    pub base_url: String,
    pub immutable_revision: String,
    #[serde(default)]
    pub resume_staging_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankerCompatibilityPinsRequestV1 {
    pub implementation_revision: String,
    pub artifact_manifest_digest: ManifestDigest,
    pub runtime_compatibility_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticEmbeddingLocalImportRequestV1 {
    pub model_id: String,
    pub manifest_canonical_json: String,
    pub source_directory: PathBuf,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticEmbeddingConfiguredHttpsImportRequestV1 {
    pub model_id: String,
    pub manifest_canonical_json: String,
    pub source: SemanticConfiguredHttpsSourceRequestV1,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankerLocalImportRequestV1 {
    pub pins: SemanticRerankerCompatibilityPinsRequestV1,
    pub manifest_canonical_json: String,
    pub source_directory: PathBuf,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankerConfiguredHttpsImportRequestV1 {
    pub pins: SemanticRerankerCompatibilityPinsRequestV1,
    pub manifest_canonical_json: String,
    pub source: SemanticConfiguredHttpsSourceRequestV1,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankerRollbackRequestV1 {
    /// Exact accepted-profile pins for the retained rollback artifact. The
    /// owner reopens and validates the target before rotating either lease.
    pub pins: SemanticRerankerCompatibilityPinsRequestV1,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

/// Canonical public, redacted lifecycle state for one model control result.
/// Root composition exhaustively projects the semantic owner's state into this
/// contract; it has no independent transitions, defaults, or persistence and
/// deliberately excludes private filesystem paths and source handles.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SemanticEmbeddingLifecycleStateV1 {
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
    },
    Loading {
        model_id: String,
        revision: String,
        artifact_digest: String,
    },
    Indexing {
        model_id: String,
        revision: String,
        artifact_digest: String,
        completed_units: u64,
        total_units: u64,
    },
    Ready {
        model_id: String,
        revision: String,
        artifact_digest: String,
    },
    Failed {
        model_id: String,
        revision: String,
        artifact_digest: String,
        retryable: bool,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticModelRemediationV1 {
    pub retry: bool,
    pub remove: bool,
    pub rollback: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticEmbeddingLifecycleStatusV1 {
    pub selected_model: Option<String>,
    pub auto_download: bool,
    pub catalog_model_ids: Vec<String>,
    pub state: Option<SemanticEmbeddingLifecycleStateV1>,
    pub remediation: SemanticModelRemediationV1,
    pub semantics_omitted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankerLifecycleStatusV1 {
    pub active_artifact_digest: Option<String>,
    pub rollback_artifact_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    tag = "artifact_profile",
    content = "status"
)]
pub enum SemanticLifecycleOperationResultV1 {
    Embedding(SemanticEmbeddingLifecycleStatusV1),
    Reranker(SemanticRerankerLifecycleStatusV1),
    /// A configured immutable HTTPS transfer stopped before verification.
    /// The opaque store-owned handle is the only value accepted by the next
    /// explicit resume request; no source URL, path, or downloaded bytes are
    /// exposed through the result.
    ImportInterrupted {
        resume_staging_id: String,
    },
}
