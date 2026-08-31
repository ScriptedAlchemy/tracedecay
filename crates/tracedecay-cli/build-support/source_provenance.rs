// Compiled twice on purpose: `build.rs` mounts this file as a `#[path]`
// module to resolve the provenance it bakes into the binary, and
// `tests/source_provenance_test.rs` mounts the same file so the code the tests
// exercise is the code the build script runs rather than a copy that can
// drift. Items are fully qualified instead of imported so the file stays
// self-contained in both hosts.

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
    /// `TRACEDECAY_RELEASE_GIT_SHA` named the commit (release builds from an
    /// exported tree). A release sha names an exact commit, so it is clean.
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
/// consulting three sources in strict order — verified git worktree,
/// `TRACEDECAY_RELEASE_GIT_SHA` (passed in by the build script so this stays
/// testable without process-global env mutation), then the `cargo package`
/// VCS journal adjacent to `manifest_dir` — and failing when none applies.
///
/// A registry install unpacks the crate into a directory that can itself sit
/// inside an unrelated repository, where `git rev-parse HEAD` would happily
/// report that repository's commit. A commit is therefore only trusted when
/// git reports `repository_root` as the worktree top level; every other case
/// falls through to the explicit sources.
pub fn resolve(
    repository_root: &std::path::Path,
    manifest_dir: &std::path::Path,
    release_env_sha: Option<&str>,
) -> Result<ResolvedSourceProvenance, String> {
    if let Some(resolved) = resolve_from_git(repository_root)? {
        return Ok(resolved);
    }
    if let Some(sha) = release_env_sha {
        return resolve_from_release_env(sha);
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

/// Verified-git source: applies only when `root` is its own worktree with a
/// resolvable `HEAD`. An unborn `HEAD` (fresh `git init`) is not a hit and
/// falls through to the explicit sources; a probe that half-answers — a HEAD
/// with no readable status — is a hard failure rather than a fabricated
/// "clean".
fn resolve_from_git(root: &std::path::Path) -> Result<Option<ResolvedSourceProvenance>, String> {
    if !is_own_worktree(root) {
        return Ok(None);
    }
    let Some(sha) = git_stdout(root, &["rev-parse", "HEAD"]) else {
        return Ok(None);
    };
    if !is_full_sha(&sha) {
        return Err(format!(
            "`git rev-parse HEAD` in {} printed {sha:?}, which is not a 40-character \
             lowercase hex commit sha",
            root.display(),
        ));
    }
    let Some(status) = git_output(root, &["status", "--porcelain"]) else {
        return Err(format!(
            "`git status --porcelain` failed in {} after HEAD resolved to {sha}; \
             refusing to guess whether the worktree was clean",
            root.display(),
        ));
    };
    Ok(Some(ResolvedSourceProvenance {
        full_sha: sha,
        dirty: !status.stdout.is_empty(),
        origin: ProvenanceOrigin::VerifiedGit,
    }))
}

fn resolve_from_release_env(sha: &str) -> Result<ResolvedSourceProvenance, String> {
    if !is_full_sha(sha) {
        return Err(format!(
            "TRACEDECAY_RELEASE_GIT_SHA is set to {sha:?}, which is not a 40-character \
             lowercase hex commit sha",
        ));
    }
    Ok(ResolvedSourceProvenance {
        full_sha: sha.to_string(),
        // A release sha names an exact exported commit; there is no worktree
        // whose drift could be observed.
        dirty: false,
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

fn is_full_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Paths whose change makes [`resolve`] answer differently: `HEAD` and its
/// reflog move with commits, checkouts, and resets; the index tracks staged and
/// stat-refreshed worktree state; and every existing tracked or untracked input
/// observes ordinary worktree edits. Without these the baked commit silently
/// describes whichever tree happened to trigger the previous build-script run.
///
/// Absent paths are dropped, because Cargo treats a missing `rerun-if-changed`
/// path as perpetually changed and would rerun the script on every build.
///
/// A newly created top-level untracked file (or one outside the source roots
/// below) cannot trigger an otherwise fresh build. Watching the worktree root
/// would catch it, but Cargo recursively scans directory watches and would
/// include target, generated, vendor, and dependency output. Those roots must
/// not become build-script inputs.
pub fn watch_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !is_own_worktree(root) {
        return Vec::new();
    }

    let mut paths = std::collections::BTreeSet::new();
    for name in ["HEAD", "logs/HEAD", "index"] {
        if let Some(raw) = git_stdout(root, &["rev-parse", "--git-path", name]) {
            let relative = std::path::Path::new(&raw);
            let path = if relative.is_absolute() {
                relative.to_path_buf()
            } else {
                root.join(relative)
            };
            if path.exists() {
                paths.insert(path);
            }
        }
    }
    paths.extend(worktree_input_paths(root));
    paths.extend(source_watch_paths(root));
    paths.into_iter().collect()
}

/// Safe source trees that may gain an untracked file without pulling generated
/// or dependency output into Cargo's recursive directory watcher. `crates`,
/// `dashboard`, and `plugin` are deliberately narrowed below rather than
/// watched as whole trees.
const SOURCE_WATCH_ROOTS: &[&str] = &[
    "src",
    "tests",
    "benches",
    "examples",
    "build-support",
    "scripts",
    "eval",
    "evals",
    "dashboard/src",
    "dashboard/codegen/schemas",
    "plugin/agents",
    "plugin/skills",
];

/// Existing Git inputs are watched individually so vendor and generated files
/// never turn into recursive directory watches. Git's standard excludes keep
/// ignored output such as `target`, `node_modules`, and `dashboard/app-dist`
/// out of this list.
fn worktree_input_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(output) = git_output(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    ) else {
        return Vec::new();
    };
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(git_path)
        .filter_map(|relative| {
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return None;
            }
            let path = root.join(relative);
            path.is_file().then_some(path)
        })
        .collect()
}

fn git_path(raw: &[u8]) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        Some(std::path::PathBuf::from(
            <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(raw.to_vec()),
        ))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(raw).ok().map(std::path::PathBuf::from)
    }
}

fn source_watch_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = SOURCE_WATCH_ROOTS
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let crates = root.join("crates");
    let Ok(entries) = std::fs::read_dir(crates) else {
        return paths;
    };
    for entry in entries.flatten() {
        let crate_root = entry.path();
        if !crate_root.is_dir() {
            continue;
        }
        for source in ["src", "tests", "benches", "examples", "test-support"] {
            let path = crate_root.join(source);
            if path.is_dir() {
                paths.push(path);
            }
        }
    }
    paths
}

/// Whether git reports `root` itself — not some ancestor — as a worktree top
/// level. This is the guard that keeps an unrelated enclosing repository out of
/// both the baked commit and the rebuild triggers.
fn is_own_worktree(root: &std::path::Path) -> bool {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Some(toplevel) = git_stdout(root, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    std::fs::canonicalize(toplevel).ok() == Some(canonical_root)
}

/// Trimmed stdout of a successful `git` run in `root`, or `None` when git is
/// missing, the command failed, or it printed nothing.
///
/// `--no-optional-locks` keeps every probe strictly read-only. Plain
/// `git status` refreshes the index stat cache and rewrites `.git/index` — a
/// path [`watch_paths`] hands Cargo as a rebuild trigger — so probing the tree
/// would arm the very trigger it reads. The flag suppresses only that
/// incidental write; the reported status is unchanged.
fn git_stdout(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = git_output(root, args)?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn git_output(root: &std::path::Path, args: &[&str]) -> Option<std::process::Output> {
    let output = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output)
}
