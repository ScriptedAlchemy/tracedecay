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
static USER_DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub fn lock_user_data_dir_test_env() -> std::sync::MutexGuard<'static, ()> {
    USER_DATA_DIR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub struct PinnedUserDataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    _root: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    previous_home: Option<std::ffi::OsString>,
    previous_userprofile: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl PinnedUserDataDir {
    pub fn new() -> Self {
        let lock = lock_user_data_dir_test_env();
        let root = tempfile::TempDir::new()
            .unwrap_or_else(|err| panic!("failed to create temp profile dir: {err}"));
        let profile = root.path().join(TRACEDECAY_DIR);
        std::fs::create_dir_all(&profile)
            .unwrap_or_else(|err| panic!("failed to create isolated profile root: {err}"));
        let previous = std::env::var_os(USER_DATA_DIR_ENV);
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var(USER_DATA_DIR_ENV, &profile);
            std::env::set_var("HOME", root.path());
            std::env::set_var("USERPROFILE", root.path());
        }
        Self {
            _lock: lock,
            _root: root,
            previous,
            previous_home,
            previous_userprofile,
        }
    }
}

#[cfg(test)]
impl Default for PinnedUserDataDir {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Drop for PinnedUserDataDir {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(USER_DATA_DIR_ENV, previous),
                None => std::env::remove_var(USER_DATA_DIR_ENV),
            }
            match self.previous_home.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(previous) => std::env::set_var("USERPROFILE", previous),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
