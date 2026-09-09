//! Exact Git ownership and managed worktree preparation/retirement.

use std::path::{Path, PathBuf};

use fs2::FileExt;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_runtime_core::branch::BranchAddOutcome;

use super::{
    PrCommandControlV1, PrGitCommandError, pr_label, pr_tracking_ref, run_git_with_control,
    successful_git_with_control,
};

const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";
const GIT_AUTHORITY_UNAVAILABLE: &str = "git_authority_unavailable";
const INVALID_BRANCH_REF: &str = "invalid_branch_ref";
const BRANCH_ACTIVATION_FAILED: &str = "branch_activation_failed";
const BRANCH_LIFECYCLE_CONTENDED: &str = "branch_lifecycle_contended";

/// Outcome of a successful manual branch-head activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualBranchActivation {
    pub branch: String,
    pub head_sha: String,
    pub worktree: PathBuf,
    pub outcome: BranchAddOutcome,
}

/// A summary of what one reconcile pass changed, for logging and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
    pub skipped_forks: Vec<u64>,
    pub capped: bool,
    pub removals_suppressed: bool,
    pub failures: Vec<(String, String)>,
}

/// The exact Git and filesystem artifacts owned by one manually activated
/// branch. The raw branch name remains the Git ref identity; only the
/// filesystem path is hashed so distinct valid refs cannot alias on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualBranchArtifactsV1 {
    pub branch: String,
    pub worktree: PathBuf,
    pub tracking_ref: String,
    pub label: String,
    /// Digest of the raw branch name, computed once at construction; both the
    /// worktree directory and the lifecycle lock file derive from it.
    branch_digest: String,
}

impl ManualBranchArtifactsV1 {
    pub fn for_branch(data_root: &Path, branch: &str) -> Self {
        let branch_digest = sha256_hex(branch.as_bytes());
        Self {
            branch: branch.to_owned(),
            worktree: data_root.join("branch-worktrees").join(&branch_digest),
            tracking_ref: format!("refs/tracedecay/branch/{branch}"),
            label: format!("tracedecay/track/{branch}"),
            branch_digest,
        }
    }

    /// Each head is staged independently so interruption cannot destroy the
    /// artifacts still named by the previously published branch provenance.
    pub fn for_head(data_root: &Path, branch: &str, head: &str) -> Self {
        let mut artifacts = Self::for_branch(data_root, branch);
        let generation = sha256_hex(head.as_bytes());
        artifacts
            .worktree
            .set_file_name(format!("{}-{generation}", artifacts.branch_digest));
        artifacts.tracking_ref = format!("{}-{generation}", artifacts.tracking_ref);
        artifacts.label = format!("{}-{generation}", artifacts.label);
        artifacts
    }

    /// Lifecycle locks live beside `branch-worktrees`, never inside it. The
    /// lease is taken before the branch identity is resolved, so a typed
    /// pre-mutation refusal (missing ref, unavailable Git authority) must not
    /// leave the worktree root behind as evidence of an activation that never
    /// happened — and nothing enumerating branch worktrees has to filter a
    /// non-worktree entry out.
    fn lifecycle_lock_path(&self, data_root: &Path) -> PathBuf {
        data_root
            .join("branch-lifecycle")
            .join(format!("{}.lock", self.branch_digest))
    }
}

/// Non-blocking exact-branch lifecycle gate. It deliberately spans activation,
/// worktree replacement, scheduler mount, and metadata sealing; a concurrent
/// caller receives a typed retryable contention rather than observing a
/// partially replaced branch route.
pub struct ManualBranchLifecycleLeaseV1 {
    branch: String,
    _lock: std::fs::File,
}

impl ManualBranchLifecycleLeaseV1 {
    pub fn matches_branch(&self, branch: &str) -> bool {
        self.branch == branch
    }
}

pub fn try_acquire_manual_branch_lifecycle(
    data_root: &Path,
    branch: &str,
) -> std::result::Result<ManualBranchLifecycleLeaseV1, ManualBranchActivationError> {
    let artifacts = ManualBranchArtifactsV1::for_branch(data_root, branch);
    let lock_path = artifacts.lifecycle_lock_path(data_root);
    let lock_directory = lock_path.parent().ok_or_else(|| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch lifecycle lock '{}' has no parent",
            lock_path.display()
        ))
    })?;
    std::fs::create_dir_all(lock_directory).map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "cannot create manual branch lifecycle lock directory: {error}"
        ))
    })?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            ManualBranchActivationError::activation_failed(format!(
                "cannot open manual branch lifecycle lock '{}': {error}",
                lock_path.display()
            ))
        })?;
    lock.try_lock_exclusive().map_err(|error| {
        ManualBranchActivationError::lifecycle_contended(format!(
            "branch '{branch}' lifecycle is already active at '{}': {error}",
            lock_path.display()
        ))
    })?;
    Ok(ManualBranchLifecycleLeaseV1 {
        branch: branch.to_owned(),
        _lock: lock,
    })
}

