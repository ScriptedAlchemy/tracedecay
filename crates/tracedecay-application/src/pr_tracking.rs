//! Git-backed PR discovery, exact worktree ownership, and durable managed state.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::{GitCommandBounds, GitCommandError};

mod worktrees;
pub use worktrees::{
    ManualBranchActivation, ManualBranchActivationError, ManualBranchArtifactOwnershipV1,
    ManualBranchArtifactsV1, ManualBranchLifecycleLeaseV1, PrCleanupArtifact, PrCleanupError,
    PrCleanupReceipt, ReconcileReport, cleanup_owned_worktree, cleanup_owned_worktree_off_runtime,
    cleanup_pr_worktree, cleanup_pr_worktree_off_runtime, manual_branch_artifact_ownership,
    manual_branch_artifact_ownership_off_runtime, manual_branch_artifacts_match,
    manual_branch_artifacts_match_off_runtime, manual_branch_source_owns_artifacts,
    prepare_manual_branch_worktree, prepare_pr_worktree, resolve_branch_head,
    try_acquire_manual_branch_lifecycle,
};

const STATE_FILENAME: &str = "pr-autotrack.json";
const PR_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PR_COMMAND_STDOUT_LIMIT: usize = 8 * 1024 * 1024;
const PR_COMMAND_STDERR_LIMIT: usize = 64 * 1024;
const GH_PR_LIST_LIMIT: usize = 1_000;

/// Bounded command control shared by PR discovery and managed worktree changes.
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

/// A same-repository PR head discovered on the origin remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPr {
    pub number: u64,
    pub head_branch: String,
    pub head_sha: String,
}

/// One complete or explicitly partial discovery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrDiscovery {
    pub open: Vec<DiscoveredPr>,
    pub skipped_forks: Vec<u64>,
    /// A partial discovery suppresses removals in the reconciliation owner.
    pub partial: bool,
}

/// A currently managed PR branch persisted in the project store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedPr {
    pub pr: u64,
    pub head_branch: String,
    #[serde(default)]
    pub head_sha: String,
    pub worktree: PathBuf,
    pub tracking_ref: String,
}

/// Durable managed-PR state keyed by collision-proof synthetic branch label.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrAutotrackState {
    #[serde(default)]
    pub managed: BTreeMap<String, ManagedPr>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManagedPrSummary {
    pub branch: String,
    pub pr: u64,
    pub head_branch: String,
}

pub fn pr_label(number: u64) -> String {
    format!("tracedecay/autotrack/pr/{number}")
}

pub fn pr_tracking_ref(number: u64) -> String {
    format!("refs/tracedecay/pr/{number}")
}

pub fn load_state(data_root: &Path) -> std::result::Result<PrAutotrackState, TraceDecayError> {
    let content = match std::fs::read_to_string(state_path(data_root)) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PrAutotrackState::default());
        }
        Err(error) => return Err(error.into()),
    };
    Ok(serde_json::from_str(&content)?)
}

pub fn save_state(data_root: &Path, state: &PrAutotrackState) -> std::io::Result<()> {
    let path = state_path(data_root);
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    tracedecay_runtime_core::storage::PrivateStoreIo::write_file_atomically(
        &path,
        &temp,
        json.as_bytes(),
    )
}

pub fn managed_summary(
    data_root: &Path,
) -> std::result::Result<Vec<ManagedPrSummary>, TraceDecayError> {
    let mut summaries = load_state(data_root)?
        .managed
        .into_iter()
        .map(|(branch, managed)| ManagedPrSummary {
            branch,
            pr: managed.pr,
            head_branch: managed.head_branch,
        })
        .collect::<Vec<_>>();
    summaries.sort_by_key(|summary| summary.pr);
    Ok(summaries)
}

fn state_path(data_root: &Path) -> PathBuf {
    data_root.join(STATE_FILENAME)
}

#[derive(Debug, Deserialize)]
struct GhPr {
    number: u64,
    #[serde(default, rename = "headRefName")]
    head_ref_name: String,
    #[serde(default, rename = "headRefOid")]
    head_ref_oid: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "isCrossRepository")]
    is_cross_repository: bool,
}

