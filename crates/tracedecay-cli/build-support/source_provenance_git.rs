// Shared by the CLI build script and the dependency-free Cargo rerun fixture.
// Packaged VCS metadata parsing stays in source_provenance.rs.

#[derive(Debug, PartialEq, Eq)]
pub struct GitProvenance {
    pub full_sha: String,
    pub dirty: bool,
}

/// Verified-git source: applies only when `root` is its own worktree with a
/// resolvable `HEAD`. An unborn `HEAD` (fresh `git init`) is not a hit and
/// falls through to the explicit sources; a probe that half-answers, a HEAD
/// with no readable status, is a hard failure rather than a fabricated
/// "clean".
pub fn resolve(root: &std::path::Path) -> Result<Option<GitProvenance>, String> {
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
    Ok(Some(GitProvenance {
        full_sha: sha,
        dirty: !status.stdout.is_empty(),
    }))
}

pub(super) fn is_full_sha(sha: &str) -> bool {
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

/// Whether git reports `root` itself, not some ancestor, as a worktree top
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
/// `git status` refreshes the index stat cache and rewrites `.git/index`, a
/// path [`watch_paths`] hands Cargo as a rebuild trigger, so probing the tree
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
