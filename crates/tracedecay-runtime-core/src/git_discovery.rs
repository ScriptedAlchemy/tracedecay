//! Bounded repository-membership discovery for admission and routing paths.
//!
//! Git repository discovery is not an availability proof: a worktree can be
//! temporarily unreadable, a helper can time out, or its caller can cancel the
//! operation. This module preserves that uncertainty instead of collapsing it
//! into "not a repository".

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::cancellation::{CancellationToken, MonotonicDeadline};

const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

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

/// Repository discovery never represents uncertainty as absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRepositoryIdentityOutcome {
    Resolved(GitRepositoryIdentity),
    NotRepository,
    Unknown(GitDiscoveryUnknown),
}

/// Resolve a repository identity without blocking the async executor.
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

    let directory = directory.to_path_buf();
    let cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || {
        discover_repository_identity_with_control(&directory, deadline, &cancellation)
    })
    .await
    .unwrap_or(GitRepositoryIdentityOutcome::Unknown(
        GitDiscoveryUnknown::ProbeFailed,
    ))
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

    let mut command = repository_identity_command(directory);
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

fn repository_identity_command(directory: &Path) -> Command {
    let mut command = Command::new(crate::git::git_program());
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(directory)
        .args([
            "rev-parse",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
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
    let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
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