/// Typed failure for manual branch-head activation. Missing scheduler or
/// identity is a project-route state, not a transport error or empty success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualBranchActivationError {
    /// No injected code-index scheduler, retained graph, or project identity.
    SchedulerUnavailable { detail: String },
    /// Git cannot name a worktree root for the requested project.
    GitAuthorityUnavailable { detail: String },
    /// The requested name is not a resolvable local or origin branch ref.
    InvalidBranchRef { detail: String },
    /// Worktree preparation or scheduler mount failed after admission.
    ActivationFailed { detail: String },
    /// An exact lifecycle owner is already activating, replacing, or retiring
    /// the requested branch.
    LifecycleContended { detail: String },
}

impl ManualBranchActivationError {
    /// Stable reason code for JSON-RPC / project-route mapping.
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::SchedulerUnavailable { .. } => CODE_INDEX_SCHEDULER_UNAVAILABLE,
            Self::GitAuthorityUnavailable { .. } => GIT_AUTHORITY_UNAVAILABLE,
            Self::InvalidBranchRef { .. } => INVALID_BRANCH_REF,
            Self::ActivationFailed { .. } => BRANCH_ACTIVATION_FAILED,
            Self::LifecycleContended { .. } => BRANCH_LIFECYCLE_CONTENDED,
        }
    }

    /// Whether a later retry with the same arguments can succeed.
    pub fn retryable(&self) -> bool {
        match self {
            Self::SchedulerUnavailable { .. }
            | Self::ActivationFailed { .. }
            | Self::LifecycleContended { .. }
            | Self::GitAuthorityUnavailable { .. } => true,
            Self::InvalidBranchRef { .. } => false,
        }
    }

    /// Human-readable detail carried beside [`Self::reason_code`].
    pub fn detail(&self) -> &str {
        match self {
            Self::SchedulerUnavailable { detail }
            | Self::GitAuthorityUnavailable { detail }
            | Self::InvalidBranchRef { detail }
            | Self::ActivationFailed { detail }
            | Self::LifecycleContended { detail } => detail,
        }
    }

    pub fn scheduler_unavailable(detail: impl Into<String>) -> Self {
        Self::SchedulerUnavailable {
            detail: detail.into(),
        }
    }

    pub fn git_unavailable(detail: impl Into<String>) -> Self {
        Self::GitAuthorityUnavailable {
            detail: detail.into(),
        }
    }

    pub fn invalid_ref(detail: impl Into<String>) -> Self {
        Self::InvalidBranchRef {
            detail: detail.into(),
        }
    }

    pub fn activation_failed(detail: impl Into<String>) -> Self {
        Self::ActivationFailed {
            detail: detail.into(),
        }
    }

    fn lifecycle_contended(detail: impl Into<String>) -> Self {
        Self::LifecycleContended {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ManualBranchActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason_code(), self.detail())
    }
}

impl std::error::Error for ManualBranchActivationError {}

pub fn resolve_branch_head(
    repo_root: &Path,
    branch: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<String, ManualBranchActivationError> {
    let candidates = [
        format!("refs/heads/{branch}"),
        branch.to_string(),
        format!("refs/remotes/origin/{branch}"),
    ];
    for reference in candidates {
        if let Some(sha) = resolve_git_ref(repo_root, &reference, command_control) {
            return Ok(sha);
        }
    }
    Err(ManualBranchActivationError::invalid_ref(format!(
        "branch '{branch}' does not resolve to a git ref"
    )))
}

fn resolve_git_ref(
    repo_root: &Path,
    reference: &str,
    command_control: &PrCommandControlV1,
) -> Option<String> {
    successful_git_with_control(
        repo_root,
        &["rev-parse", "--verify", "--end-of-options", reference],
        command_control,
    )
    .ok()
    .and_then(|output| String::from_utf8(output.stdout).ok())
    .map(|sha| sha.trim().to_string())
    .filter(|sha| !sha.is_empty())
}

pub fn prepare_manual_branch_worktree(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), String> {
    successful_git_with_control(
        repo_root,
        &["update-ref", tracking_ref, expected_head],
        command_control,
    )
    .map_err(|error| format!("failed to publish branch tracking ref: {error}"))?;
    checkout_linked_worktree(repo_root, worktree, tracking_ref, label, command_control)
}

pub fn manual_branch_source_owns_artifacts(
    data_root: &Path,
    branch: &str,
    source: &tracedecay_runtime_core::branch_meta::BranchGraphSourceV1,
) -> bool {
    let canonical_data_root = data_root
        .canonicalize()
        .unwrap_or_else(|_| data_root.to_path_buf());
    let artifacts =
        ManualBranchArtifactsV1::for_head(&canonical_data_root, branch, &source.source_oid);
    let worktree = artifacts
        .worktree
        .canonicalize()
        .unwrap_or(artifacts.worktree);
    source.worktree_root == worktree.to_string_lossy().as_ref()
        && source.reference == format!("refs/heads/{}", artifacts.label)
        && !source.source_oid.is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualBranchArtifactOwnershipV1 {
    Absent,
    Exact,
    Foreign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExactRefReadV1 {
    Absent,
    Present(String),
}

fn checked_path_exists(path: &Path) -> std::result::Result<bool, ManualBranchActivationError> {
    path.try_exists().map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot inspect manual artifact '{}': {error}",
            path.display()
        ))
    })
}

