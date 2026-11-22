//! One-way retirement of recorded automatic PR worktrees.
//!
//! This owner runs once at daemon startup. It never discovers remote PRs,
//! creates a checkout, or opens a project index. Only artifacts named by the
//! old ledger can be considered; unsafe entries remain with an exact reason
//! for doctor. Shared project stores and session history are not targets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_application::pr_tracking::{
    ManualBranchActivationError, PrCommandControlV1, refs_are_unattached_outside_worktree,
    successful_git_with_control, try_acquire_manual_branch_lifecycle,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};
use tracedecay_runtime_core::storage::{
    PrivateStoreIo, profile_sharded_data_root, reject_symlink_components,
};

use super::branch_admin::StoreAdministration;

const STATE_FILENAME: &str = "pr-autotrack.json";
const PROJECT_PAGE_SIZE: usize = 64;
// This retired control file cannot authorize unbounded startup allocation.
// Larger records stay untouched and are reported for operator inspection.
const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyManagedPr {
    pr: u64,
    head_branch: String,
    head_sha: String,
    worktree: PathBuf,
    tracking_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retirement_reason: Option<PreservedReason>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPrState {
    #[serde(default)]
    managed: BTreeMap<String, LegacyManagedPr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreservedReason {
    Dirty,
    Foreign,
    Leased,
    Cancelled,
    Unpublished,
    Unavailable,
}

impl PreservedReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dirty => "dirty",
            Self::Foreign => "foreign",
            Self::Leased => "leased",
            Self::Cancelled => "cancelled",
            Self::Unpublished => "unpublished commits",
            Self::Unavailable => "ownership evidence unavailable",
        }
    }
}

#[derive(Default)]
struct LegacyPrRetirementReport {
    retired: Vec<String>,
    preserved: Vec<(String, PreservedReason)>,
}

