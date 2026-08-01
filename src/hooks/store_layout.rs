//! Per-hook-process memoization of profile store-layout resolution.
//!
//! One hook invocation resolves the same checkout's store layout three to six
//! times: once per analytics row written (`hook_invoked`, every hint row, and
//! `hook_completed` from the timing span's `Drop`), once in Hook V2's
//! `prepare_bound_hook`, once per surviving hint in the dedupe path, and twice
//! more in the memory-injection seen-facts path. Every one of those repeats the
//! same filesystem work — reading the enrollment and repository identity
//! markers, and, for a checkout no authority names yet, a `read_dir` sweep of
//! the whole profile's `projects/` directory.
//!
//! A hook is a one-shot subprocess spawned by `hook_cmd` for a single event, so
//! resolution is stable for its entire lifetime and the answer can simply be
//! kept. The cache is keyed by (profile root, project root) so a changed
//! `TRACEDECAY_USER_DATA_DIR` — the shape tests and multi-profile runs use —
//! never reads another profile's answer.
//!
//! Errors collapse to `None`, matching every hook caller, all of which already
//! discard the error and fall back (to the profile-wide analytics file, or to
//! emitting the hint without persisted dedupe).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::storage::StoreLayout;

type LayoutCache = HashMap<(PathBuf, PathBuf), Option<StoreLayout>>;

static ENROLLED_LAYOUTS: LazyLock<Mutex<LayoutCache>> =
    LazyLock::new(|| Mutex::new(LayoutCache::new()));
static RESOLVED_LAYOUTS: LazyLock<Mutex<LayoutCache>> =
    LazyLock::new(|| Mutex::new(LayoutCache::new()));

/// The store layout for `project_root` only when an authority already names
/// this checkout. Memoized [`crate::storage::resolve_enrolled_layout_for_current_profile`].
pub(super) fn enrolled_layout(project_root: &Path) -> Option<StoreLayout> {
    memoized(&ENROLLED_LAYOUTS, project_root, |root| {
        crate::storage::resolve_enrolled_layout_for_current_profile(root)
            .ok()
            .flatten()
    })
}

/// The store layout for `project_root`, falling back to the default
/// profile-sharded layout. Memoized [`crate::storage::resolve_layout_for_current_profile`].
pub(super) fn layout(project_root: &Path) -> Option<StoreLayout> {
    memoized(&RESOLVED_LAYOUTS, project_root, |root| {
        crate::storage::resolve_layout_for_current_profile(root).ok()
    })
}

fn memoized(
    cache: &Mutex<LayoutCache>,
    project_root: &Path,
    resolve: impl FnOnce(&Path) -> Option<StoreLayout>,
) -> Option<StoreLayout> {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        // Without a profile there is nothing to key on and nothing to resolve.
        return None;
    };
    let key = (profile_root, project_root.to_path_buf());
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

/// Drops every memoized layout. Test-only: within one test process the same
/// checkout is enrolled, re-enrolled, and re-pointed at fresh profiles, which a
/// hook subprocess never does.
#[cfg(test)]
pub(crate) fn clear_memoized_layouts() {
    ENROLLED_LAYOUTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    RESOLVED_LAYOUTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn enrolled_layout_is_resolved_once_per_project_root() {
        // `PinnedUserDataDir` already holds the user-data-dir test lock.
        let _profile = crate::config::PinnedUserDataDir::new();
        clear_memoized_layouts();

        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        assert!(
            enrolled_layout(&project_root).is_none(),
            "an unenrolled checkout must resolve to no layout"
        );

        // Enrolling after the miss is cached does not change the memoized
        // answer: a hook process resolves one checkout's identity once.
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_hook_layout_memo".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        assert!(enrolled_layout(&project_root).is_none());

        clear_memoized_layouts();
        let resolved = enrolled_layout(&project_root).expect("an enrolled checkout resolves");
        assert_eq!(
            resolved.identity.project_id.as_deref(),
            Some("proj_hook_layout_memo")
        );
        // A second call returns the same memoized layout.
        assert_eq!(
            enrolled_layout(&project_root).map(|layout| layout.data_root),
            Some(resolved.data_root)
        );
    }

    #[test]
    fn layout_falls_back_to_the_default_profile_shard() {
        // `PinnedUserDataDir` already holds the user-data-dir test lock.
        let _profile = crate::config::PinnedUserDataDir::new();
        clear_memoized_layouts();

        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        // Unlike `enrolled_layout`, this resolver mints the default shard.
        let first = layout(&project_root).expect("default profile-sharded layout resolves");
        let second = layout(&project_root).expect("memoized layout is returned again");
        assert_eq!(first.data_root, second.data_root);
        assert!(enrolled_layout(&project_root).is_none());
    }
}