fn validated_git_oid(
    stdout: &[u8],
    reference: &str,
) -> std::result::Result<String, ManualBranchActivationError> {
    let oid = std::str::from_utf8(stdout).map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "Git returned non-UTF-8 OID for '{reference}': {error}"
        ))
    })?;
    let oid = oid.trim();
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManualBranchActivationError::git_unavailable(format!(
            "Git returned an invalid OID for '{reference}'"
        )));
    }
    Ok(oid.to_owned())
}

fn read_exact_ref(
    repo_root: &Path,
    reference: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<ExactRefReadV1, ManualBranchActivationError> {
    let output = run_git_with_control(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            reference,
        ],
        command_control,
    )
    .map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot read exact Git ref '{reference}': {error}"
        ))
    })?;
    match output.status.code() {
        Some(0) => Ok(ExactRefReadV1::Present(validated_git_oid(
            &output.stdout,
            reference,
        )?)),
        Some(1) => Ok(ExactRefReadV1::Absent),
        _ => Err(ManualBranchActivationError::git_unavailable(format!(
            "cannot read exact Git ref '{reference}': Git exited with {}",
            output.status
        ))),
    }
}

fn read_worktree_head(
    worktree: &Path,
    command_control: &PrCommandControlV1,
) -> std::result::Result<ExactRefReadV1, ManualBranchActivationError> {
    let output = run_git_with_control(
        worktree,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        command_control,
    )
    .map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot read manual worktree HEAD '{}': {error}",
            worktree.display()
        ))
    })?;
    match output.status.code() {
        Some(0) => Ok(ExactRefReadV1::Present(validated_git_oid(
            &output.stdout,
            "HEAD",
        )?)),
        Some(1) => Ok(ExactRefReadV1::Absent),
        _ => Err(ManualBranchActivationError::git_unavailable(format!(
            "cannot read manual worktree HEAD '{}': Git exited with {}",
            worktree.display(),
            output.status
        ))),
    }
}

fn read_worktree_branch(
    worktree: &Path,
    command_control: &PrCommandControlV1,
) -> std::result::Result<Option<String>, ManualBranchActivationError> {
    let output = run_git_with_control(worktree, &["symbolic-ref", "-q", "HEAD"], command_control)
        .map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot read manual worktree branch '{}': {error}",
            worktree.display()
        ))
    })?;
    match output.status.code() {
        Some(0) => {
            let reference = std::str::from_utf8(&output.stdout).map_err(|error| {
                ManualBranchActivationError::git_unavailable(format!(
                    "Git returned non-UTF-8 symbolic ref for '{}': {error}",
                    worktree.display()
                ))
            })?;
            let reference = reference.trim();
            if reference.is_empty() {
                return Err(ManualBranchActivationError::git_unavailable(format!(
                    "Git returned an empty symbolic ref for '{}'",
                    worktree.display()
                )));
            }
            Ok(Some(reference.to_owned()))
        }
        Some(1) => Ok(None),
        _ => Err(ManualBranchActivationError::git_unavailable(format!(
            "cannot read manual worktree branch '{}': Git exited with {}",
            worktree.display(),
            output.status
        ))),
    }
}

