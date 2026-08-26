//! Root-owned seam onto the extracted semantic runtime crate.
//!
//! The implementation lives in `tracedecay-semantic`. Only the two contracts
//! that genuinely need the root binary stay here: user-data-directory
//! discovery (owned by `crate::config`) and the Doctor/status projection
//! (owned by `tracedecay_usecases::semantic_runtime`). Re-exports stay
//! `pub(crate)` so extraction does not widen the root's public API.

use std::path::PathBuf;
use std::sync::Arc;

pub(crate) use tracedecay_semantic::*;

use tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1 as Runtime;

/// Resolve the lifecycle store root under the user data directory.
pub(crate) fn default_lifecycle_root() -> Option<PathBuf> {
    crate::config::user_data_dir().map(|root| tracedecay_semantic::default_lifecycle_root_in(&root))
}

/// Process-wide lifecycle owner under the user semantic-models root.
pub(crate) fn shared_lifecycle_owner() -> Option<Arc<SemanticModelLifecycleOwnerV1>> {
    tracedecay_semantic::shared_lifecycle_owner(&default_lifecycle_root()?)
}

/// Apply config selection and queue explicitly enabled background acquisition.
pub(crate) fn apply_config_and_queue_startup(
    selected_model: Option<&str>,
    auto_download: bool,
) -> Option<tracedecay_semantic::SemanticModelLifecycleStatusV1> {
    tracedecay_semantic::apply_config_and_queue_startup(
        &default_lifecycle_root()?,
        selected_model,
        auto_download,
    )
}

/// Map lifecycle state into the Doctor/status semantic runtime state surface.
pub(crate) fn lifecycle_to_runtime_state(state: &SemanticModelLifecycleStateV1) -> Runtime {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded {
            model_id,
            artifact_digest,
            ..
        } => Runtime::SelectedNotDownloaded {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        SemanticModelLifecycleStateV1::Downloading {
            model_id,
            artifact_digest,
            bytes_received,
            bytes_total,
            ..
        } => Runtime::Downloading {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
            bytes_received: *bytes_received,
            bytes_total: *bytes_total,
        },
        SemanticModelLifecycleStateV1::Verifying {
            model_id,
            artifact_digest,
            ..
        } => Runtime::Verifying {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        // Lifecycle Ready means the package is locally complete. Semantic
        // search influence still requires a Current activation receipt.
        SemanticModelLifecycleStateV1::Installed {
            model_id,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            artifact_digest,
            ..
        } => Runtime::Installed {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        // Vector-generation Indexing requires a configuration pin +
        // generation id. Acquisition-phase indexing is reported as Loading
        // until the semantic owner publishes an activation receipt.
        SemanticModelLifecycleStateV1::Loading {
            model_id,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            artifact_digest,
            ..
        } => Runtime::Loading {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        SemanticModelLifecycleStateV1::Failed {
            model_id,
            artifact_digest,
            detail,
            retryable,
            ..
        } => Runtime::Failed {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
            detail: detail.clone(),
            retryable: *retryable,
        },
    }
}
