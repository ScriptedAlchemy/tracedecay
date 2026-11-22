//! The one place this workspace opens a `gix` repository.
//!
//! Every repository `TraceDecay` reads is named by its caller. A project root,
//! a resolved Git directory, a linked worktree. The ambient `GIT_*` environment
//! names a different one. Git exports `GIT_DIR` and `GIT_INDEX_FILE` (and
//! sometimes `GIT_WORK_TREE`) to every hook process, and `TraceDecay` ships an
//! optional global post-commit hook that runs `tracedecay sync`, so an open
//! that honours the environment reads the hook's repository and its in-flight
//! index instead of the root it was asked to read.
//!
//! `gix::open::Options::default()` honours that environment. Since gix 0.87
//! `use_repository_local_environment` defaults to on, `GIT_INDEX_FILE` is a
//! config key with an environment override, and `Repository::index_path`
//! returns whatever the open resolved. Denying the `git_prefix` environment
//! gates every one of those keys, so identity stays with the requested path.
//!
//! Open through [`open`] or [`discover`]. A raw `gix::open`, `gix::open_opts`,
//! `gix::discover`, or `gix::discover_opts` anywhere else in `crates/` fails
//! the source-shape guard in `tests/git_repository_authority.rs`.

use std::path::Path;

/// Open the repository rooted at `path`.
///
/// `path` may be a worktree root or a Git directory, exactly as `gix::open`
/// accepts either.
pub fn open(path: &Path) -> gix::Result<gix::Repository> {
    gix::open_opts(path, options())
}

/// Walk upward from `path` to the repository that contains it.
pub fn discover(path: &Path) -> gix::Result<gix::Repository> {
    gix::discover_opts(path, gix::discover::upwards::Options::default(), options())
}

/// Visit linked worktrees without retaining Git's complete proxy inventory.
/// Git still owns parsing each worktree's metadata; only directory enumeration
/// is streamed. A failed entry aborts the census instead of yielding partial
/// liveness authority to callers that collect abandoned stores.
pub fn visit_linked_worktrees(
    repository: &gix::Repository,
    mut visit: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = repository
        .current_dir()
        .join(repository.common_dir())
        .join("worktrees");
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let id = gix::path::into_bstr(std::path::PathBuf::from(entry.file_name()))
            .map_err(std::io::Error::other)?;
        let id: &gix::bstr::BStr = id.as_ref();
        let Some(worktree) = repository
            .worktree_proxy_by_id(id)
            .map_err(|error| std::io::Error::other(error.to_string()))?
        else {
            // Gitoxide omits entries without a regular gitdir file, including
            // inaccessible metadata. Destructive retention requires a complete
            // census, so even a disappearing entry invalidates this attempt.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "worktree metadata is unavailable at {}",
                    entry.path().display()
                ),
            ));
        };
        let base = worktree
            .base()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        visit(worktree.git_dir(), &base)?;
    }
    Ok(())
}

/// Preserve the repository's normal configuration and attribute semantics
/// while rejecting `GIT_*` redirection from the ambient environment.
fn options() -> gix::open::Options {
    let mut permissions = gix::open::Permissions::secure();
    permissions.env.git_prefix = gix::sec::Permission::Deny;
    permissions.env.objects = gix::sec::Permission::Deny;
    permissions.config.env = false;
    gix::open::Options::default().permissions(permissions)
}
