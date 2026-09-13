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

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
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

    match authority_identity_off_executor(directory, deadline, cancellation).await {
        AuthorityProbe::Decided(outcome) => return outcome,
        AuthorityProbe::Interrupted(reason) => {
            return GitRepositoryIdentityOutcome::Unknown(reason);
        }
        AuthorityProbe::Unreadable => {}
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

/// What the in-process authority probe decided, or why it could not.
enum AuthorityProbe {
    Decided(GitRepositoryIdentityOutcome),
    /// The repository exists but its authority is unreadable, so the `git`
    /// helper is still worth asking.
    Unreadable,
    Interrupted(GitDiscoveryUnknown),
}

/// What one resolution published, once it finished.
#[derive(Clone, Debug)]
enum IdentityResolutionResult {
    Decided(GitRepositoryIdentityOutcome),
    Unreadable,
}

/// One resolution that is still running, and every caller's view of its answer.
struct IdentityResolution {
    started: Instant,
    published: tokio::sync::watch::Receiver<Option<IdentityResolutionResult>>,
}

/// In-flight identity resolutions, keyed by the directory each was asked about.
///
/// An entry exists only while its resolution runs: the answer itself is
/// retained by the per-root topology the resolution publishes into, not here.
static IDENTITY_RESOLUTIONS: LazyLock<Mutex<HashMap<PathBuf, IdentityResolution>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn identity_resolutions() -> MutexGuard<'static, HashMap<PathBuf, IdentityResolution>> {
    IDENTITY_RESOLUTIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// How long the in-flight resolution for `directory` has been running, when one
/// is still running.
///
/// A deferred caller reports this so its refusal says the root is *being*
/// resolved rather than merely unresolved.
#[must_use]
pub fn identity_resolution_elapsed(directory: &Path) -> Option<Duration> {
    identity_resolutions()
        .get(directory)
        .map(|resolution| resolution.started.elapsed())
}

/// Join the resolution running for `directory`, starting one if none is.
///
/// Single-flight per directory: concurrent callers share one walk, and a
/// caller that abandons its bounded wait does not abandon the work. The
/// resolution runs to completion on the blocking pool and publishes into the
/// retained per-root topology, so the next caller reads a resolved root
/// instead of starting the walk over.
fn join_identity_resolution(
    directory: &Path,
) -> tokio::sync::watch::Receiver<Option<IdentityResolutionResult>> {
    let mut resolutions = identity_resolutions();
    if let Some(resolution) = resolutions.get(directory) {
        return resolution.published.clone();
    }
    let (publish, published) = tokio::sync::watch::channel(None);
    resolutions.insert(
        directory.to_path_buf(),
        IdentityResolution {
            started: Instant::now(),
            published: published.clone(),
        },
    );
    drop(resolutions);

    // The slot is retired when the resolution ends, however it ends: normally,
    // by panic, or by the runtime dropping a blocking task it never ran. A root
    // is never left pointing at a resolution that will never publish, and no
    // typed failure survives its own resolution to poison the next one.
    let retire = RetireResolution(directory.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let result = resolve_identity_from_authority(&retire.0);
        // Retired before publishing, so a caller arriving after the answer
        // starts a fresh resolution — which the retained topology answers
        // without a walk — instead of joining a resolution that is history.
        drop(retire);
        let _ = publish.send(Some(result));
    });
    published
}

/// Retires one directory's resolution slot when the resolution ends.
struct RetireResolution(PathBuf);

impl Drop for RetireResolution {
    fn drop(&mut self) {
        identity_resolutions().remove(&self.0);
    }
}

fn resolve_identity_from_authority(path: &Path) -> IdentityResolutionResult {
    let exists = hotpath::measure_block!(
        "runtime_core.git.discover.control_walk",
        repository_control_may_exist(path)
    );
    if !exists {
        return IdentityResolutionResult::Decided(GitRepositoryIdentityOutcome::NotRepository);
    }
    hotpath::measure_block!(
        "runtime_core.git.discover.authority",
        repository_identity_from_authority(path)
    )
    .map_or(
        IdentityResolutionResult::Unreadable,
        IdentityResolutionResult::Decided,
    )
}

