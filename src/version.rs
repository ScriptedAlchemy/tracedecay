//! What version this binary reports, and which value is safe to compare.
//!
//! [`PACKAGE_VERSION`] is the released `SemVer` that release-plz owns. It is the
//! only value with defined precedence, so upgrade checks, plugin staleness
//! stamps, and anything ordering releases must keep using it.
//!
//! [`build_version()`] is what the binary reports about *itself*. It appends
//! `SemVer` build metadata naming the commit it was compiled from
//! (`0.0.66+ab12cd34ef56`, or `…+ab12cd34ef56.dirty` when the worktree had
//! uncommitted changes). `SemVer` requires build metadata to be ignored when
//! determining precedence, so a locally built checkout binary is
//! traceable to an exact tree without touching the `version` field in
//! `Cargo.toml`. A build with no git checkout — a published crate, a registry
//! install — has no commit to name and reports bare [`PACKAGE_VERSION`].

use std::sync::LazyLock;

/// Git identity of the crate's own worktree, observed while compiling it.
///
/// `build.rs` `include!`s this module's file, so the probe that bakes
/// `TRACEDECAY_GIT_SHA` and `TRACEDECAY_GIT_DIRTY` into the binary is the same
/// code these tests exercise rather than a second copy that can drift.
pub mod build_identity;

/// The released version release-plz owns. Compare precedence against this, not
/// against [`build_version()`].
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short commit this binary was compiled from, or `None` when it was not built
/// from a git checkout of this crate.
pub fn git_sha() -> Option<&'static str> {
    sha_from_build_env(env!("TRACEDECAY_GIT_SHA"))
}

/// build.rs reports an unresolvable commit as `unknown`, which reads better
/// than an empty field in the plugin provenance header that shares the value.
fn sha_from_build_env(baked: &'static str) -> Option<&'static str> {
    match baked {
        "" | "unknown" => None,
        sha => Some(sha),
    }
}

/// Whether the worktree carried uncommitted or untracked changes when this
/// binary was compiled.
pub fn is_dirty() -> bool {
    env!("TRACEDECAY_GIT_DIRTY") == "1"
}

/// The self-identifying version of this binary: [`PACKAGE_VERSION`] plus build
/// metadata naming the commit it came from, when there was one.
pub fn build_version() -> &'static str {
    static BUILD_VERSION: LazyLock<String> =
        LazyLock::new(|| compose_build_version(PACKAGE_VERSION, git_sha(), is_dirty()));
    &BUILD_VERSION
}

/// Joins a package version with its build metadata. Kept separate from the
/// baked-in environment so both branches are directly testable.
fn compose_build_version(package: &str, sha: Option<&str>, dirty: bool) -> String {
    let Some(sha) = sha else {
        return package.to_string();
    };
    let dirty = if dirty { ".dirty" } else { "" };
    format!("{package}+{sha}{dirty}")
}

#[cfg(test)]
mod tests {
    use super::{
        PACKAGE_VERSION, build_version, compose_build_version, git_sha, is_dirty,
        sha_from_build_env,
    };

    /// The build script has to bake *something*; `unknown` is its way of saying
    /// there was no commit, and it must not reach the version string.
    #[test]
    fn an_unresolvable_commit_is_not_a_commit() {
        assert_eq!(sha_from_build_env("unknown"), None);
        assert_eq!(sha_from_build_env(""), None);
        assert_eq!(sha_from_build_env("ab12cd34ef56"), Some("ab12cd34ef56"));
    }

    #[test]
    fn a_checkout_build_names_its_commit_in_build_metadata() {
        assert_eq!(
            compose_build_version("0.0.66", Some("ab12cd34ef56"), false),
            "0.0.66+ab12cd34ef56"
        );
    }

    #[test]
    fn a_dirty_checkout_build_admits_its_uncommitted_state() {
        assert_eq!(
            compose_build_version("0.0.66", Some("ab12cd34ef56"), true),
            "0.0.66+ab12cd34ef56.dirty"
        );
    }

    /// A published crate has no `.git`, so there is no commit to name and no
    /// dangling `+` for a `SemVer` parser to choke on.
    #[test]
    fn a_build_without_git_reports_bare_package_version() {
        assert_eq!(compose_build_version("0.0.66", None, false), "0.0.66");
        assert_eq!(compose_build_version("0.0.66", None, true), "0.0.66");
    }

    /// Build metadata never changes precedence, so whatever this build reports
    /// still begins with the version release-plz published.
    #[test]
    fn this_binary_reports_its_own_build_identity() {
        let version = build_version();
        assert!(
            version.starts_with(PACKAGE_VERSION),
            "{version} must begin with the released version {PACKAGE_VERSION}"
        );
        match git_sha() {
            Some(sha) => {
                let suffix = if is_dirty() { ".dirty" } else { "" };
                assert_eq!(version, format!("{PACKAGE_VERSION}+{sha}{suffix}"));
            }
            None => assert_eq!(version, PACKAGE_VERSION),
        }
    }
}
