//! Definition + on-demand shallow clone of the large repositories used by the
//! bench. Each repo is pinned to a constant ref so successive bench runs hit
//! identical source, and cloned with `--depth CLONE_DEPTH` (via init + fetch)
//! to avoid pulling full history. The depth still bounds the transfer but
//! leaves enough ancestry for history-walking tools (commit/pr context,
//! changelog, blame).
//!
//! All repos live under the directory pointed at by `TRACEDECAY_BENCH_REPOS_DIR`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug)]
pub struct Repo {
    pub name: &'static str,
    pub url: &'static str,
    /// A constant ref (tag preferred, SHA also fine). Anything that
    /// `git fetch --depth N origin <ref>` will resolve.
    pub git_ref: &'static str,
}

/// Commits of ancestry kept in the shallow clone: enough for `HEAD~n`
/// walks and base/head diffs the coverage groups exercise.
const CLONE_DEPTH: &str = "64";

pub const REPOS: &[Repo] = &[
    Repo {
        name: "polkadot-sdk",
        url: "https://github.com/paritytech/polkadot-sdk",
        git_ref: "polkadot-stable2412",
    },
    Repo {
        name: "emacs",
        url: "https://github.com/emacs-mirror/emacs",
        git_ref: "emacs-30.1",
    },
    Repo {
        name: "scipy",
        url: "https://github.com/scipy/scipy",
        git_ref: "v1.14.1",
    },
    Repo {
        name: "node",
        url: "https://github.com/nodejs/node",
        git_ref: "v22.11.0",
    },
];

/// The disposable repository name used by the bounded tool audit. The wrapper
/// creates this checkout from `benchmark_data/runtime/fixtures/project` and
/// gives it enough native history for the Git coverage seeds.
pub const SMALL_FIXTURE_NAME: &str = "runtime-fixture";

pub fn small_fixture_enabled() -> bool {
    std::env::var_os("TRACEDECAY_BENCH_SMALL_FIXTURE").is_some()
}

pub fn small_fixture_repo(root: &Path) -> Result<(Repo, PathBuf), String> {
    let dir = root.join(SMALL_FIXTURE_NAME);
    if !dir.is_dir() {
        return Err(format!(
            "small fixture checkout is missing at {}",
            dir.display()
        ));
    }
    Ok((
        Repo {
            name: SMALL_FIXTURE_NAME,
            url: "benchmark_data/runtime/fixtures/project",
            git_ref: "fixture",
        },
        dir,
    ))
}

/// Returns the bench repos root, or `None` if `TRACEDECAY_BENCH_REPOS_DIR` is unset.
pub fn repos_root() -> Option<PathBuf> {
    std::env::var_os("TRACEDECAY_BENCH_REPOS_DIR").map(PathBuf::from)
}

/// Optional comma-separated filter (`TRACEDECAY_BENCH_REPOS`). When set, only
/// repos whose name appears in the list are processed.
pub fn selected_repos() -> Vec<Repo> {
    let filter = std::env::var("TRACEDECAY_BENCH_REPOS").ok();
    match filter {
        None => REPOS.to_vec(),
        Some(s) => {
            let wanted: Vec<&str> = s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            REPOS
                .iter()
                .filter(|r| wanted.iter().any(|w| w.eq_ignore_ascii_case(r.name)))
                .copied()
                .collect()
        }
    }
}

/// Run a git command, inheriting stdout/stderr so the user sees fetch/clone
/// progress in real time. `output()` would capture the pipes and make a
/// multi-GB shallow clone look like the bench is frozen.
fn run_git(args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let status = cmd.status().map_err(|e| format!("spawn git: {e}"))?;
    if !status.success() {
        return Err(format!("git {} failed ({status})", args.join(" ")));
    }
    Ok(())
}

/// Skips work if the marker file `.bench-ref` already records the right ref.
pub fn ensure_cloned(root: &Path, repo: Repo) -> Result<PathBuf, String> {
    let dir = root.join(repo.name);
    let marker = dir.join(".bench-ref");
    if marker.exists()
        && let Ok(existing) = std::fs::read_to_string(&marker)
        && existing.trim() == repo.git_ref
    {
        // A cache populated by an older shallower clone still needs the
        // ancestry the coverage groups walk; deepen it in place.
        if history_depth(&dir) < CLONE_DEPTH.parse::<u64>().unwrap_or(1) {
            eprintln!("[bench] deepening {} (depth {CLONE_DEPTH})...", repo.name);
            run_git(
                &[
                    "fetch",
                    "--progress",
                    "--deepen",
                    CLONE_DEPTH,
                    "origin",
                    repo.git_ref,
                ],
                Some(&dir),
            )?;
        }
        return Ok(dir);
    }

    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("create dir: {e}"))?;
        run_git(&["init", "-q", "-b", "bench"], Some(&dir))?;
        run_git(&["remote", "add", "origin", repo.url], Some(&dir))?;
    }

    if std::env::var_os("TRACEDECAY_BENCH_SKIP_CLONE").is_some() {
        return Err(format!(
            "{} not at ref {} but TRACEDECAY_BENCH_SKIP_CLONE is set",
            repo.name, repo.git_ref
        ));
    }

    eprintln!(
        "[bench] fetching {} @ {} (depth {CLONE_DEPTH})...",
        repo.name, repo.git_ref
    );
    // `--progress` forces progress output even when stderr isn't a TTY (criterion
    // wraps the bench binary, so without it `git fetch` falls back to silence on
    // a multi-GB fetch).
    run_git(
        &[
            "fetch",
            "--progress",
            "--depth",
            CLONE_DEPTH,
            "origin",
            repo.git_ref,
        ],
        Some(&dir),
    )?;
    run_git(&["checkout", "--force", "FETCH_HEAD"], Some(&dir))?;
    std::fs::write(&marker, repo.git_ref).map_err(|e| format!("write marker: {e}"))?;
    Ok(dir)
}

/// Count commits reachable from HEAD (0 for an empty or detached checkout).
fn history_depth(dir: &Path) -> u64 {
    Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Revert all changes (tracked and untracked) made in `repo_dir` during the
/// bench: `git stash --include-untracked` followed by `git stash drop`. Run
/// at end-of-bench so write queries don't leak modifications past the run.
///
/// Returns `Ok(())` even if there is nothing to stash (`git stash` exits 0
/// in that case, just with a "No local changes" message on stderr).
pub fn restore_repo(repo_dir: &Path) -> Result<(), String> {
    // Check whether there is anything to stash; `stash drop` errors if the
    // stash list is empty, so we want to skip it cleanly when the tree is clean.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("git status: {e}"))?;
    if dirty.stdout.is_empty() {
        return Ok(());
    }

    run_git(
        &[
            "stash",
            "push",
            "--include-untracked",
            "--quiet",
            "-m",
            "tracedecay-bench",
        ],
        Some(repo_dir),
    )?;
    run_git(&["stash", "drop", "--quiet"], Some(repo_dir))?;
    Ok(())
}