fn exact_ref_ownership(
    repo_root: &Path,
    reference: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<ManualBranchArtifactOwnershipV1, ManualBranchActivationError> {
    match read_exact_ref(repo_root, reference, command_control)? {
        ExactRefReadV1::Absent => Ok(ManualBranchArtifactOwnershipV1::Absent),
        ExactRefReadV1::Present(head) if head == expected_head => {
            Ok(ManualBranchArtifactOwnershipV1::Exact)
        }
        ExactRefReadV1::Present(_) => Ok(ManualBranchArtifactOwnershipV1::Foreign),
    }
}

pub fn manual_branch_artifact_ownership(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<ManualBranchArtifactOwnershipV1, ManualBranchActivationError> {
    let branch_ref = format!("refs/heads/{label}");
    let tracking = exact_ref_ownership(repo_root, tracking_ref, expected_head, command_control)?;
    let branch = exact_ref_ownership(repo_root, &branch_ref, expected_head, command_control)?;
    let worktree = if checked_path_exists(worktree)? {
        if worktree_matches_branch_head(
            repo_root,
            worktree,
            &branch_ref,
            expected_head,
            command_control,
        )? {
            ManualBranchArtifactOwnershipV1::Exact
        } else {
            ManualBranchArtifactOwnershipV1::Foreign
        }
    } else {
        ManualBranchArtifactOwnershipV1::Absent
    };
    if [tracking, branch, worktree]
        .into_iter()
        .any(|ownership| ownership == ManualBranchArtifactOwnershipV1::Foreign)
    {
        Ok(ManualBranchArtifactOwnershipV1::Foreign)
    } else if [tracking, branch, worktree]
        .into_iter()
        .any(|ownership| ownership == ManualBranchArtifactOwnershipV1::Exact)
    {
        Ok(ManualBranchArtifactOwnershipV1::Exact)
    } else {
        Ok(ManualBranchArtifactOwnershipV1::Absent)
    }
}

pub fn cleanup_owned_worktree(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<bool, ManualBranchActivationError> {
    let branch_ref = format!("refs/heads/{label}");
    match manual_branch_artifact_ownership(
        repo_root,
        worktree,
        tracking_ref,
        label,
        expected_head,
        command_control,
    )? {
        ManualBranchArtifactOwnershipV1::Absent => return Ok(true),
        ManualBranchArtifactOwnershipV1::Foreign => return Ok(false),
        ManualBranchArtifactOwnershipV1::Exact => {}
    }
    if checked_path_exists(worktree)? {
        if !worktree_matches_branch_head(
            repo_root,
            worktree,
            &branch_ref,
            expected_head,
            command_control,
        )? {
            return Ok(false);
        }
        remove_owned_manual_worktree(repo_root, worktree, command_control)?;
    }
    match exact_ref_ownership(repo_root, &branch_ref, expected_head, command_control)? {
        ManualBranchArtifactOwnershipV1::Exact => {
            delete_exact_ref(repo_root, &branch_ref, expected_head, command_control)?;
        }
        ManualBranchArtifactOwnershipV1::Foreign => return Ok(false),
        ManualBranchArtifactOwnershipV1::Absent => {}
    }
    if exact_ref_ownership(repo_root, &branch_ref, expected_head, command_control)?
        != ManualBranchArtifactOwnershipV1::Absent
    {
        return Ok(false);
    }
    match exact_ref_ownership(repo_root, tracking_ref, expected_head, command_control)? {
        ManualBranchArtifactOwnershipV1::Exact => {
            delete_exact_ref(repo_root, tracking_ref, expected_head, command_control)?;
        }
        ManualBranchArtifactOwnershipV1::Foreign => return Ok(false),
        ManualBranchArtifactOwnershipV1::Absent => {}
    }
    Ok(manual_branch_artifact_ownership(
        repo_root,
        worktree,
        tracking_ref,
        label,
        expected_head,
        command_control,
    )? == ManualBranchArtifactOwnershipV1::Absent)
}

pub fn manual_branch_artifacts_match(
    repo_root: &Path,
    artifacts: &ManualBranchArtifactsV1,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<bool, ManualBranchActivationError> {
    let branch_ref = format!("refs/heads/{}", artifacts.label);
    Ok(exact_ref_ownership(
        repo_root,
        &artifacts.tracking_ref,
        expected_head,
        command_control,
    )? == ManualBranchArtifactOwnershipV1::Exact
        && exact_ref_ownership(repo_root, &branch_ref, expected_head, command_control)?
            == ManualBranchArtifactOwnershipV1::Exact
        && worktree_matches_branch_head(
            repo_root,
            &artifacts.worktree,
            &branch_ref,
            expected_head,
            command_control,
        )?)
}

fn worktree_matches_branch_head(
    _repo_root: &Path,
    worktree: &Path,
    branch_ref: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<bool, ManualBranchActivationError> {
    if !checked_path_exists(worktree)? {
        return Ok(false);
    }
    Ok(matches!(
        read_worktree_head(worktree, command_control)?,
        ExactRefReadV1::Present(head) if head == expected_head
    ) && read_worktree_branch(worktree, command_control)?.as_deref() == Some(branch_ref))
}

fn delete_exact_ref(
    repo_root: &Path,
    reference: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), ManualBranchActivationError> {
    let output = run_git_with_control(
        repo_root,
        &["update-ref", "-d", reference, expected_head],
        command_control,
    )
    .map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot delete exact Git ref '{reference}': {error}"
        ))
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ManualBranchActivationError::activation_failed(format!(
        "cannot delete exact Git ref '{reference}': Git exited with {}",
        output.status
    )))
}

fn remove_owned_manual_worktree(
    repo_root: &Path,
    worktree: &Path,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), ManualBranchActivationError> {
    if !checked_path_exists(worktree)? {
        return Ok(());
    }
    let worktree_arg = worktree.to_string_lossy();
    for arguments in [
        vec!["worktree", "remove", "--force", &worktree_arg],
        vec!["worktree", "prune"],
    ] {
        let output =
            run_git_with_control(repo_root, &arguments, command_control).map_err(|error| {
                ManualBranchActivationError::git_unavailable(format!(
                    "cannot remove manual worktree '{}': {error}",
                    worktree.display()
                ))
            })?;
        if !output.status.success() {
            return Err(ManualBranchActivationError::activation_failed(format!(
                "cannot remove manual worktree '{}': Git exited with {}",
                worktree.display(),
                output.status
            )));
        }
    }
    if checked_path_exists(worktree)? {
        std::fs::remove_dir_all(worktree).map_err(|error| {
            ManualBranchActivationError::git_unavailable(format!(
                "cannot remove manual worktree directory '{}': {error}",
                worktree.display()
            ))
        })?;
    }
    if checked_path_exists(worktree)? {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "manual worktree '{}' remained after exact removal",
            worktree.display()
        )));
    }
    Ok(())
}

