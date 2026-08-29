//! Root-owned seam onto the extracted semantic runtime crate.
//!
//! The implementation lives in `tracedecay-semantic`. Only the two contracts
//! that genuinely need the root binary stay here: user-data-directory
//! discovery (owned by `crate::config`) and the Doctor/status projection
//! (owned by `tracedecay_usecases::semantic_runtime`). Re-exports stay
//! `pub(crate)` so extraction does not widen the root's public API.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use tracedecay_semantic::*;

use tracedecay_usecases::semantic_runtime::{SemanticConfigurationPinV1, SemanticRuntimeStatusV1};

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

/// Doctor/MCP status for a seated or unseated project.
///
/// Seated scheduler views that only report generic unavailability yield to
/// the model-lifecycle owner. A mounted-but-broken runtime keeps its error.
pub(crate) fn resolve_project_semantic_runtime_status(
    project_path: Option<&Path>,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let scheduler = project_path.and_then(|path| {
        tracedecay_usecases::semantic_runtime::project_semantic_application_status(
            path,
            configuration.clone(),
        )
    });
    let lifecycle = match project_path {
        Some(path) => project_or_shared_lifecycle_status(path),
        None if configuration.is_none() => None,
        None => shared_lifecycle_owner().map(|owner| owner.status()),
    };
    tracedecay_usecases::semantic_runtime::resolve_semantic_application_status(
        scheduler,
        lifecycle.as_ref(),
        configuration,
    )
}

fn project_or_shared_lifecycle_status(
    project_path: &Path,
) -> Option<SemanticModelLifecycleStatusV1> {
    if let Some(runtime) =
        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(project_path)
    {
        return Some(runtime.lifecycle_status());
    }
    shared_lifecycle_owner().map(|owner| owner.status())
}
