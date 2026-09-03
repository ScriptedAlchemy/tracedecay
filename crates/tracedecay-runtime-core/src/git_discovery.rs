//! Bounded repository-membership discovery for admission and routing paths.
//!
//! Git repository discovery is not an availability proof: a worktree can be
//! temporarily unreadable, a helper can time out, or its caller can cancel the
//! operation. This module preserves that uncertainty instead of collapsing it
//! into "not a repository".
//!
//! Admission uses the authority-first helpers. Session ingest and other path
//! probes that must not open pack indexes use
//! [`discover_repository_identity_cli_first`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::cancellation::{CancellationToken, MonotonicDeadline};

const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const REPOSITORY_IDENTITY_ARGS: [&str; 4] = [
    "rev-parse",
    "--show-toplevel",
    "--git-dir",
    "--git-common-dir",
];

/// Paired identity needed to compare a worktree with its repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryIdentity {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Why repository membership could not be decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitDiscoveryUnknown {
    Cancelled,
    DeadlineExceeded,
    SpawnFailed,
    ProbeFailed,
}

impl fmt::Display for GitDiscoveryUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::DeadlineExceeded => f.write_str("deadline exceeded"),
            Self::SpawnFailed => f.write_str("git helper could not be started"),
            Self::ProbeFailed => f.write_str("git identity probe failed"),
        }
    }
}

/// Repository discovery never represents uncertainty as absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRepositoryIdentityOutcome {
    Resolved(GitRepositoryIdentity),
    NotRepository,
    Unknown(GitDiscoveryUnknown),
}

impl GitRepositoryIdentityOutcome {
    /// True when membership could not be decided.
    #[hotpath::skip]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

/// Resolve a repository identity without blocking the async executor.
#[hotpath::measure(label = "runtime_core.git.discover")]
pub async fn discover_repository_identity(
    directory: &Path,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> GitRepositoryIdentityOutcome {
    if cancellation.is_cancelled() {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled);
    }
    if deadline.is_elapsed_at(Instant::now()) {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
    }

    if !repository_control_may_exist(directory) {
        return GitRepositoryIdentityOutcome::NotRepository;
    }
    if let Some(identity) = repository_identity_from_authority(directory) {
        return identity;
    }

    let Ok(mut command) = async_repository_identity_command(directory) else {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::SpawnFailed);
    };
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::SpawnFailed);
        }
    };
    let output = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled);
        }
        () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.instant())) => {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
        }
        output = child.wait_with_output() => output,
    };
    match output {
        Ok(output) if output.status.success() => {
            parse_repository_identity(directory, &output.stdout).unwrap_or(
                GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed),
            )
        }
        Ok(_) | Err(_) => GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed),
    }
}

/// Synchronous bounded discovery for legacy parser seams that cannot await.
///
/// Daemon and other async callers should use [`discover_repository_identity`].
pub fn discover_repository_identity_bounded(directory: &Path) -> GitRepositoryIdentityOutcome {
    discover_repository_identity_with_control(
        directory,
        MonotonicDeadline::at(Instant::now() + DEFAULT_DISCOVERY_TIMEOUT),
        &CancellationToken::new(),
    )
}

/// Synchronous discovery with explicit cancellation and monotonic deadline.
#[hotpath::measure(label = "runtime_core.git.discover_control")]
pub fn discover_repository_identity_with_control(
    directory: &Path,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> GitRepositoryIdentityOutcome {
    if cancellation.is_cancelled() {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled);
    }
    if deadline.is_elapsed_at(Instant::now()) {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
    }
    if !repository_control_may_exist(directory) {
        return GitRepositoryIdentityOutcome::NotRepository;
    }
    if let Some(identity) = repository_identity_from_authority(directory) {
        return identity;
    }

    let Ok(mut command) = repository_identity_command(directory) else {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::SpawnFailed);
    };
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::SpawnFailed);
        }
    };
    match capture_child(child, deadline, cancellation) {
        ChildCaptureOutcome::Completed(output) if output.status.success() => {
            parse_repository_identity(directory, &output.stdout).unwrap_or(
                GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed),
            )
        }
        ChildCaptureOutcome::Cancelled => {
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled)
        }
        ChildCaptureOutcome::DeadlineExceeded => {
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        }
        ChildCaptureOutcome::Completed(_) | ChildCaptureOutcome::Failed => {
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed)
        }
    }
}