pub fn prepare_pr_worktree(
    repo_root: &Path,
    worktree: &Path,
    pr_number: u64,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), String> {
    let pr_ref_spec = format!("+refs/pull/{pr_number}/head:{tracking_ref}");
    successful_git_with_control(
        repo_root,
        &["fetch", "--no-tags", "origin", &pr_ref_spec],
        command_control,
    )
    .map_err(|error| format!("fetch of PR head failed: {error}"))?;
    let fetched_head =
        successful_git_with_control(repo_root, &["rev-parse", tracking_ref], command_control)
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|sha| sha.trim().to_string());
    if fetched_head.as_deref() != Some(expected_head) {
        return Err("PR head changed during reconciliation".to_string());
    }

    checkout_linked_worktree(repo_root, worktree, tracking_ref, label, command_control)
}

fn checkout_linked_worktree(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), String> {
    if let Some(parent) = worktree.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    remove_worktree(repo_root, worktree, command_control)
        .map_err(|error| format!("worktree replacement cleanup failed: {error}"))?;

    let wt_str = worktree.to_string_lossy();
    successful_git_with_control(
        repo_root,
        &[
            "worktree",
            "add",
            "-B",
            label,
            "--force",
            &wt_str,
            tracking_ref,
        ],
        command_control,
    )
    .map_err(|error| format!("worktree add failed: {error}"))?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrCleanupArtifact {
    Worktree(PathBuf),
    Branch(String),
    TrackingRef(String),
}

impl std::fmt::Display for PrCleanupArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Worktree(path) => write!(formatter, "worktree '{}'", path.display()),
            Self::Branch(reference) => write!(formatter, "branch '{reference}'"),
            Self::TrackingRef(reference) => write!(formatter, "tracking ref '{reference}'"),
        }
    }
}

#[derive(Debug)]
pub struct PrCleanupReceipt(());

#[derive(Debug, thiserror::Error)]
pub enum PrCleanupError {
    #[error("PR cleanup task failed to join: {0}")]
    Join(String),
    #[error("PR cleanup command failed for {artifact}: {source}")]
    Command {
        artifact: PrCleanupArtifact,
        #[source]
        source: PrGitCommandError,
    },
    #[error(
        "PR cleanup did not remove owned artifacts: {}",
        .0.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    Remaining(Vec<PrCleanupArtifact>),
}

