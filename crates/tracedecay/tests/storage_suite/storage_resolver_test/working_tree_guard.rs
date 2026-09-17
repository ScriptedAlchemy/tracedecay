//! Product-rule guard: TraceDecay never creates entries inside a user's
//! working tree.
//!
//! The only durable homes are the user profile (`~/.tracedecay`) and, for git
//! repositories, the `.git/` repository identity marker. These tests snapshot
//! every visible working-tree entry before the init/open journey and fail on
//! ANY new entry, so any reintroduced project-local write (an enrollment
//! marker, a cache, a lock, a config template) breaks them.

use super::*;
use std::collections::BTreeSet;

/// Every visible entry under `root`, relative to it, excluding `.git`
/// internals (the sanctioned repo-adjacent home).
fn visible_working_tree_entries(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == std::ffi::OsStr::new(".git"))
            {
                continue;
            }
            out.insert(relative);
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut entries = BTreeSet::new();
    walk(root, root, &mut entries);
    entries
}

#[tokio::test]
async fn init_and_open_leave_a_git_working_tree_unchanged() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn guarded() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    let before = visible_working_tree_entries(&project);

    init_with_maintenance(&project).await.unwrap().close();
    open_with_maintenance(&project).await.unwrap().close();

    let after = visible_working_tree_entries(&project);
    assert_eq!(
        before,
        after,
        "init/open must not create, rename, or remove working-tree entries; \
         new entries: {:?}; removed entries: {:?}",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>()
    );
    // The identity did persist — in the sanctioned `.git/` anchor, which is
    // deliberately outside the guarded snapshot.
    assert!(
        read_repository_identity_marker(&project).unwrap().is_some(),
        "the journey must persist identity in the .git/ repository marker"
    );
}

#[tokio::test]
async fn init_and_open_leave_a_non_git_working_tree_unchanged() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("plain-project");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn guarded() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    let before = visible_working_tree_entries(&project);

    init_with_maintenance(&project).await.unwrap().close();
    open_with_maintenance(&project).await.unwrap().close();

    let after = visible_working_tree_entries(&project);
    assert_eq!(
        before,
        after,
        "a non-git project must gain no entries at all — its identity is \
         deterministic and lives only in the profile registry; \
         new entries: {:?}",
        after.difference(&before).collect::<Vec<_>>()
    );
}
