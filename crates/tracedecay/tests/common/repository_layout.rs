use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static REPOSITORY_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Returns the repository root for repository-owned test assets.
///
/// Cargo and the Bazel launcher both start a suite in the `tracedecay`
/// package directory, two levels below the root. A compile-time manifest path
/// would instead name whichever checkout or Bazel output base compiled the
/// binary. The root is absolute because suites also run child processes in it.
pub fn repository_root() -> &'static Path {
    REPOSITORY_ROOT
        .get_or_init(|| std::path::absolute("../..").expect("absolute repository root"))
        .as_path()
}

pub fn repository_path(relative: impl AsRef<Path>) -> PathBuf {
    repository_root().join(relative)
}