/// Builds the `git` invocation these PR commands run in `repo_root`.
///
/// The arguments carry resolved paths — the worktree `git worktree add`/`remove`
/// operate on descends from a canonicalized data root — and Git for Windows
/// rewrites a `\\?\` path *argument* to `//?/D:/...` and then fails with
/// "could not create leading directories". `plain_git_args` spells those
/// plainly for the child process, exactly as `bounded_git_output` does; every
/// other argument passes through unchanged.
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

#[hotpath::measure(label = "application.pr_tracking.discover")]
pub fn discover_open_prs_with_control(
    repo_root: &Path,
    control: &PrCommandControlV1,
) -> Result<PrDiscovery, String> {
    if origin_is_github(repo_root, control)
        && gh_available(control)
        && let Some(discovery) = discover_via_gh(repo_root, control)
    {
        return Ok(discovery);
    }
    discover_via_ls_remote(repo_root, control)
}

fn parse_gh_pr_list(json: &str, limit: usize) -> serde_json::Result<PrDiscovery> {
    let prs: Vec<GhPr> = serde_json::from_str(json)?;
    let mut discovery = PrDiscovery {
        partial: limit > 0 && prs.len() >= limit,
        ..PrDiscovery::default()
    };
    for pr in prs {
        if !pr.state.eq_ignore_ascii_case("open") {
            continue;
        }
        if pr.is_cross_repository || pr.head_ref_name.is_empty() || pr.head_ref_oid.is_empty() {
            discovery.skipped_forks.push(pr.number);
        } else {
            discovery.open.push(DiscoveredPr {
                number: pr.number,
                head_branch: pr.head_ref_name,
                head_sha: pr.head_ref_oid,
            });
        }
    }
    Ok(discovery)
}

fn parse_ls_remote_heads(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(split_ls_remote_line)
        .filter_map(|(sha, reference)| {
            reference
                .strip_prefix("refs/heads/")
                .map(|branch| (sha.to_owned(), branch.to_owned()))
        })
        .collect()
}

fn parse_ls_remote_pull_heads(output: &str) -> Vec<(u64, String)> {
    output
        .lines()
        .filter_map(split_ls_remote_line)
        .filter_map(|(sha, reference)| {
            reference
                .strip_prefix("refs/pull/")
                .and_then(|rest| rest.strip_suffix("/head"))
                .and_then(|number| number.parse::<u64>().ok())
                .map(|number| (number, sha.to_owned()))
        })
        .collect()
}

fn split_ls_remote_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let sha = parts.next()?;
    let reference = parts.next()?;
    (!sha.is_empty() && !reference.is_empty()).then_some((sha, reference))
}

fn map_pull_heads_to_branches(
    pull_heads: &[(u64, String)],
    head_shas: &HashMap<String, String>,
) -> PrDiscovery {
    let mut discovery = PrDiscovery::default();
    for (number, sha) in pull_heads {
        match head_shas.get(sha) {
            Some(branch) => discovery.open.push(DiscoveredPr {
                number: *number,
                head_branch: branch.clone(),
                head_sha: sha.clone(),
            }),
            None => discovery.skipped_forks.push(*number),
        }
    }
    discovery.open.sort_by_key(|pr| pr.number);
    discovery.skipped_forks.sort_unstable();
    discovery
}

fn disable_git_credential_prompt(command: &mut std::process::Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo");
}

fn origin_is_github(repo_root: &Path, control: &PrCommandControlV1) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(origins) = cache.lock()
        && let Some(cached) = origins.get(repo_root)
    {
        return *cached;
    }
    let result = successful_git_with_control(repo_root, &["remote", "get-url", "origin"], control)
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|url| url.contains("github.com"));
    if let Ok(mut origins) = cache.lock() {
        origins.insert(repo_root.to_path_buf(), result);
    }
    result
}

fn gh_available(control: &PrCommandControlV1) -> bool {
    if control
        .cancellation
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return false;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let mut command = std::process::Command::new("gh");
        command.arg("--version");
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
        .is_ok_and(|output| output.status.success())
    })
}

