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
fn apply_config_selection_to_owner(
    owner: &SemanticModelLifecycleOwnerV1,
    selected_model: Option<&str>,
    auto_download: bool,
) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
    owner.select_model(selected_model, auto_download)
}

/// Apply the configured selection without fetching model bytes.
///
/// `auto_download` authorizes a later demand-triggered acquisition; project
/// open itself only publishes the selected lifecycle state.
pub fn apply_config_selection(
    lifecycle_root: &Path,
    selected_model: Option<&str>,
    auto_download: bool,
) -> Option<SemanticModelLifecycleStatusV1> {
    let owner = shared_lifecycle_owner(lifecycle_root)?;
    apply_config_selection_to_owner(&owner, selected_model, auto_download).ok()
}