fn load_state(data_root: &Path) -> Result<LegacyPrState, String> {
    let path = data_root.join(STATE_FILENAME);
    reject_symlink_components(&path, "legacy PR ownership")
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let Some(bytes) = tracedecay_private_fs::framed_log::read_bounded(&path, MAX_STATE_BYTES)
        .map_err(|error| format!("{}: {error}", path.display()))?
    else {
        return Ok(LegacyPrState::default());
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The only remaining ledger writer shrinks old ownership or records why an
/// entry was preserved. There is no API to add automatic PR ownership.
fn persist_retirement(data_root: &Path, state: &LegacyPrState) -> Result<(), String> {
    let path = data_root.join(STATE_FILENAME);
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    PrivateStoreIo::write_file_atomically(&path, &path.with_extension("json.tmp"), &bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn legacy_pr_artifacts_finding(data_root: &Path) -> Option<String> {
    match load_state(data_root) {
        Err(error) => Some(format!(
            "Automatic PR tracking is retired; its recorded ownership cannot be read ({error}). \
             Preserve the worktrees and refs until the ownership record can be inspected."
        )),
        Ok(state) if state.managed.is_empty() => None,
        Ok(state) => Some(format!(
            "Automatic PR tracking is retired. Preserved artifacts: {}. \
             Resolve each preservation reason and restart the daemon to retry; \
             explicit branches remain available through `tracedecay branch add`.",
            state
                .managed
                .iter()
                .map(|(label, managed)| format!(
                    "{label}: worktree '{}', ref '{}' ({})",
                    managed.worktree.display(),
                    managed.tracking_ref,
                    managed
                        .retirement_reason
                        .map_or("awaiting retirement", PreservedReason::as_str),
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

fn git_text(
    root: &Path,
    args: &[&str],
    control: &PrCommandControlV1,
) -> Result<String, PreservedReason> {
    if control.is_cancelled() {
        return Err(PreservedReason::Cancelled);
    }
    let output = successful_git_with_control(root, args, control).map_err(|_| {
        if control.is_cancelled() {
            PreservedReason::Cancelled
        } else {
            PreservedReason::Unavailable
        }
    })?;
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| PreservedReason::Unavailable)
}

fn exact_commit(
    root: &Path,
    reference: &str,
    control: &PrCommandControlV1,
) -> Result<Option<String>, PreservedReason> {
    if control.is_cancelled() {
        return Err(PreservedReason::Cancelled);
    }
    let repository =
        tracedecay_runtime_core::git_open::open(root).map_err(|_| PreservedReason::Unavailable)?;
    let Some(reference) = repository
        .try_find_reference(reference)
        .map_err(|_| PreservedReason::Unavailable)?
    else {
        return Ok(None);
    };
    // Ownership requires the exact direct commit ref. Peeling would accept
    // a substituted symbolic ref or tag, and cannot distinguish a missing
    // ref from a present non-commit object.
    let target = reference.target();
    let id = target.try_id().ok_or(PreservedReason::Foreign)?;
    let header = repository
        .find_header(id.to_owned())
        .map_err(|_| PreservedReason::Foreign)?;
    if !header.kind().is_commit() {
        return Err(PreservedReason::Foreign);
    }
    if control.is_cancelled() {
        return Err(PreservedReason::Cancelled);
    }
    Ok(Some(id.to_string()))
}

/// Validate the existing checkout before its HEAD participates in publication
/// proof. The caller has already required direct commit targets for its refs.
fn verify_clean_owned_worktree_head(
    repo: &Path,
    data_root: &Path,
    branch: &str,
    managed: &LegacyManagedPr,
    control: &PrCommandControlV1,
) -> Result<String, PreservedReason> {
    let canonical_data = data_root
        .canonicalize()
        .map_err(|_| PreservedReason::Unavailable)?;
    let canonical_worktree = managed
        .worktree
        .canonicalize()
        .map_err(|_| PreservedReason::Unavailable)?;
    if canonical_worktree
        != canonical_data
            .join("pr-worktrees")
            .join(format!("pr-{}", managed.pr))
    {
        return Err(PreservedReason::Foreign);
    }
    let common = git_text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        control,
    )?;
    if git_text(
        &managed.worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        control,
    )? != common
        || git_text(
            &managed.worktree,
            &["symbolic-ref", "--quiet", "HEAD"],
            control,
        )? != branch
    {
        return Err(PreservedReason::Foreign);
    }
    let git_dir = git_text(
        &managed.worktree,
        &["rev-parse", "--absolute-git-dir"],
        control,
    )?;
    if Path::new(&git_dir)
        .join("locked")
        .try_exists()
        .map_err(|_| PreservedReason::Unavailable)?
    {
        return Err(PreservedReason::Leased);
    }
    if !git_text(
        &managed.worktree,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
        ],
        control,
    )?
    .is_empty()
    {
        return Err(PreservedReason::Dirty);
    }
    // HEAD is the one intentional symbolic ref: its exact branch was
    // checked above, while the branch itself must be a direct commit.
    Ok(tracedecay_runtime_core::git_open::open(&managed.worktree)
        .map_err(|_| PreservedReason::Unavailable)?
        .head_commit()
        .map_err(|_| PreservedReason::Unavailable)?
        .id
        .to_string())
}

/// Rechecked immediately before mutation under the exact lifecycle lease.
/// A clean worktree alone is insufficient: every disappearing ref must also
/// be reachable through a retained remote-tracking ref.
fn verify_owned_published_artifacts(
    repo: &Path,
    data_root: &Path,
    label: &str,
    managed: &LegacyManagedPr,
    control: &PrCommandControlV1,
) -> Result<(bool, Option<String>, Option<String>), PreservedReason> {
    let expected = data_root
        .join("pr-worktrees")
        .join(format!("pr-{}", managed.pr));
    if label != format!("tracedecay/autotrack/pr/{}", managed.pr)
        || managed.tracking_ref != format!("refs/tracedecay/pr/{}", managed.pr)
        || managed.worktree != expected
        || managed.head_sha.is_empty()
    {
        return Err(PreservedReason::Foreign);
    }
    let branch = format!("refs/heads/{label}");
    let branch_head = exact_commit(repo, &branch, control)?;
    let tracking_head = exact_commit(repo, &managed.tracking_ref, control)?;
    let exists = match std::fs::symlink_metadata(&managed.worktree) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(PreservedReason::Foreign);
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(PreservedReason::Unavailable),
    };
    if !refs_are_unattached_outside_worktree(
        repo,
        &[&branch, &managed.tracking_ref],
        Some(&managed.worktree),
        control,
    )
    .map_err(|_| {
        if control.is_cancelled() {
            PreservedReason::Cancelled
        } else {
            PreservedReason::Unavailable
        }
    })? {
        return Err(PreservedReason::Foreign);
    }
    if !exists {
        // A missing checkout can still have a locked or detached Git
        // registration. Keep that record for operator inspection; never
        // clear ownership merely because the working directory disappeared.
        let repository = tracedecay_runtime_core::git_open::open(repo)
            .map_err(|_| PreservedReason::Unavailable)?;
        let mut still_registered = false;
        tracedecay_runtime_core::git_open::visit_linked_worktrees(&repository, |_, root| {
            still_registered |= root == managed.worktree.as_path();
            Ok(())
        })
        .map_err(|_| PreservedReason::Unavailable)?;
        if still_registered {
            return Err(PreservedReason::Unavailable);
        }
    }
    let worktree_head = if exists {
        Some(verify_clean_owned_worktree_head(
            repo, data_root, &branch, managed, control,
        )?)
    } else {
        None
    };
    for head in branch_head
        .iter()
        .chain(tracking_head.iter())
        .chain(worktree_head.iter())
        .map(String::as_str)
    {
        if git_text(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(symref)",
                "--contains",
                head,
                "refs/remotes/",
            ],
            control,
        )?
        .lines()
        .all(|line| {
            !line
                .split_once('\0')
                .is_some_and(|(_, target)| target.is_empty())
        }) {
            // A remote alias back to this managed branch would become
            // dangling when the branch retires. Only a direct remote ref
            // proves a retained anchor independent of these artifacts.
            return Err(PreservedReason::Unpublished);
        }
        if head != managed.head_sha {
            return Err(PreservedReason::Foreign);
        }
    }
    Ok((exists, branch_head, tracking_head))
}

fn remove_owned_published_artifacts(
    repo: &Path,
    data_root: &Path,
    label: &str,
    managed: &LegacyManagedPr,
    control: &PrCommandControlV1,
) -> Result<(), PreservedReason> {
    let (exists, branch_head, tracking_head) =
        verify_owned_published_artifacts(repo, data_root, label, managed, control)?;
    if exists {
        // No --force and no recursive filesystem fallback: Git refuses a
        // dirty or locked checkout even if it changed after our proof.
        git_text(
            repo,
            &["worktree", "remove", &managed.worktree.to_string_lossy()],
            control,
        )?;
    }
    // Removing the old checkout releases its branch for ordinary Git users.
    // Recheck the complete attachment census before either ref is deleted.
    let branch = format!("refs/heads/{label}");
    if !refs_are_unattached_outside_worktree(repo, &[&branch, &managed.tracking_ref], None, control)
        .map_err(|_| {
            if control.is_cancelled() {
                PreservedReason::Cancelled
            } else {
                PreservedReason::Unavailable
            }
        })?
    {
        return Err(PreservedReason::Foreign);
    }
    for (reference, head) in [
        (branch, branch_head),
        (managed.tracking_ref.clone(), tracking_head),
    ] {
        if let Some(head) = head {
            delete_exact_commit_ref(repo, &reference, &head, control)?;
        }
    }
    Ok(())
}

fn delete_exact_commit_ref(
    repo: &Path,
    reference: &str,
    head: &str,
    control: &PrCommandControlV1,
) -> Result<(), PreservedReason> {
    // Never follow a concurrently substituted symbolic ref into a user
    // branch. The expected OID also guards a moved direct ref.
    git_text(
        repo,
        &["update-ref", "--no-deref", "-d", reference, head],
        control,
    )
    .map(|_| ())
}

async fn retire_project(
    repo: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    cancellation: &CancellationToken,
) -> Result<LegacyPrRetirementReport, String> {
    let mut state = load_state(data_root)?;
    let mut report = LegacyPrRetirementReport::default();
    for (label, managed) in state.managed.clone() {
        if cancellation.is_cancelled() {
            break;
        }
        let outcome =
            retire_entry(repo, data_root, schedulers, cancellation, &label, &managed).await;
        match outcome {
            Ok(()) => {
                state.managed.remove(&label);
                report.retired.push(label);
            }
            Err(reason) => {
                if let Some(entry) = state.managed.get_mut(&label) {
                    entry.retirement_reason = Some(reason);
                }
                report.preserved.push((label, reason));
            }
        }
    }
    if !report.retired.is_empty() || !report.preserved.is_empty() {
        // A crash before this checkpoint only causes an idempotent retry.
        // Do not rewrite the complete ledger once per entry or during every
        // remaining iteration after cancellation.
        persist_retirement(data_root, &state)?;
    }
    Ok(report)
}

async fn retire_entry(
    repo: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    cancellation: &CancellationToken,
    label: &str,
    managed: &LegacyManagedPr,
) -> Result<(), PreservedReason> {
    if cancellation.is_cancelled() {
        return Err(PreservedReason::Cancelled);
    }
    let lifecycle = try_acquire_manual_branch_lifecycle(data_root, label)
        .await
        .map_err(|error| match error {
            ManualBranchActivationError::LifecycleContended { .. } => PreservedReason::Leased,
            _ => PreservedReason::Unavailable,
        })?;
    let repo = repo.to_path_buf();
    let data_root = data_root.to_path_buf();
    let label = label.to_owned();
    let managed = managed.clone();
    let control = PrCommandControlV1::with_cancellation(cancellation.clone());
    let (verified, lifecycle, repo, data_root, label, managed, control) =
        native_runtime::spawn_blocking(move || {
            let verified =
                verify_owned_published_artifacts(&repo, &data_root, &label, &managed, &control);
            (
                verified, lifecycle, repo, data_root, label, managed, control,
            )
        })
        .await
        .map_err(|_| PreservedReason::Unavailable)?;
    verified?;
    super::branch_worktrees::retire_worktree_mount(Some(schedulers), &managed.worktree)
        .await
        .map_err(|_| PreservedReason::Unavailable)?;
    native_runtime::spawn_blocking(move || {
        let _lifecycle = lifecycle;
        remove_owned_published_artifacts(&repo, &data_root, &label, &managed, &control)
    })
    .await
    .map_err(|_| PreservedReason::Unavailable)?
}

pub(crate) struct LegacyPrRetirementTask {
    cancellation: CancellationToken,
    task: NativeTask<()>,
}

impl LegacyPrRetirementTask {
    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) async fn shutdown(self) {
        self.cancellation.cancel();
        if let NativeTaskOutcome::Panicked(error) = self.task.wait().await {
            tracing::error!(%error, "legacy PR retirement task failed");
        }
    }
}

impl Drop for LegacyPrRetirementTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

pub(super) fn spawn_with_administration(
    administration: StoreAdministration,
    schedulers: CodeIndexSchedulerRegistryV1,
) -> Result<LegacyPrRetirementTask, String> {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = NativeTask::spawn(async move {
        if let Err(error) =
            retire_registered_projects(&administration, &schedulers, &task_cancellation).await
        {
            log_daemon_event("legacy_pr_retirement", &[("reason", error)]);
        }
    })?;
    Ok(LegacyPrRetirementTask { cancellation, task })
}

async fn retire_registered_projects(
    administration: &StoreAdministration,
    schedulers: &CodeIndexSchedulerRegistryV1,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let profile = administration
        .owner_profile()
        .map_err(|error| error.to_string())?;
    let database = administration
        .registered_profile_database()
        .await
        .map_err(|error| error.to_string())?;
    let mut after = None;
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let projects = database
            .list_code_projects_after(after.as_deref(), PROJECT_PAGE_SIZE)
            .await
            .map_err(|error| error.to_string())?;
        let exhausted = projects.len() < PROJECT_PAGE_SIZE;
        after = projects.last().map(|project| project.project_id.clone());
        for project in projects {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let root = PathBuf::from(project.canonical_root);
            let data_root = profile_sharded_data_root(profile.data_dir(), &project.project_id);
            match retire_project(&root, &data_root, schedulers, cancellation).await {
                Ok(report) if report.retired.is_empty() && report.preserved.is_empty() => {}
                Ok(report) => log_daemon_event(
                    "legacy_pr_retirement",
                    &[
                        ("project", root.display().to_string()),
                        ("retired", report.retired.len().to_string()),
                        ("preserved", report.preserved.len().to_string()),
                    ],
                ),
                Err(error) => log_daemon_event(
                    "legacy_pr_retirement",
                    &[("project", root.display().to_string()), ("reason", error)],
                ),
            }
        }
        if exhausted {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