/// Resolve identity from `rev-parse` first so session ingest does not open
/// pack indexes. Authority discovery runs only when the helper fails without
/// a timeout.
///
/// A timed-out helper is [`GitDiscoveryUnknown::DeadlineExceeded`] and does
/// not fall through to in-process discovery. An unreadable authority after a
/// failed helper is [`GitDiscoveryUnknown::ProbeFailed`], not
/// [`GitRepositoryIdentityOutcome::NotRepository`].
pub fn discover_repository_identity_cli_first(directory: &Path) -> GitRepositoryIdentityOutcome {
    if !repository_control_may_exist(directory) {
        return GitRepositoryIdentityOutcome::NotRepository;
    }
    discover_repository_identity_from_cli(
        directory,
        crate::git::git_capture_at(directory, &REPOSITORY_IDENTITY_ARGS),
        || repository_identity_from_authority(directory),
    )
}

fn discover_repository_identity_from_cli(
    directory: &Path,
    cli: crate::git::GitCaptureAtResult,
    authority_fallback: impl FnOnce() -> Option<GitRepositoryIdentityOutcome>,
) -> GitRepositoryIdentityOutcome {
    let fallback = || {
        authority_fallback().unwrap_or(GitRepositoryIdentityOutcome::Unknown(
            GitDiscoveryUnknown::ProbeFailed,
        ))
    };
    match cli {
        crate::git::GitCaptureAtResult::Captured(output) => {
            parse_repository_identity(directory, output.as_bytes()).unwrap_or_else(fallback)
        }
        crate::git::GitCaptureAtResult::Failed => fallback(),
        crate::git::GitCaptureAtResult::TimedOut => {
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        }
    }
}

fn repository_control_may_exist(directory: &Path) -> bool {
    let direct = directory.ancestors().any(git_control_exists_or_unknown);
    if direct {
        return true;
    }
    directory
        .canonicalize()
        .ok()
        .is_some_and(|canonical| canonical.ancestors().any(git_control_exists_or_unknown))
}

fn git_control_exists_or_unknown(candidate: &Path) -> bool {
    candidate.join(".git").try_exists().unwrap_or(true)
}

fn repository_identity_from_authority(directory: &Path) -> Option<GitRepositoryIdentityOutcome> {
    match crate::git_repository::GitRepositoryAuthority::discover(directory) {
        Ok(repository) => {
            let Some(worktree_root) = repository.worktree_root() else {
                return Some(GitRepositoryIdentityOutcome::NotRepository);
            };
            Some(GitRepositoryIdentityOutcome::Resolved(
                GitRepositoryIdentity {
                    worktree_root: worktree_root.to_path_buf(),
                    git_dir: repository.git_dir().to_path_buf(),
                    common_dir: repository.common_dir().to_path_buf(),
                },
            ))
        }
        Err(crate::git_repository::GitRepositoryError::NotARepository { .. }) => {
            Some(GitRepositoryIdentityOutcome::NotRepository)
        }
        Err(_) => None,
    }
}

