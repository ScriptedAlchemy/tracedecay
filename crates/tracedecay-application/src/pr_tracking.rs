//! Exact Git worktree ownership and bounded Git execution for explicitly
//! requested branch activation.

use std::path::Path;
use std::process::ExitStatus;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::{GitCommandBounds, GitCommandError};

mod worktrees;
pub use worktrees::{
    ManualBranchActivation, ManualBranchActivationError, ManualBranchArtifactOwnershipV1,
    ManualBranchArtifactsV1, ManualBranchLifecycleLeaseV1, acquire_manual_branch_lifecycle,
    cleanup_owned_worktree, cleanup_owned_worktree_off_runtime, manual_branch_artifact_ownership,
    manual_branch_artifact_ownership_off_runtime, manual_branch_artifacts_match,
    manual_branch_artifacts_match_off_runtime, manual_branch_source_owns_artifacts,
    prepare_manual_branch_worktree, resolve_branch_head,
};

const PR_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PR_COMMAND_STDOUT_LIMIT: usize = 8 * 1024 * 1024;
const PR_COMMAND_STDERR_LIMIT: usize = 64 * 1024;

/// Bounded command control for manual branch Git reads and worktree changes.
#[derive(Clone, Debug)]
pub struct PrCommandControlV1 {
    cancellation: Option<CancellationToken>,
    command_timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

impl PrCommandControlV1 {
    pub fn with_cancellation(cancellation: CancellationToken) -> Self {
        Self {
            cancellation: Some(cancellation),
            ..Self::default()
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_timeout(command_timeout: Duration) -> Self {
        Self {
            command_timeout,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_stdout_limit(max_stdout_bytes: usize) -> Self {
        Self {
            max_stdout_bytes,
            ..Self::default()
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

impl Default for PrCommandControlV1 {
    fn default() -> Self {
        Self {
            cancellation: None,
            command_timeout: PR_COMMAND_TIMEOUT,
            max_stdout_bytes: PR_COMMAND_STDOUT_LIMIT,
            max_stderr_bytes: PR_COMMAND_STDERR_LIMIT,
        }
    }
}

fn pr_git_command(
    repo_root: &Path,
    args: &[&str],
) -> Result<std::process::Command, GitCommandError> {
    use tracedecay_runtime_core::path_safety::{plain_git_args, plain_host_path};

    let mut command = std::process::Command::new(tracedecay_runtime_core::git::try_git_program()?);
    command
        .args(plain_git_args(args))
        .current_dir(plain_host_path(repo_root));
    Ok(command)
}

pub fn run_git_with_control(
    repo_root: &Path,
    args: &[&str],
    control: &PrCommandControlV1,
) -> Result<std::process::Output, GitCommandError> {
    let mut command = pr_git_command(repo_root, args)?;
    disable_git_credential_prompt(&mut command);
    tracedecay_runtime_core::git::bounded_command_output(
        command,
        None,
        &GitCommandBounds {
            deadline: Instant::now() + control.command_timeout,
            cancel: control.cancellation.clone(),
            max_stdout_bytes: control.max_stdout_bytes,
            max_stderr_bytes: control.max_stderr_bytes,
        },
    )
}

#[derive(Debug, thiserror::Error)]
pub enum PrGitCommandError {
    #[error(transparent)]
    Command(#[from] GitCommandError),
    #[error("git command '{arguments}' exited with {status}: {stderr}")]
    NonZeroExit {
        arguments: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("git command '{arguments}' returned invalid output: {detail}")]
    InvalidOutput { arguments: String, detail: String },
}

pub fn successful_git_with_control(
    repo_root: &Path,
    args: &[&str],
    control: &PrCommandControlV1,
) -> Result<std::process::Output, PrGitCommandError> {
    let output = run_git_with_control(repo_root, args, control)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(PrGitCommandError::NonZeroExit {
            arguments: args.join(" "),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

pub fn default_pr_command_control() -> &'static PrCommandControlV1 {
    static CONTROL: OnceLock<PrCommandControlV1> = OnceLock::new();
    CONTROL.get_or_init(PrCommandControlV1::default)
}

fn disable_git_credential_prompt(command: &mut std::process::Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worktree these commands add descends from a canonicalized data
    /// root, so on Windows it arrives here in the `\\?\` verbatim spelling
    /// Git rejects as an argument. The rewrite is defined on the spelling, so
    /// this runs on every host.
    #[test]
    fn worktree_path_arguments_are_spelled_plainly_for_git() {
        let command = pr_git_command(
            std::path::Path::new("/repo"),
            &[
                "worktree",
                "add",
                "-B",
                "tracedecay/pr-8",
                r"\\?\D:\a\_temp\tmp\.tmpF1zlYs-admission-wt",
                "refs/tracedecay/pr/8",
            ],
        )
        .expect("git executable should resolve");

        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "worktree",
                "add",
                "-B",
                "tracedecay/pr-8",
                r"D:\a\_temp\tmp\.tmpF1zlYs-admission-wt",
                "refs/tracedecay/pr/8",
            ]
            .map(std::ffi::OsStr::new)
        );
    }

    #[test]
    fn git_commands_enforce_deadline_cancellation_and_output_limits() {
        let root = tempfile::tempdir().expect("repository root");
        assert!(matches!(
            run_git_with_control(
                root.path(),
                &["--version"],
                &PrCommandControlV1::with_timeout(Duration::ZERO),
            ),
            Err(GitCommandError::DeadlineExceeded)
        ));

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            run_git_with_control(
                root.path(),
                &["--version"],
                &PrCommandControlV1::with_cancellation(cancellation),
            ),
            Err(GitCommandError::Cancelled)
        ));
        assert!(matches!(
            run_git_with_control(
                root.path(),
                &["--version"],
                &PrCommandControlV1::with_stdout_limit(1),
            ),
            Err(GitCommandError::OutputLimitExceeded {
                stream: "stdout",
                bound: 1
            })
        ));
        assert!(matches!(
            successful_git_with_control(
                root.path(),
                &["rev-parse", "--verify", "missing"],
                default_pr_command_control(),
            ),
            Err(PrGitCommandError::NonZeroExit { .. })
        ));
    }
}