#[hotpath::measure(label = "application.pr_tracking.discover_gh")]
fn discover_via_gh(repo_root: &Path, control: &PrCommandControlV1) -> Option<PrDiscovery> {
    let limit = GH_PR_LIST_LIMIT.to_string();
    let mut command = std::process::Command::new("gh");
    command
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,headRefName,headRefOid,state,isCrossRepository",
        ])
        .current_dir(repo_root);
    disable_git_credential_prompt(&mut command);
    let output = tracedecay_runtime_core::git::bounded_command_output(
        command,
        None,
        &GitCommandBounds {
            deadline: Instant::now() + control.command_timeout,
            cancel: control.cancellation.clone(),
            max_stdout_bytes: control.max_stdout_bytes,
            max_stderr_bytes: control.max_stderr_bytes,
        },
    )
    .ok()
    .filter(|output| output.status.success())?;
    parse_gh_pr_list(&String::from_utf8(output.stdout).ok()?, GH_PR_LIST_LIMIT).ok()
}

#[hotpath::measure(label = "application.pr_tracking.discover_ls_remote")]
fn discover_via_ls_remote(
    repo_root: &Path,
    control: &PrCommandControlV1,
) -> Result<PrDiscovery, String> {
    let pull_heads = successful_git_with_control(
        repo_root,
        &["ls-remote", "origin", "refs/pull/*/head"],
        control,
    )
    .map_err(|error| format!("git ls-remote of PR head refs failed: {error}"))
    .and_then(|output| {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("git ls-remote PR output was not UTF-8: {error}"))
    })?;
    let head_shas =
        successful_git_with_control(repo_root, &["ls-remote", "--heads", "origin"], control)
            .map_err(|error| format!("git ls-remote of head refs failed: {error}"))
            .and_then(|output| {
                String::from_utf8(output.stdout)
                    .map_err(|error| format!("git ls-remote head output was not UTF-8: {error}"))
            })?;
    Ok(map_pull_heads_to_branches(
        &parse_ls_remote_pull_heads(&pull_heads),
        &parse_ls_remote_heads(&head_shas),
    ))
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

    #[test]
    fn gh_discovery_splits_same_repository_prs_from_forks() {
        let discovery = parse_gh_pr_list(
            r#"[
                {"number":1,"headRefName":"feature","headRefOid":"sha-1","state":"OPEN","isCrossRepository":false},
                {"number":2,"headRefName":"fork","headRefOid":"sha-2","state":"OPEN","isCrossRepository":true},
                {"number":3,"headRefName":"closed","headRefOid":"sha-3","state":"CLOSED","isCrossRepository":false}
            ]"#,
            200,
        )
        .expect("parse gh response");
        assert_eq!(
            discovery.open,
            vec![DiscoveredPr {
                number: 1,
                head_branch: "feature".to_owned(),
                head_sha: "sha-1".to_owned(),
            }]
        );
        assert_eq!(discovery.skipped_forks, vec![2]);
        assert!(!discovery.partial);
    }

    #[test]
    fn remote_ref_discovery_matches_same_repository_heads() {
        let pull_heads = parse_ls_remote_pull_heads(
            "sha-feature\trefs/pull/1/head\nsha-fork\trefs/pull/2/head\n",
        );
        let heads = parse_ls_remote_heads("sha-feature\trefs/heads/feature\n");
        let discovery = map_pull_heads_to_branches(&pull_heads, &heads);
        assert_eq!(discovery.open[0].number, 1);
        assert_eq!(discovery.skipped_forks, vec![2]);
    }

    #[test]
    fn reaching_the_gh_limit_marks_discovery_partial() {
        let json = r#"[
            {"number":1,"headRefName":"a","headRefOid":"s1","state":"OPEN","isCrossRepository":false},
            {"number":2,"headRefName":"b","headRefOid":"s2","state":"OPEN","isCrossRepository":false}
        ]"#;
        assert!(parse_gh_pr_list(json, 2).expect("partial list").partial);
        assert!(!parse_gh_pr_list(json, 3).expect("complete list").partial);
    }

    #[test]
    fn legacy_state_without_head_sha_remains_refreshable() {
        let store = tempfile::tempdir().expect("store root");
        std::fs::write(
            state_path(store.path()),
            r#"{"managed":{"pr/8":{"pr":8,"head_branch":"legacy","worktree":"pr-worktrees/pr-8","tracking_ref":"refs/tracedecay/pr/8"}}}"#,
        )
        .expect("legacy state");

        assert_eq!(
            load_state(store.path()).expect("load legacy state").managed["pr/8"].head_sha,
            ""
        );
    }
}