fn repository_identity_command(
    directory: &Path,
) -> Result<Command, crate::git::GitProgramUnavailable> {
    let mut command = Command::new(crate::git::try_git_program()?);
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(directory)
        .args(REPOSITORY_IDENTITY_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}

fn async_repository_identity_command(
    directory: &Path,
) -> Result<tokio::process::Command, crate::git::GitProgramUnavailable> {
    let mut command = tokio::process::Command::new(crate::git::try_git_program()?);
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(directory)
        .args(REPOSITORY_IDENTITY_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    Ok(command)
}

fn parse_repository_identity(
    directory: &Path,
    stdout: &[u8],
) -> Option<GitRepositoryIdentityOutcome> {
    let text = std::str::from_utf8(stdout).ok()?;
    let mut lines = text.lines();
    let raw_worktree = PathBuf::from(lines.next()?.trim());
    let raw_git_dir = PathBuf::from(lines.next()?.trim());
    let raw_common = PathBuf::from(lines.next()?.trim());
    if raw_worktree.as_os_str().is_empty()
        || raw_git_dir.as_os_str().is_empty()
        || raw_common.as_os_str().is_empty()
    {
        return None;
    }
    let worktree_root = if raw_worktree.is_absolute() {
        raw_worktree
    } else {
        directory.join(raw_worktree)
    };
    let worktree_root = worktree_root.canonicalize().ok()?;
    let git_dir = if raw_git_dir.is_absolute() {
        raw_git_dir
    } else {
        directory.join(raw_git_dir)
    };
    let git_dir = git_dir.canonicalize().ok()?;
    let common_dir = if raw_common.is_absolute() {
        raw_common
    } else {
        directory.join(raw_common)
    };
    let common_dir = common_dir.canonicalize().ok()?;
    Some(GitRepositoryIdentityOutcome::Resolved(
        GitRepositoryIdentity {
            worktree_root,
            git_dir,
            common_dir,
        },
    ))
}

enum ChildCaptureOutcome {
    Completed(Output),
    Cancelled,
    DeadlineExceeded,
    Failed,
}

fn capture_child(
    mut child: Child,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> ChildCaptureOutcome {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_or(ChildCaptureOutcome::Failed, ChildCaptureOutcome::Completed);
            }
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(&mut child);
                return ChildCaptureOutcome::Failed;
            }
        }

        if cancellation.is_cancelled() {
            kill_and_reap(&mut child);
            return ChildCaptureOutcome::Cancelled;
        }
        let now = Instant::now();
        if deadline.is_elapsed_at(now) {
            kill_and_reap(&mut child);
            return ChildCaptureOutcome::DeadlineExceeded;
        }
        std::thread::sleep(
            deadline
                .instant()
                .saturating_duration_since(now)
                .min(CHILD_WAIT_POLL_INTERVAL),
        );
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git not on PATH — required for identity tests");
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    #[test]
    fn paired_cli_identity_resolves_relative_paths_without_discovery() {
        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let nested = worktree.join("src/deep");
        let git_dir = worktree.join(".git");
        let common_dir = tmp.path().join("main/.git");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&common_dir).unwrap();
        let output = format!("{}\n../../.git\n../../../main/.git", worktree.display());

        let outcome = discover_repository_identity_from_cli(
            &nested,
            crate::git::GitCaptureAtResult::Captured(output),
            || panic!("valid CLI identity must short-circuit in-process discovery"),
        );
        let GitRepositoryIdentityOutcome::Resolved(identity) = outcome else {
            panic!("paired CLI identity should resolve");
        };

        assert_eq!(identity.worktree_root, fs::canonicalize(&worktree).unwrap());
        assert_eq!(identity.git_dir, fs::canonicalize(&git_dir).unwrap());
        assert_eq!(identity.common_dir, fs::canonicalize(&common_dir).unwrap());
    }

    #[test]
    fn timed_out_cli_identity_does_not_fallback_to_discovery() {
        let tmp = tempdir().unwrap();
        let outcome = discover_repository_identity_from_cli(
            tmp.path(),
            crate::git::GitCaptureAtResult::TimedOut,
            || panic!("timed-out CLI identity must not fall through to in-process discovery"),
        );
        assert_eq!(
            outcome,
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        );
    }

    #[test]
    fn failed_cli_unreadable_authority_is_unknown_not_absent() {
        let tmp = tempdir().unwrap();
        let outcome = discover_repository_identity_from_cli(
            tmp.path(),
            crate::git::GitCaptureAtResult::Failed,
            || None,
        );
        assert_eq!(
            outcome,
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed)
        );
    }

    #[test]
    fn uncanonicalizable_common_dir_is_probe_failed() {
        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let git_dir = worktree.join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        let output = format!(
            "{}\n{}\n{}",
            worktree.display(),
            git_dir.display(),
            tmp.path().join("missing/.git").display()
        );

        let outcome = discover_repository_identity_from_cli(
            &worktree,
            crate::git::GitCaptureAtResult::Captured(output),
            || None,
        );
        assert_eq!(
            outcome,
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::ProbeFailed)
        );
    }

    #[test]
    fn cli_first_resolves_nested_linked_worktree() {
        let tmp = tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        run_git(&main, &["init", "--quiet"]);
        fs::write(main.join("README.md"), "hi").unwrap();
        run_git(&main, &["add", "."]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        let worktree = tmp.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", "--detach", worktree.to_str().unwrap()],
        );
        let nested = worktree.join("src/deep");
        fs::create_dir_all(&nested).unwrap();

        let GitRepositoryIdentityOutcome::Resolved(identity) =
            discover_repository_identity_cli_first(&nested)
        else {
            panic!("linked worktree identity");
        };
        assert_eq!(identity.worktree_root, fs::canonicalize(&worktree).unwrap());
        assert_eq!(
            identity.common_dir,
            fs::canonicalize(main.join(".git")).unwrap()
        );
        assert_ne!(identity.git_dir, identity.common_dir);
    }
}