#[hotpath::measure(label = "application.pr_tracking.cleanup_worktree")]
pub fn cleanup_pr_worktree(
    repo_root: &Path,
    data_root: &Path,
    pr: u64,
    expected_head: &str,
    remove_synthetic_branch: bool,
    command_control: &PrCommandControlV1,
) -> std::result::Result<PrCleanupReceipt, PrCleanupError> {
    let worktree = data_root.join("pr-worktrees").join(format!("pr-{pr}"));
    let tracking_ref = pr_tracking_ref(pr);
    let label = pr_label(pr);
    let branch_ref = format!("refs/heads/{label}");
    let artifacts = || {
        let mut artifacts = vec![
            PrCleanupArtifact::Worktree(worktree.clone()),
            PrCleanupArtifact::TrackingRef(tracking_ref.clone()),
        ];
        if remove_synthetic_branch {
            artifacts.push(PrCleanupArtifact::Branch(branch_ref.clone()));
        }
        artifacts
    };
    if command_control.is_cancelled() {
        return Err(PrCleanupError::Remaining(artifacts()));
    }
    let owned_head = if expected_head.is_empty() {
        let ref_head = ref_sha(repo_root, &tracking_ref, command_control)?;
        let worktree_head = ref_sha(&worktree, "HEAD", command_control)?;
        match (ref_head, worktree_head) {
            (Some(ref_head), Some(worktree_head)) if ref_head == worktree_head => Some(ref_head),
            _ => None,
        }
    } else {
        Some(expected_head.to_string())
    };
    remove_worktree(repo_root, &worktree, command_control).map_err(|source| {
        PrCleanupError::Command {
            artifact: PrCleanupArtifact::Worktree(worktree.clone()),
            source,
        }
    })?;
    if let Some(owned_head) = owned_head {
        if remove_synthetic_branch
            && ref_points_to(repo_root, &branch_ref, &owned_head, command_control)?
        {
            successful_git_with_control(repo_root, &["branch", "-D", &label], command_control)
                .map_err(|source| PrCleanupError::Command {
                    artifact: PrCleanupArtifact::Branch(branch_ref.clone()),
                    source,
                })?;
        }
        if ref_points_to(repo_root, &tracking_ref, &owned_head, command_control)? {
            successful_git_with_control(
                repo_root,
                &["update-ref", "-d", &tracking_ref],
                command_control,
            )
            .map_err(|source| PrCleanupError::Command {
                artifact: PrCleanupArtifact::TrackingRef(tracking_ref.clone()),
                source,
            })?;
        }
    }
    let verification_control = PrCommandControlV1::default();
    let remaining = remaining_pr_artifacts(
        repo_root,
        &worktree,
        remove_synthetic_branch.then_some(branch_ref.as_str()),
        &tracking_ref,
        &verification_control,
    )?;
    if !remaining.is_empty() {
        return Err(PrCleanupError::Remaining(remaining));
    }
    Ok(PrCleanupReceipt(()))
}

fn ref_points_to(
    repo_root: &Path,
    reference: &str,
    expected_head: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<bool, PrCleanupError> {
    Ok(ref_sha(repo_root, reference, command_control)?.is_some_and(|sha| sha == expected_head))
}

fn ref_sha(
    repo_root: &Path,
    reference: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<Option<String>, PrCleanupError> {
    let output = run_git_with_control(
        repo_root,
        &["rev-parse", "--verify", "--end-of-options", reference],
        command_control,
    )
    .map_err(|source| PrCleanupError::Command {
        artifact: cleanup_artifact_for_ref(reference),
        source: PrGitCommandError::Command(source),
    })?;
    if !output.status.success() {
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .map(|sha| Some(sha.trim().to_owned()))
        .map_err(|source| PrCleanupError::Command {
            artifact: cleanup_artifact_for_ref(reference),
            source: PrGitCommandError::InvalidOutput {
                arguments: format!("rev-parse --verify --end-of-options {reference}"),
                detail: source.to_string(),
            },
        })
}

fn cleanup_artifact_for_ref(reference: &str) -> PrCleanupArtifact {
    if reference.starts_with("refs/heads/") {
        PrCleanupArtifact::Branch(reference.to_owned())
    } else {
        PrCleanupArtifact::TrackingRef(reference.to_owned())
    }
}

fn remaining_pr_artifacts(
    repo_root: &Path,
    worktree: &Path,
    branch_ref: Option<&str>,
    tracking_ref: &str,
    command_control: &PrCommandControlV1,
) -> std::result::Result<Vec<PrCleanupArtifact>, PrCleanupError> {
    let worktrees = successful_git_with_control(
        repo_root,
        &["worktree", "list", "--porcelain"],
        command_control,
    )
    .map_err(|source| PrCleanupError::Command {
        artifact: PrCleanupArtifact::Worktree(worktree.to_owned()),
        source,
    })?;
    let listed = String::from_utf8(worktrees.stdout).map_err(|source| PrCleanupError::Command {
        artifact: PrCleanupArtifact::Worktree(worktree.to_owned()),
        source: PrGitCommandError::InvalidOutput {
            arguments: "worktree list --porcelain".to_owned(),
            detail: source.to_string(),
        },
    })?;
    let mut remaining = Vec::new();
    if worktree.exists()
        || listed
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|listed| Path::new(listed) == worktree)
    {
        remaining.push(PrCleanupArtifact::Worktree(worktree.to_owned()));
    }
    if let Some(branch_ref) = branch_ref
        && ref_sha(repo_root, branch_ref, command_control)?.is_some()
    {
        remaining.push(PrCleanupArtifact::Branch(branch_ref.to_owned()));
    }
    if ref_sha(repo_root, tracking_ref, command_control)?.is_some() {
        remaining.push(PrCleanupArtifact::TrackingRef(tracking_ref.to_owned()));
    }
    Ok(remaining)
}

fn remove_worktree(
    repo_root: &Path,
    worktree: &Path,
    command_control: &PrCommandControlV1,
) -> std::result::Result<(), PrGitCommandError> {
    let wt_str = worktree.to_string_lossy();
    match successful_git_with_control(
        repo_root,
        &["worktree", "remove", "--force", &wt_str],
        command_control,
    ) {
        Ok(_) => {}
        Err(_) if !worktree.exists() => {}
        Err(error) => return Err(error),
    }
    successful_git_with_control(repo_root, &["worktree", "prune"], command_control)?;
    if command_control.is_cancelled() {
        return Err(PrGitCommandError::Command(
            tracedecay_runtime_core::git::GitCommandError::Cancelled,
        ));
    }
    if worktree.exists() {
        std::fs::remove_dir_all(worktree).map_err(|source| {
            PrGitCommandError::Command(tracedecay_runtime_core::git::GitCommandError::Wait(source))
        })?;
    }
    Ok(())
}

pub async fn cleanup_owned_worktree_off_runtime(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: PrCommandControlV1,
) -> std::result::Result<bool, ManualBranchActivationError> {
    let repo_root = repo_root.to_path_buf();
    let worktree = worktree.to_path_buf();
    let tracking_ref = tracking_ref.to_owned();
    let label = label.to_owned();
    let expected_head = expected_head.to_owned();
    tokio::task::spawn_blocking(move || {
        cleanup_owned_worktree(
            &repo_root,
            &worktree,
            &tracking_ref,
            &label,
            &expected_head,
            &command_control,
        )
    })
    .await
    .map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch cleanup task did not complete: {error}"
        ))
    })?
}

