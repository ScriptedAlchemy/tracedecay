//! Compatibility façade for runtime configuration.

pub use tracedecay_runtime_core::config::*;

pub async fn get_config_path_with_identity(project_root: &std::path::Path) -> std::path::PathBuf {
    if let Ok(layout) =
        crate::tracedecay::TraceDecay::resolve_store_layout_for_identity(project_root).await
    {
        return layout.config_path;
    }
    get_config_path(project_root)
}

pub async fn load_config_with_identity(
    project_root: &std::path::Path,
) -> crate::errors::Result<TraceDecayConfig> {
    let config_path = get_config_path_with_identity(project_root).await;
    load_config_from_path(project_root, &config_path)
}

pub async fn save_config_with_identity(
    project_root: &std::path::Path,
    config: &TraceDecayConfig,
) -> crate::errors::Result<()> {
    let config_path = get_config_path_with_identity(project_root).await;
    save_config_to_path(&config_path, config)
}

pub async fn discover_project_root_with_identity(
    start: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if let Some(root) = discover_project_root(start) {
        return Some(root);
    }
    let candidate =
        crate::worktree::git_worktree_root(start).unwrap_or_else(|| start.to_path_buf());
    crate::tracedecay::TraceDecay::has_initialized_store(&candidate)
        .await
        .then_some(candidate)
}

#[cfg(test)]
mod tests;