/// Run the ancestor walk and repository open on the blocking pool.
///
/// Live defect this exists for: this function promises discovery "without
/// blocking the async executor", but the in-process authority probe ran inline
/// on the calling worker with no bound at all — only the `git` subprocess
/// fallback below ever observed the deadline. On a slow volume every tokio
/// worker serving daemon connections sat inside `gix` discovery at once, so
/// the accept loop was never polled and the listening socket refused new
/// clients while the process stayed alive.
///
/// The blocking task cannot be interrupted once started, but the caller is:
/// an elapsed deadline or a cancelled token returns the typed uncertainty the
/// module contract already defines, and the resolution finishes on the
/// blocking pool without holding a worker.
///
/// Second live defect: that abandoned probe used to be *forgotten* as well as
/// abandoned, so every retry started its own walk and a root on a slow volume
/// stayed deferred for as long as clients kept asking. The resolution is now
/// single-flight and outlives the caller that started it.
async fn authority_identity_off_executor(
    directory: &Path,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> AuthorityProbe {
    let mut published = join_identity_resolution(directory);
    tokio::select! {
        biased;
        result = published_identity(&mut published) => match result {
            Some(IdentityResolutionResult::Decided(outcome)) => AuthorityProbe::Decided(outcome),
            Some(IdentityResolutionResult::Unreadable) => AuthorityProbe::Unreadable,
            None => AuthorityProbe::Interrupted(GitDiscoveryUnknown::ProbeFailed),
        },
        () = cancellation.cancelled() => {
            AuthorityProbe::Interrupted(GitDiscoveryUnknown::Cancelled)
        }
        () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.instant())) => {
            AuthorityProbe::Interrupted(GitDiscoveryUnknown::DeadlineExceeded)
        }
    }
}

