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