pub async fn manual_branch_artifact_ownership_off_runtime(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
    command_control: PrCommandControlV1,
) -> std::result::Result<ManualBranchArtifactOwnershipV1, ManualBranchActivationError> {
    let repo_root = repo_root.to_path_buf();
    let worktree = worktree.to_path_buf();
    let tracking_ref = tracking_ref.to_owned();
    let label = label.to_owned();
    let expected_head = expected_head.to_owned();
    tokio::task::spawn_blocking(move || {
        manual_branch_artifact_ownership(
            &repo_root,
            &worktree,
            &tracking_ref,
            &label,
            &expected_head,
            &command_control,
        )
    })
    .await
    .map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch ownership check task did not complete: {error}"
        ))
    })?
}

pub async fn manual_branch_artifacts_match_off_runtime(
    repo_root: &Path,
    artifacts: &ManualBranchArtifactsV1,
    expected_head: &str,
    command_control: PrCommandControlV1,
) -> std::result::Result<bool, ManualBranchActivationError> {
    let repo_root = repo_root.to_path_buf();
    let artifacts = artifacts.clone();
    let expected_head = expected_head.to_owned();
    tokio::task::spawn_blocking(move || {
        manual_branch_artifacts_match(&repo_root, &artifacts, &expected_head, &command_control)
    })
    .await
    .map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch exactness inspection task did not complete: {error}"
        ))
    })?
}