/// Await the answer a joined resolution publishes, or `None` when the
/// resolution ended without one.
#[hotpath::measure(label = "runtime_core.git.discover.single_flight_wait", future = true)]
async fn published_identity(
    published: &mut tokio::sync::watch::Receiver<Option<IdentityResolutionResult>>,
) -> Option<IdentityResolutionResult> {
    loop {
        let result = published.borrow_and_update().clone();
        if let Some(result) = result {
            return Some(result);
        }
        published.changed().await.ok()?;
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
    match crate::git_repository::repository_topology(directory) {
        Ok(topology) => {
            let Some(worktree_root) = topology.worktree_root.clone() else {
                return Some(GitRepositoryIdentityOutcome::NotRepository);
            };
            Some(GitRepositoryIdentityOutcome::Resolved(
                GitRepositoryIdentity {
                    worktree_root,
                    git_dir: topology.git_dir.clone(),
                    common_dir: topology.common_dir.clone(),
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

    /// What one discovery walk costs on the modelled slow volume.
    const SLOW_WALK: Duration = Duration::from_millis(750);

    fn budget(within: Duration) -> MonotonicDeadline {
        MonotonicDeadline::at(Instant::now() + within)
    }

    /// A repository whose every live discovery walk costs [`SLOW_WALK`].
    fn slow_volume_repository(fixture: &Path) -> PathBuf {
        let repository = fixture.join("repository");
        fs::create_dir_all(&repository).unwrap();
        run_git(&repository, &["init", "-b", "main", "--quiet"]);
        crate::git_repository::delay_repository_discovery_for_test(&repository, SLOW_WALK);
        repository
    }

    /// Wait for whatever resolution is running for `directory` to retire its
    /// slot, which it does only after publishing into the retained topology.
    async fn published_resolution(directory: &Path) {
        for _ in 0..200 {
            if identity_resolution_elapsed(directory).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the resolution for {} never published", directory.display());
    }

    /// A probe abandoned at its deadline must still finish and publish, or the
    /// deferral never converges: every retry starts the walk over and a root on
    /// a slow volume stays deferred for as long as clients keep asking.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_probe_over_its_budget_publishes_for_the_next_resolution() {
        let tmp = tempdir().unwrap();
        let repository = slow_volume_repository(tmp.path());

        let deferred = discover_repository_identity(
            &repository,
            budget(Duration::from_millis(100)),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            deferred,
            GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded),
            "a probe past its budget is deferred, not decided"
        );
        assert!(
            identity_resolution_elapsed(&repository).is_some(),
            "the abandoned resolution must still be running, not discarded with its caller"
        );

        published_resolution(&repository).await;
        let started = Instant::now();
        let converged = discover_repository_identity(
            &repository,
            budget(Duration::from_millis(250)),
            &CancellationToken::new(),
        )
        .await;
        let elapsed = started.elapsed();
        let resolutions =
            crate::git_repository::repository_topology_resolution_count_for_test(&repository);
        crate::git_repository::reset_repository_discovery_for_test(&repository);

        let GitRepositoryIdentityOutcome::Resolved(identity) = converged else {
            panic!("the abandoned resolution must decide the next probe: {converged:?}");
        };
        assert_eq!(identity.worktree_root, repository.canonicalize().unwrap());
        assert_eq!(resolutions, 1, "the converged probe re-ran discovery");
        assert!(
            elapsed < SLOW_WALK,
            "the converged probe waited {elapsed:?}, so it walked the volume again"
        );
    }

    /// Once a root's topology is published, reading its HEAD must not walk the
    /// volume to find the repository again.
    ///
    /// Live wedge this covers: the topology memo only short-circuited topology
    /// questions. Every route resolution still read HEAD through a complete
    /// `gix::discover`, so a deferred root on a slow volume was rediscovered
    /// from scratch on every retry and the deferral never converged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_head_read_after_a_published_topology_does_not_walk_again() {
        let tmp = tempdir().unwrap();
        let repository = slow_volume_repository(tmp.path());
        let resolved = discover_repository_identity(
            &repository,
            budget(Duration::from_secs(5)),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            resolved,
            GitRepositoryIdentityOutcome::Resolved(_)
        ));

        let started = Instant::now();
        let branch = crate::branch::current_branch(&repository);
        let elapsed = started.elapsed();
        let walks = crate::git_repository::repository_discovery_count_for_test(&repository);
        crate::git_repository::reset_repository_discovery_for_test(&repository);

        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(
            walks, 1,
            "the HEAD read walked the volume again instead of opening the published Git directory"
        );
        assert!(
            elapsed < SLOW_WALK,
            "the HEAD read took {elapsed:?}, the cost of a fresh discovery walk"
        );
    }

    /// Reusing a retained topology opens the repository at its own Git
    /// directory instead of walking to it. A linked worktree is where that can
    /// go wrong: its HEAD lives beside its per-worktree Git directory, not in
    /// the common directory it shares with the main checkout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retained_topology_still_reports_the_linked_worktree_head() {
        let tmp = tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        run_git(&main, &["init", "-b", "main", "--quiet"]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        let linked = tmp.path().join("linked");
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "linked-branch",
                linked.to_str().unwrap(),
            ],
        );
        let within = Duration::from_secs(2);

        let cold =
            discover_repository_identity(&linked, budget(within), &CancellationToken::new()).await;
        let warm =
            discover_repository_identity(&linked, budget(within), &CancellationToken::new()).await;

        assert_eq!(
            cold, warm,
            "a retained topology must name the same identity the walk did"
        );
        assert!(matches!(warm, GitRepositoryIdentityOutcome::Resolved(_)));
        assert_eq!(
            crate::branch::current_branch(&linked).as_deref(),
            Some("linked-branch"),
            "a worktree opened at its own Git directory must report its own HEAD"
        );
        assert_eq!(
            crate::branch::current_branch(&main).as_deref(),
            Some("main"),
            "the main checkout must keep reporting its own HEAD"
        );
    }

    /// A decided-but-negative answer is an observation, not a verdict about the
    /// root: nothing about it survives the resolution that produced it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_typed_probe_failure_does_not_poison_the_root() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let within = Duration::from_secs(2);

        let absent =
            discover_repository_identity(&workspace, budget(within), &CancellationToken::new())
                .await;
        assert_eq!(
            absent,
            GitRepositoryIdentityOutcome::NotRepository,
            "an ordinary directory is not a repository"
        );
        assert!(
            identity_resolution_elapsed(&workspace).is_none(),
            "a finished resolution must not stay in flight"
        );

        run_git(&workspace, &["init", "--quiet"]);
        let outcome =
            discover_repository_identity(&workspace, budget(within), &CancellationToken::new())
                .await;

        let GitRepositoryIdentityOutcome::Resolved(identity) = outcome else {
            panic!("a repository created after a negative answer must resolve: {outcome:?}");
        };
        assert_eq!(identity.worktree_root, workspace.canonicalize().unwrap());
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
