// Compiled twice on purpose: `build.rs` mounts this file as a `#[path]`
// module to resolve the provenance it bakes into the binary, and
// `tests/core_cli_suite/source_provenance_test.rs` mounts the same file so the code the tests
// exercise is the code the build script runs rather than a copy that can
// drift. Items are fully qualified instead of imported so the file stays
// self-contained in both hosts.

#[path = "source_provenance_git.rs"]
mod git;

use git::is_full_sha;
pub use git::watch_paths;

/// The `cargo package` VCS journal, written next to the packaged crate's
/// `Cargo.toml`.
const CARGO_VCS_INFO_FILE: &str = ".cargo_vcs_info.json";

/// Which of the three provenance sources answered. The build script emits a
/// different set of rerun edges per origin, so the origin travels with the
/// resolved identity instead of being re-derived.
#[derive(Debug, PartialEq, Eq)]
pub enum ProvenanceOrigin {
    /// Git reports `repository_root` as its own worktree top level; the
    /// repo-wide [`watch_paths`] watcher applies.
    VerifiedGit,
    /// `TRACEDECAY_RELEASE_GIT_SHA` named the commit and optional dirty state
    /// for an exported or externally orchestrated build.
    ReleaseEnv,
    /// `.cargo_vcs_info.json` from `cargo package` named the commit; the file
    /// itself becomes a rerun edge.
    PackagedVcsInfo { manifest_file: std::path::PathBuf },
}

/// Commit identity of the source tree this build compiles, plus where it came
/// from. There is no "unknown" state: a build that cannot name its commit
/// fails instead of shipping a fabricated identity.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedSourceProvenance {
    /// Full 40-hex commit sha.
    pub full_sha: String,
    /// Whether the compiled tree differed from the named commit.
    pub dirty: bool,
    pub origin: ProvenanceOrigin,
}

/// Resolves the commit identity of the crate rooted at `repository_root`,
/// consulting three sources in strict order, verified git worktree,
/// `TRACEDECAY_RELEASE_GIT_SHA` plus its optional `.dirty` marker, then the
/// `cargo package`
/// VCS journal adjacent to `manifest_dir`, and failing when none applies.
///
/// A registry install unpacks the crate into a directory that can itself sit
/// inside an unrelated repository, where `git rev-parse HEAD` would happily
/// report that repository's commit. A commit is therefore only trusted when
/// git reports `repository_root` as the worktree top level; every other case
/// falls through to the explicit sources.
pub fn resolve(
    repository_root: &std::path::Path,
    manifest_dir: &std::path::Path,
    release_env_provenance: Option<&str>,
) -> Result<ResolvedSourceProvenance, String> {
    if let Some(resolved) = git::resolve(repository_root)? {
        return Ok(ResolvedSourceProvenance {
            full_sha: resolved.full_sha,
            dirty: resolved.dirty,
            origin: ProvenanceOrigin::VerifiedGit,
        });
    }
    if let Some(value) = release_env_provenance {
        return resolve_from_release_env(value);
    }
    let vcs_info = manifest_dir.join(CARGO_VCS_INFO_FILE);
    if vcs_info.is_file() {
        return resolve_from_vcs_info(&vcs_info);
    }
    Err(format!(
        "no source provenance available for this build: {} is not its own git worktree \
         (checked `git rev-parse --show-toplevel` against the canonicalized root), \
         TRACEDECAY_RELEASE_GIT_SHA is unset, and {} does not exist; \
         a tracedecay binary must name the exact commit it was built from",
        repository_root.display(),
        vcs_info.display(),
    ))
}

fn resolve_from_release_env(value: &str) -> Result<ResolvedSourceProvenance, String> {
    let (sha, dirty) = match value.strip_suffix(".dirty") {
        Some(sha) => (sha, true),
        None => (value, false),
    };
    if !is_full_sha(sha) {
        return Err(format!(
            "TRACEDECAY_RELEASE_GIT_SHA is set to {value:?}, which is not a 40-character \
             lowercase hex commit sha optionally followed by `.dirty`",
        ));
    }
    Ok(ResolvedSourceProvenance {
        full_sha: sha.to_string(),
        dirty,
        origin: ProvenanceOrigin::ReleaseEnv,
    })
}

fn resolve_from_vcs_info(vcs_info: &std::path::Path) -> Result<ResolvedSourceProvenance, String> {
    let bytes = std::fs::read(vcs_info)
        .map_err(|error| format!("failed to read {}: {error}", vcs_info.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", vcs_info.display()))?;
    let git = value
        .get("git")
        .ok_or_else(|| format!("{} has no `git` object", vcs_info.display()))?;
    let sha = git
        .get("sha1")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{} has no `git.sha1` string", vcs_info.display()))?;
    if !is_full_sha(sha) {
        return Err(format!(
            "{} names `git.sha1` {sha:?}, which is not a 40-character lowercase hex \
             commit sha",
            vcs_info.display(),
        ));
    }
    // Cargo writes `git.dirty: true` only for `--allow-dirty` packages; its
    // absence is Cargo's statement that the packaged tree matched the commit.
    let dirty = match git.get("dirty") {
        None => false,
        Some(flag) => flag.as_bool().ok_or_else(|| {
            format!(
                "{} has a non-boolean `git.dirty` flag: {flag}",
                vcs_info.display(),
            )
        })?,
    };
    Ok(ResolvedSourceProvenance {
        full_sha: sha.to_string(),
        dirty,
        origin: ProvenanceOrigin::PackagedVcsInfo {
            manifest_file: vcs_info.to_path_buf(),
        },
    })
}