pub async fn cleanup_pr_worktree_off_runtime(
    repo_root: &Path,
    data_root: &Path,
    pr: u64,
    expected_head: &str,
    remove_synthetic_branch: bool,
    command_control: PrCommandControlV1,
) -> std::result::Result<PrCleanupReceipt, PrCleanupError> {
    let repo_root = repo_root.to_path_buf();
    let data_root = data_root.to_path_buf();
    let expected_head = expected_head.to_owned();
    tokio::task::spawn_blocking(move || {
        cleanup_pr_worktree(
            &repo_root,
            &data_root,
            pr,
            &expected_head,
            remove_synthetic_branch,
            &command_control,
        )
    })
    .await
    .map_err(|error| PrCleanupError::Join(error.to_string()))?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        ManualBranchActivationError, ManualBranchArtifactsV1, PrCommandControlV1,
        cleanup_owned_worktree, prepare_manual_branch_worktree, ref_points_to, remove_worktree,
        resolve_branch_head, successful_git_with_control,
    };
    use crate::pr_tracking::default_pr_command_control;
    use std::path::Path;
    use std::time::Duration;
    #[test]
    fn manual_artifact_cleanup_accepts_absence_but_refuses_foreign_provenance() {
        let repo = tempfile::tempdir().unwrap();
        let branch = "feature/exact-cleanup";
        init_manual_branch_repo(repo.path(), branch);
        let data = tempfile::tempdir().unwrap();
        let head = resolve_branch_head(repo.path(), branch, default_pr_command_control())
            .expect("feature branch head");
        let artifacts = ManualBranchArtifactsV1::for_head(data.path(), branch, &head);

        prepare_manual_branch_worktree(
            repo.path(),
            &artifacts.worktree,
            &artifacts.tracking_ref,
            &artifacts.label,
            &head,
            default_pr_command_control(),
        )
        .expect("prepare exact worktree");
        assert!(
            cleanup_owned_worktree(
                repo.path(),
                &artifacts.worktree,
                &artifacts.tracking_ref,
                &artifacts.label,
                &head,
                default_pr_command_control(),
            )
            .expect("exact cleanup")
        );
        assert!(
            cleanup_owned_worktree(
                repo.path(),
                &artifacts.worktree,
                &artifacts.tracking_ref,
                &artifacts.label,
                &head,
                default_pr_command_control(),
            )
            .expect("absent artifacts are an idempotent success")
        );

        prepare_manual_branch_worktree(
            repo.path(),
            &artifacts.worktree,
            &artifacts.tracking_ref,
            &artifacts.label,
            &head,
            default_pr_command_control(),
        )
        .expect("prepare replacement exact worktree");
        let foreign = resolve_branch_head(repo.path(), "main", default_pr_command_control())
            .expect("main branch head");
        assert_ne!(foreign, head, "fixture branches must have distinct heads");
        assert!(
            successful_git_with_control(
                repo.path(),
                &["update-ref", &artifacts.tracking_ref, &foreign],
                default_pr_command_control(),
            )
            .is_ok()
        );

        assert!(
            !cleanup_owned_worktree(
                repo.path(),
                &artifacts.worktree,
                &artifacts.tracking_ref,
                &artifacts.label,
                &head,
                default_pr_command_control(),
            )
            .expect("foreign provenance must be a typed false result"),
            "foreign ref replacement must survive an exact-source cleanup"
        );
        assert!(
            ref_points_to(
                repo.path(),
                &artifacts.tracking_ref,
                &foreign,
                default_pr_command_control(),
            )
            .expect("foreign tracking ref remains readable"),
            "the foreign tracking ref must remain untouched"
        );
        assert!(
            artifacts.worktree.exists(),
            "a foreign provenance mismatch must not delete the linked worktree"
        );
    }

    #[test]
    fn manual_artifact_cleanup_keeps_exact_refs_when_git_authority_is_unavailable() {
        let repo = tempfile::tempdir().unwrap();
        let branch = "feature/retry-after-git-failure";
        init_manual_branch_repo(repo.path(), branch);
        let data = tempfile::tempdir().unwrap();
        let head = resolve_branch_head(repo.path(), branch, default_pr_command_control())
            .expect("feature branch head");
        let artifacts = ManualBranchArtifactsV1::for_head(data.path(), branch, &head);
        let branch_ref = format!("refs/heads/{}", artifacts.label);

        prepare_manual_branch_worktree(
            repo.path(),
            &artifacts.worktree,
            &artifacts.tracking_ref,
            &artifacts.label,
            &head,
            default_pr_command_control(),
        )
        .expect("prepare exact worktree");
        remove_worktree(
            repo.path(),
            &artifacts.worktree,
            default_pr_command_control(),
        )
        .expect("remove exact worktree");
        assert!(
            !artifacts.worktree.try_exists().expect("inspect worktree"),
            "the sealed ref retry begins after the linked worktree is absent"
        );

        let unavailable = PrCommandControlV1::with_timeout(Duration::ZERO);
        let error = cleanup_owned_worktree(
            repo.path(),
            &artifacts.worktree,
            &artifacts.tracking_ref,
            &artifacts.label,
            &head,
            &unavailable,
        )
        .expect_err("unavailable Git must not be collapsed into an absent ref");
        assert!(matches!(
            &error,
            ManualBranchActivationError::GitAuthorityUnavailable { .. }
        ));
        assert!(
            error.retryable(),
            "a bounded exact-ref read timeout must remain retryable"
        );
        assert_eq!(error.reason_code(), "git_authority_unavailable");
        assert!(
            git_ref_exists(repo.path(), &artifacts.tracking_ref)
                && git_ref_exists(repo.path(), &branch_ref),
            "a failed exact read must retain the sealed reference proof for retry"
        );

        assert!(
            cleanup_owned_worktree(
                repo.path(),
                &artifacts.worktree,
                &artifacts.tracking_ref,
                &artifacts.label,
                &head,
                default_pr_command_control(),
            )
            .expect("restored Git authority must complete exact cleanup")
        );
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn init_manual_branch_repo(repo: &Path, branch: &str) {
        // Pin the files ref backend. This suite's exact-ref coverage opens the
        // loose ref file directly, which a reftable repository never materializes.
        // Git versions that predate `--ref-format` reject the option and already
        // create files-backed repositories.
        if !git_succeeds(repo, &["init", "-q", "-b", "main", "--ref-format=files"]) {
            git(repo, &["init", "-q", "-b", "main"]);
        }
        git(repo, &["config", "user.name", "TraceDecay Test"]);
        git(
            repo,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", "initial"]);
        git(repo, &["checkout", "-q", "-b", branch, "main"]);
        std::fs::write(repo.join("src/feature.rs"), "pub fn on_feature() {}\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", "feature content"]);
        git(repo, &["checkout", "-q", "main"]);
    }

    fn git_ref_exists(repo: &Path, reference: &str) -> bool {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--end-of-options", reference])
            .current_dir(repo)
            .output()
            .is_ok_and(|output| output.status.success())
    }
}
