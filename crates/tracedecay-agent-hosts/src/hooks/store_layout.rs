//! Per-hook-process memoization of profile store-layout resolution.
//!
//! One hook invocation resolves the same checkout's store layout three to six
//! times: once per analytics row written (`hook_invoked`, every hint row, and
//! `hook_completed` from the timing span's `Drop`), once in the native Hook
//! `prepare_bound_hook`, once per surviving hint in the dedupe path, and twice
//! more in the memory-injection seen-facts path. Every one of those repeats the
//! same filesystem work, reading the enrollment and repository identity
//! markers, and, for a checkout no authority names yet, a `read_dir` sweep of
//! the whole profile's `projects/` directory.
//!
//! A hook is a one-shot subprocess spawned by `hook_cmd` for a single event, so
//! resolution is stable for its entire lifetime and the answer can simply be
//! kept. The cache is keyed by (profile root, project root) so two profiles
//! in one process never read each other's answer.
//!
//! Errors collapse to `None`, matching every hook caller, all of which already
//! discard the error and fall back (to the profile-wide analytics file, or to
//! emitting the hint without persisted dedupe).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use tracedecay_runtime_core::storage::StoreLayout;

type LayoutCache = HashMap<(PathBuf, PathBuf), Option<StoreLayout>>;

static ENROLLED_LAYOUTS: LazyLock<Mutex<LayoutCache>> =
    LazyLock::new(|| Mutex::new(LayoutCache::new()));
static RESOLVED_LAYOUTS: LazyLock<Mutex<LayoutCache>> =
    LazyLock::new(|| Mutex::new(LayoutCache::new()));

/// The store layout for `project_root` in `profile_root` only when an
/// authority already names this checkout. Memoized
/// [`tracedecay_runtime_core::storage::resolve_persisted_layout`].
pub(super) fn enrolled_layout(profile_root: &Path, project_root: &Path) -> Option<StoreLayout> {
    memoized(&ENROLLED_LAYOUTS, profile_root, project_root, |root| {
        tracedecay_runtime_core::storage::resolve_persisted_layout(root, profile_root)
            .ok()
            .flatten()
    })
}

/// The store layout for `project_root` in `profile_root`, falling back to the
/// default profile-sharded layout. Memoized
/// [`tracedecay_runtime_core::storage::resolve_layout`].
pub(super) fn layout(profile_root: &Path, project_root: &Path) -> Option<StoreLayout> {
    memoized(&RESOLVED_LAYOUTS, profile_root, project_root, |root| {
        tracedecay_runtime_core::storage::resolve_layout(root, profile_root).ok()
    })
}

fn memoized(
    cache: &Mutex<LayoutCache>,
    profile_root: &Path,
    project_root: &Path,
    resolve: impl FnOnce(&Path) -> Option<StoreLayout>,
) -> Option<StoreLayout> {
    let key = (profile_root.to_path_buf(), project_root.to_path_buf());
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return hit.clone();
    }
    // Resolved outside the lock: resolution touches the filesystem, and a hook
    // must never serialize on another resolution to read a cache.
    let resolved = resolve(project_root);
    cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, resolved.clone());
    resolved
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn enrolled_layout_is_resolved_once_per_project_root() {
        let profile = tempfile::tempdir().unwrap();
        let profile = profile.path();

        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        assert!(
            enrolled_layout(profile, &project_root).is_none(),
            "an unenrolled checkout must resolve to no layout"
        );

        // Enrolling after the miss is cached does not change the memoized
        // answer: a hook process resolves one checkout's identity once.
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            "proj_hook_layout_memo",
        )
        .unwrap();
        assert!(enrolled_layout(profile, &project_root).is_none());

        ENROLLED_LAYOUTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(profile.to_path_buf(), project_root.clone()));
        let resolved =
            enrolled_layout(profile, &project_root).expect("an enrolled checkout resolves");
        assert_eq!(
            resolved.identity.project_id.as_deref(),
            Some("proj_hook_layout_memo")
        );
        // A second call returns the same memoized layout.
        assert_eq!(
            enrolled_layout(profile, &project_root).map(|layout| layout.data_root),
            Some(resolved.data_root)
        );
    }

    #[test]
    fn layout_falls_back_to_the_default_profile_shard() {
        let profile = tempfile::tempdir().unwrap();
        let profile = profile.path();

        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        // Unlike `enrolled_layout`, this resolver mints the default shard.
        let first =
            layout(profile, &project_root).expect("default profile-sharded layout resolves");
        let second = layout(profile, &project_root).expect("memoized layout is returned again");
        assert_eq!(first.data_root, second.data_root);
        assert!(enrolled_layout(profile, &project_root).is_none());
    }
}
