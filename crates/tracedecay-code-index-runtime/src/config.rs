//! Configuration surfaces this crate needs without depending on the root
//! `crate::config` module (another effort is splitting that module).
//!
//! Path primitives already live in runtime-core / domain. The watcher-only
//! sync knobs are constructor-injected as [`crate::ports::GitWatchSyncConfigV1`],
//! and the index path policy as a mount argument.

#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_domain::IndexPathPolicyV1;
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_domain::configuration::{
    ConfigurationValueV1, INDEX_EXCLUDE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY, SettingKey,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
#[cfg(test)]
pub use tracedecay_global_db::configuration::{registry, resolver};
pub use tracedecay_runtime_core::config::is_ambient_project_root;

/// The `index.exclude.v1` / `index.include.v1` policy a fresh profile
/// resolves, read from the registry's own defaults, for standalone owners
/// that no project configuration mounts.
#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used, clippy::panic)] // fixture gate: an invalid core registry is a build bug
pub fn registry_default_index_path_policy() -> IndexPathPolicyV1 {
    let registry = ConfigurationRegistry::core().expect("core configuration registry");
    let default = |key: &str| {
        let key = SettingKey::new(key).expect("index path setting key");
        match &registry.definition(&key).expect("registered").default_value {
            ConfigurationValueV1::StringList(patterns) => patterns.clone(),
            other => panic!("{key} default is not a string list: {other:?}"),
        }
    };
    IndexPathPolicyV1::new(
        default(INDEX_EXCLUDE_SETTING_KEY),
        default(INDEX_INCLUDE_SETTING_KEY),
    )
    .expect("registry default index path patterns compile")
}
