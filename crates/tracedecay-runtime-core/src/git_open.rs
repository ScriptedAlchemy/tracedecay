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
// The error is gix's own and it carries it by value; boxing here would only
// make every caller's `map_err` differ from the type gix hands back. Upstream
// allows the same lint on the call this wraps.
#[allow(clippy::result_large_err)]
pub fn open(path: &Path) -> Result<gix::Repository, gix::open::Error> {
    gix::open_opts(path, options())
}

/// Walk upward from `path` to the repository that contains it.
#[allow(clippy::result_large_err)]
pub fn discover(path: &Path) -> Result<gix::Repository, gix::discover::Error> {
    gix::discover_opts(path, gix::discover::upwards::Options::default(), options())
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
