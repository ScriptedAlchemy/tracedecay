//! Process-wide registry of per-project semantic runtime handles and status reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, VectorGenerationIdV1};
use tracedecay_semantic::DaemonSemanticRuntimeHandleV1;
use tracedecay_semantic_contracts::{SemanticGenerationPointerV1, SemanticModelLifecycleStatusV1};

use super::super::ports::{SemanticConfigurationPinV1, SemanticRuntimeStatusV1};
use super::ProductionSemanticRuntimeV1;
use super::application_status::{
    application_status_from_projection, prefer_lifecycle_over_generic_unavailable,
    resolve_semantic_application_status,
};
use super::daemon_backend::DaemonSemanticRuntimeBackendV1;

/// Process-local registry so Doctor/`tracedecay_runtime` can observe the
/// daemon-private scheduler without a wire operation.
pub(super) fn project_semantic_handles()
-> &'static Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>> {
    static HANDLES: OnceLock<Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>>> =
        OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn project_semantic_production_runtimes()
-> &'static Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>> {
    static RUNTIMES: OnceLock<Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>>> =
        OnceLock::new();
    RUNTIMES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain a project semantic handle for status/search composition.
pub fn register_project_semantic_runtime(
    project_root: PathBuf,
    handle: DaemonSemanticRuntimeHandleV1,
) {
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, handle);
}

/// Everything a project's semantic unregistration released from the
/// process-local registries, handed back so the caller decides where it is
/// freed. The retained code generations and the query runtime cache are
/// generation-sized; dropping them inside the registry locks (or inline on
/// the daemon shutdown drain) is what held shutdown past its TERM grace.
#[must_use = "drop this off the registry locks; it owns generation-sized memory"]
pub struct RetiredProjectSemanticRuntimeV1 {
    _generations: Vec<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>>,
    _handle: Option<DaemonSemanticRuntimeHandleV1>,
    _runtime: Option<ProductionSemanticRuntimeV1>,
}

/// Remove a project's semantic handle, runtime, and redundancy state from the
/// process-local registries. The removed owners are returned, not dropped.
pub fn unregister_project_semantic_runtime(project_root: &Path) -> RetiredProjectSemanticRuntimeV1 {
    let generations = super::super::unregister_project_semantic_redundancy_generation(project_root);
    let handle = project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    let runtime = project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    RetiredProjectSemanticRuntimeV1 {
        _generations: generations,
        _handle: handle,
        _runtime: runtime,
    }
}

pub fn project_semantic_production_runtime(
    project_root: &Path,
) -> Option<ProductionSemanticRuntimeV1> {
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

pub fn unbind_project_semantic_cache_if_current(
    project_root: &Path,
    generation: &VectorGenerationIdV1,
) -> bool {
    if let Some(runtime) = project_semantic_production_runtime(project_root) {
        return runtime.unbind_cache_if_current(generation);
    }
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .is_some_and(|handle| handle.unbind_query_runtime_if_current(generation))
}

/// Source generation observed in the cache for the committed semantic pins.
///
/// Query adapters compare this identity with the exact sealed code generation
/// selected at admission. A stale or merely indexing vector generation never
/// becomes eligible for semantic composition.
pub fn project_semantic_source_generation(project_root: &Path) -> Option<CodeGenerationId> {
    project_semantic_generation_pointer(project_root).map(|pointer| pointer.source_generation)
}

pub(crate) fn project_semantic_generation_pointer(
    project_root: &Path,
) -> Option<SemanticGenerationPointerV1> {
    let pins = super::super::project_committed_semantic_pins(project_root)?;
    let observed = if let Some(runtime) = project_semantic_production_runtime(project_root) {
        runtime.handle.current()
    } else {
        project_semantic_handles()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(DaemonSemanticRuntimeHandleV1::current)
    }?;
    (observed.generation == pins.vector_generation_id
        && observed.projection_key == *pins.projection.projection_key())
    .then_some(observed)
}

/// Application status for a mounted project semantic scheduler, if any.
///
/// The durable activation receipt and the scheduler status projection are
/// read under one acquisition of the project activation gate. A concurrent
/// activation mutates the receipt and the installed authorities under that
/// same gate, so status can never pair a stale receipt with a newer scheduler
/// generation (reporting `Current` while semantic is unavailable) or the
/// inverse (reporting degraded after a coherent install).
pub fn project_semantic_application_status(
    project_root: &Path,
    configuration: Option<SemanticConfigurationPinV1>,
) -> Option<SemanticRuntimeStatusV1> {
    super::super::with_project_semantic_activation_receipt(project_root, |activation_receipt| {
        if let Some(runtime) = project_semantic_production_runtime(project_root) {
            let lifecycle = runtime.lifecycle_status();
            let backend = DaemonSemanticRuntimeBackendV1::from_production(runtime);
            if let Some(configuration) = configuration {
                backend.bind_configuration(configuration);
            }
            return Some(prefer_lifecycle_over_generic_unavailable(
                backend.application_status_with_receipt(activation_receipt),
                &lifecycle,
            ));
        }
        let handle = project_semantic_handles()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()?;
        Some(application_status_from_projection(
            &handle.status_projection(),
            configuration,
            activation_receipt,
        ))
    })
}

/// Doctor/MCP status for a seated or unseated project.
///
/// Seated scheduler views that only report generic unavailability yield to
/// the model-lifecycle owner. A mounted-but-broken runtime keeps its error.
pub fn resolve_project_semantic_runtime_status(
    project_path: Option<&Path>,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let scheduler = project_path
        .and_then(|path| project_semantic_application_status(path, configuration.clone()));
    let lifecycle = match project_path {
        Some(path) => project_lifecycle_status(path),
        None => None,
    };
    resolve_semantic_application_status(scheduler, lifecycle.as_ref(), configuration)
}

pub fn project_lifecycle_status(project_path: &Path) -> Option<SemanticModelLifecycleStatusV1> {
    if let Some(runtime) = project_semantic_production_runtime(project_path) {
        return Some(runtime.lifecycle_status());
    }
    None
}
