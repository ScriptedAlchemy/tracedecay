//! Daemon adaptation for PR-branch activation and reconciliation.
//!
//! [`tracedecay_application::pr_tracking`] owns Git discovery and managed state.
//! When a project enables `sync.auto_track_pr_branches`, this adapter activates
//! each discovered same-repository PR head as a registered linked worktree
//! through the daemon's retained code-index scheduler. The poll runtime and
//! daemon branch-add handler receive that authority and refuse Git or durable
//! state mutation when identity or Git discovery cannot name a worktree root.
//!
//! # Why worktrees
//!
//! A code-index scheduler captures a working tree rather than reading blobs
//! directly out of a git ref. So to index a PR head accurately the head must be
//! checked out somewhere. The retained-authority topology therefore uses a
//! deterministic local ref
//! (`refs/tracedecay/pr/<N>`), check it out into a linked worktree on a local
//! branch named `pr/<N>` under the store's `pr-worktrees/` dir. A branch
//! can only be checked out in one worktree at a time, so we never reuse the PR's
//! real head-branch name (which the user may have checked out); instead every
//! PR-managed entry is tracked under the synthetic label `pr/<N>`. That also keeps
//! PR-managed entries cleanly separable from the user's own tracked branches, so
//! we never untrack a branch a human added.
//!
//! # Scope decision: same-repo PRs only
//!
//! Fork PRs (head on a different repository) are **skipped** with a logged reason.
//! Discovery classifies a PR as a fork when its head SHA matches no `refs/heads/*`
//! ref on `origin` (or, via `gh`, when `isCrossRepository` is true). Supporting
//! forks would mean fetching untrusted `refs/pull/N/head` from arbitrary
//! repositories; that is deliberately out of scope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use tracedecay_application::pr_tracking::{
    DiscoveredPr, ManagedPr, ManualBranchActivation, ManualBranchActivationError,
    ManualBranchArtifactOwnershipV1, ManualBranchArtifactsV1, ManualBranchLifecycleLeaseV1,
    PrAutotrackState, PrCommandControlV1 as PrCommandControl, PrDiscovery, ReconcileReport,
    cleanup_owned_worktree, cleanup_owned_worktree_off_runtime, cleanup_pr_worktree_off_runtime,
    default_pr_command_control, discover_open_prs_with_control, load_state,
    manual_branch_artifact_ownership_off_runtime, manual_branch_artifacts_match_off_runtime,
    manual_branch_source_owns_artifacts, pr_label, pr_tracking_ref, prepare_manual_branch_worktree,
    prepare_pr_worktree, resolve_branch_head, save_state,
};
#[cfg(test)]
use tracedecay_application::pr_tracking::{managed_summary, try_acquire_manual_branch_lifecycle};
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::TraceDecayError;

use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_runtime_core::logging::log_daemon_event;

const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";

fn scheduler_unavailable(detail: &str) -> String {
    format!("{CODE_INDEX_SCHEDULER_UNAVAILABLE}: {detail}")
}

async fn git_authority_available(repo_root: &Path) -> bool {
    let repo = repo_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tracedecay_runtime_core::worktree::git_worktree_root(&repo).is_some()
    })
    .await
    .ok()
    .unwrap_or(false)
}

#[cfg(test)]
use super::branch_admin::StoreAdministration;

mod runtime;
pub(crate) use runtime::PrAutotrackTask;
pub(super) use runtime::spawn_with_administration;

#[derive(Clone, Copy)]
struct PrStoreAdministration<'a> {
    schedulers: Option<&'a CodeIndexSchedulerRegistryV1>,
    graph: Option<&'a Arc<crate::tracedecay::TraceDecay>>,
    command_control: &'a PrCommandControl,
}

impl<'a> PrStoreAdministration<'a> {
    fn with_control(
        schedulers: &'a CodeIndexSchedulerRegistryV1,
        graph: &'a Arc<crate::tracedecay::TraceDecay>,
        command_control: &'a PrCommandControl,
    ) -> Self {
        Self {
            schedulers: Some(schedulers),
            graph: Some(graph),
            command_control,
        }
    }

    #[cfg(test)]
    fn state_only(_daemon: &StoreAdministration) -> Self {
        Self {
            schedulers: None,
            graph: None,
            command_control: default_pr_command_control(),
        }
    }
}

/// Maximum number of *new* PR branches tracked per poll cycle, so a repo with
/// 100 open PRs ramps up gradually instead of forking 100 syncs at once.
const MAX_NEW_TRACKS_PER_CYCLE: usize = 10;

// ---------------------------------------------------------------------------
// Lifecycle reconciliation
// ---------------------------------------------------------------------------

/// Logs a `pr_autotrack` "skipped" daemon event with the optional branch label
/// and PR number. Every skip path (persistence failure, track failure, fork,
/// reconciled-state persistence failure) funnels through here so the field set
/// and ordering stay identical across them.
fn log_pr_skip(repo_root: &Path, branch_label: Option<&str>, pr: Option<u64>, reason: &str) {
    let mut fields = vec![
        ("project", repo_root.display().to_string()),
        ("action", "skipped".to_string()),
    ];
    if let Some(branch) = branch_label {
        fields.push(("branch", branch.to_string()));
    }
    if let Some(pr) = pr {
        fields.push(("pr", pr.to_string()));
    }
    fields.push(("reason", reason.to_string()));
    log_daemon_event("pr_autotrack", &fields);
}

/// Activates an operator-requested branch head through the same worktree
/// prep + scheduler mount path as [`track_pr`].
#[cfg(test)]
pub(crate) async fn activate_manual_branch_head(
    repo_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    branch: &str,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    if schedulers.is_none() {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    }
    let lifecycle = try_acquire_manual_branch_lifecycle(&graph.store_layout().data_root, branch)?;
    activate_manual_branch_head_with_lifecycle(
        repo_root,
        graph,
        schedulers,
        branch,
        &lifecycle,
        default_pr_command_control(),
    )
    .await
}

#[hotpath::measure(label = "daemon.pr_autotrack.activate", future = true)]
pub(crate) async fn activate_manual_branch_head_with_lifecycle(
    repo_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    branch: &str,
    lifecycle: &ManualBranchLifecycleLeaseV1,
    command_control: &PrCommandControl,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match requested branch",
        ));
    }
    let administration = match schedulers {
        Some(schedulers) => PrStoreAdministration::with_control(schedulers, graph, command_control),
        None => PrStoreAdministration {
            schedulers: None,
            graph: Some(graph),
            command_control,
        },
    };
    activate_manual_branch_with_administration(
        repo_root,
        &graph.store_layout().data_root,
        branch,
        administration,
        lifecycle,
    )
    .await
}

#[hotpath::measure(label = "daemon.pr_autotrack.activate_manual_branch", future = true)]
async fn activate_manual_branch_with_administration(
    repo_root: &Path,
    data_root: &Path,
    branch: &str,
    administration: PrStoreAdministration<'_>,
    lifecycle: &ManualBranchLifecycleLeaseV1,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    let Some(schedulers) = administration.schedulers else {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    };
    let Some(graph) = administration.graph else {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    };
    if !git_authority_available(repo_root).await {
        return Err(ManualBranchActivationError::git_unavailable(
            "git authority is unavailable for branch activation",
        ));
    }

    if branch.starts_with('-') || branch.is_empty() {
        return Err(ManualBranchActivationError::invalid_ref(format!(
            "branch '{branch}' is not a valid branch name"
        )));
    }

    let command_control = administration.command_control.clone();
    let repo = repo_root.to_path_buf();
    let branch_name = branch.to_string();
    let head_sha = match tokio::task::spawn_blocking(move || {
        resolve_branch_head(&repo, &branch_name, &command_control)
    })
    .await
    {
        Ok(Ok(sha)) => sha,
        Ok(Err(error)) => return Err(error),
        Err(error) => {
            return Err(ManualBranchActivationError::activation_failed(format!(
                "branch resolution join error: {error}"
            )));
        }
    };

    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease changed before activation",
        ));
    }
    let artifacts = ManualBranchArtifactsV1::for_head(data_root, branch, &head_sha);
    let worktree = artifacts.worktree.clone();
    if worktree.try_exists().map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot inspect manual worktree '{}': {error}",
            worktree.display()
        ))
    })? && manual_branch_artifacts_match_off_runtime(
        repo_root,
        &artifacts,
        &head_sha,
        administration.command_control.clone(),
    )
    .await?
    {
        if !schedulers.is_worktree_mounted(&worktree).await {
            activate_linked_worktree(schedulers, graph, &worktree)
                .await
                .map_err(ManualBranchActivationError::activation_failed)?;
        }
        return Ok(ManualBranchActivation {
            branch: branch.to_string(),
            head_sha,
            worktree,
            outcome: tracedecay_runtime_core::branch::BranchAddOutcome::AlreadyTracked,
        });
    }

    let tracking_ref = artifacts.tracking_ref.clone();
    let label = artifacts.label.clone();
    if worktree.try_exists().map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot inspect manual worktree '{}': {error}",
            worktree.display()
        ))
    })? {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "existing manual worktree '{}' does not match requested branch generation",
            worktree.display()
        )));
    }
    let repo = repo_root.to_path_buf();
    let wt = worktree.clone();
    let tref = tracking_ref.clone();
    let label_for_prep = label.clone();
    let expected_head = head_sha.clone();
    let command_control = administration.command_control.clone();
    match tokio::task::spawn_blocking(move || {
        prepare_manual_branch_worktree(
            &repo,
            &wt,
            &tref,
            &label_for_prep,
            &expected_head,
            &command_control,
        )
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            return cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(reason),
            )
            .await;
        }
        Err(error) => {
            return cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(format!(
                    "worktree preparation join error: {error}"
                )),
            )
            .await;
        }
    }

    match activate_linked_worktree(schedulers, graph, &worktree).await {
        Ok(()) => Ok(ManualBranchActivation {
            branch: branch.to_string(),
            head_sha,
            worktree,
            outcome: tracedecay_runtime_core::branch::BranchAddOutcome::Added,
        }),
        Err(reason) => {
            cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(reason),
            )
            .await
        }
    }
}

async fn cleanup_failed_manual_track(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    head_sha: &str,
    administration: PrStoreAdministration<'_>,
    original: ManualBranchActivationError,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    match retire_worktree_mount(administration.schedulers, worktree).await {
        Ok(()) => {
            let cleanup_control = PrCommandControl::default();
            if !cleanup_owned_worktree_off_runtime(
                repo_root,
                worktree,
                tracking_ref,
                label,
                head_sha,
                cleanup_control,
            )
            .await?
            {
                return Err(ManualBranchActivationError::activation_failed(format!(
                    "{original}; incomplete branch worktree ownership changed before cleanup"
                )));
            }
            Err(original)
        }
        Err(cleanup_reason) => Err(ManualBranchActivationError::activation_failed(format!(
            "{original}; failed to remove incomplete branch store: {cleanup_reason}"
        ))),
    }
}

/// Retires the exact artifacts created by a newly activated manual branch when
/// its subsequent metadata sealing fails. Callers retain the same lifecycle
/// lease that covered activation, so no concurrent add or removal can replace
/// the worktree between the ownership proof and cleanup.
#[hotpath::measure(label = "daemon.pr_autotrack.cleanup_manual_activation", future = true)]
pub(crate) async fn cleanup_manual_branch_activation(
    repo_root: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    activation: &ManualBranchActivation,
    lifecycle: &ManualBranchLifecycleLeaseV1,
) -> std::result::Result<(), ManualBranchActivationError> {
    if !lifecycle.matches_branch(&activation.branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match failed activation",
        ));
    }
    let artifacts =
        ManualBranchArtifactsV1::for_head(data_root, &activation.branch, &activation.head_sha);
    if artifacts.worktree != activation.worktree {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "failed activation worktree '{}' does not match exact branch identity",
            activation.worktree.display()
        )));
    }
    retire_worktree_mount(Some(schedulers), &artifacts.worktree)
        .await
        .map_err(ManualBranchActivationError::activation_failed)?;
    if !cleanup_owned_worktree_off_runtime(
        repo_root,
        &artifacts.worktree,
        &artifacts.tracking_ref,
        &artifacts.label,
        &activation.head_sha,
        default_pr_command_control().clone(),
    )
    .await?
    {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "failed activation for branch '{}' changed before exact cleanup",
            activation.branch
        )));
    }
    Ok(())
}

/// Retires only the manual artifacts proven by a persisted graph-source entry
/// before branch metadata removal commits. The source's exact worktree,
/// synthetic ref, and OID are the ownership proof; a legacy entry without
/// that proof is intentionally left untouched rather than guessing at Git
/// artifacts. The lifecycle lease returns only after synchronous Git teardown
/// finishes, so request cancellation cannot admit a concurrent replacement
/// while the blocking worker still owns those artifacts.
#[hotpath::measure(label = "daemon.pr_autotrack.cleanup_manual_retirement", future = true)]
pub(crate) async fn cleanup_manual_branch_retirement(
    repo_root: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch: &str,
    source: &tracedecay_runtime_core::branch_meta::BranchGraphSourceV1,
    lifecycle: ManualBranchLifecycleLeaseV1,
) -> std::result::Result<ManualBranchLifecycleLeaseV1, ManualBranchActivationError> {
    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match metadata retirement",
        ));
    }
    if !manual_branch_source_owns_artifacts(data_root, branch, source) {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "stored branch provenance does not own manual artifacts for '{branch}'"
        )));
    }
    let artifacts = ManualBranchArtifactsV1::for_head(data_root, branch, &source.source_oid);
    let expected_worktree = artifacts
        .worktree
        .canonicalize()
        .unwrap_or(artifacts.worktree.clone());
    let ownership = manual_branch_artifact_ownership_off_runtime(
        repo_root,
        &expected_worktree,
        &artifacts.tracking_ref,
        &artifacts.label,
        &source.source_oid,
        default_pr_command_control().clone(),
    )
    .await?;
    if ownership == ManualBranchArtifactOwnershipV1::Foreign {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "manual artifacts for branch '{branch}' are no longer owned by the stored source"
        )));
    }
    retire_worktree_mount(Some(schedulers), &expected_worktree)
        .await
        .map_err(ManualBranchActivationError::activation_failed)?;
    let repo_root = repo_root.to_path_buf();
    let tracking_ref = artifacts.tracking_ref;
    let label = artifacts.label;
    let source_oid = source.source_oid.clone();
    let (cleaned, lifecycle) = tokio::task::spawn_blocking(move || {
        let cleaned = cleanup_owned_worktree(
            &repo_root,
            &expected_worktree,
            &tracking_ref,
            &label,
            &source_oid,
            default_pr_command_control(),
        );
        (cleaned, lifecycle)
    })
    .await
    .map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch retirement cleanup task did not complete: {error}"
        ))
    })?;
    if !cleaned? {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "manual artifacts for branch '{branch}' changed before exact retirement"
        )));
    }
    Ok(lifecycle)
}

pub(crate) async fn retire_worktree_mount(
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    worktree: &Path,
) -> std::result::Result<(), String> {
    let Some(schedulers) = schedulers else {
        return Err(scheduler_unavailable(
            "code-index scheduler authority is unavailable for worktree retirement",
        ));
    };
    let root = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    let roots = BTreeSet::from([root]);
    if !schedulers.retire_project_roots(&roots).await {
        return Err(scheduler_unavailable(
            "code-index scheduler did not finish worktree retirement",
        ));
    }
    Ok(())
}

#[hotpath::measure(label = "daemon.pr_autotrack.reconcile", future = true)]
async fn reconcile_project_with_administration(
    repo_root: &Path,
    data_root: &Path,
    discovery: &PrDiscovery,
    cap: usize,
    administration: PrStoreAdministration<'_>,
) -> std::result::Result<ReconcileReport, TraceDecayError> {
    let mut state = load_state(data_root)?;
    let mut report = ReconcileReport {
        skipped_forks: discovery.skipped_forks.clone(),
        ..Default::default()
    };
    let mut state_dirty = false;

    // Desired label → discovered PR.
    let desired: BTreeMap<String, &DiscoveredPr> = discovery
        .open
        .iter()
        .map(|pr| (pr_label(pr.number), pr))
        .collect();

    // Removals first (cheap, unblocks disk) — managed entries no longer open.
    // Suppress them entirely when the discovery is `partial`: an incomplete
    // listing must never be read as "these PRs closed", or a truncated `gh`
    // page (or gh↔ls-remote flapping) would churn-untrack still-open PRs.
    if discovery.partial {
        report.removals_suppressed = true;
        log_daemon_event(
            "pr_autotrack",
            &[
                ("project", repo_root.display().to_string()),
                ("action", "poll".to_string()),
                ("outcome", "partial".to_string()),
                (
                    "reason",
                    "removals suppressed: discovery incomplete".to_string(),
                ),
            ],
        );
    } else {
        // Sweep leaked checkouts before removals: a `pr-worktrees/pr-<N>` dir
        // whose PR is neither open nor managed is an orphan left by a daemon
        // crash between `worktree add` and `save_state`. Remove it so stale
        // worktrees don't accumulate on disk across restarts.
        sweep_orphan_pr_worktrees(repo_root, data_root, &desired, &state, administration).await;

        let stale: Vec<String> = state
            .managed
            .keys()
            .filter(|label| !desired.contains_key(*label))
            .cloned()
            .collect();
        for label in stale {
            let Some(managed) = state.managed.get(&label).cloned() else {
                continue;
            };
            match untrack_pr(repo_root, data_root, &label, &managed, administration).await {
                Ok(()) => {
                    state.managed.remove(&label);
                    state_dirty = true;
                    report.untracked.push(label.clone());
                    log_daemon_event(
                        "pr_autotrack",
                        &[
                            ("project", repo_root.display().to_string()),
                            ("action", "untracked".to_string()),
                            ("branch", label),
                            ("pr", managed.pr.to_string()),
                        ],
                    );
                }
                Err(reason) => {
                    report.failures.push((label.clone(), reason.clone()));
                    log_pr_skip(repo_root, Some(&label), Some(managed.pr), &reason);
                }
            }
        }
    }

    // Additions, capped per cycle.
    let mut added = 0usize;
    for (label, pr) in &desired {
        let current = state.managed.get(label).cloned();
        if current.as_ref().is_some_and(|managed| {
            managed.head_sha == pr.head_sha && managed.head_branch == pr.head_branch
        }) {
            continue;
        }
        let is_new = current.is_none();
        if is_new && added >= cap {
            // The cap bounds only *new* tracks. `continue` (not `break`) so a
            // later entry that is already managed but has a changed head_sha
            // still gets its refresh — otherwise a burst of new PRs would starve
            // head updates for existing managed PRs, serving stale graphs.
            report.capped = true;
            continue;
        }
        if let Some(managed) = current {
            // A changed remote head invalidates the entire branch graph. Drop
            // the owned store before rebuilding so stale data is never served.
            // If removal is busy or fails, leave the old state and owned Git
            // artifacts intact; tracking the new head would otherwise mix the
            // two generations under one label.
            match untrack_pr(repo_root, data_root, label, &managed, administration).await {
                Ok(()) => {
                    state.managed.remove(label);
                    state_dirty = true;
                }
                Err(reason) => {
                    report.failures.push((label.clone(), reason.clone()));
                    log_pr_skip(repo_root, Some(label), Some(managed.pr), &reason);
                    continue;
                }
            }
        }
        match track_pr(repo_root, data_root, pr, administration).await {
            Ok(managed) => {
                let dirty_before_insert = state_dirty;
                state.managed.insert(label.clone(), managed.clone());
                match save_state(data_root, &state) {
                    Ok(()) => {
                        state_dirty = false;
                        report.tracked.push(label.clone());
                        if is_new {
                            added += 1;
                        }
                        log_daemon_event(
                            "pr_autotrack",
                            &[
                                ("project", repo_root.display().to_string()),
                                ("action", "tracked".to_string()),
                                ("branch", label.clone()),
                                ("pr", pr.number.to_string()),
                                ("head", pr.head_branch.clone()),
                            ],
                        );
                    }
                    Err(error) => {
                        let persist_reason = format!("failed to persist managed state: {error}");
                        match untrack_pr(repo_root, data_root, label, &managed, administration)
                            .await
                        {
                            Ok(()) => {
                                state.managed.remove(label);
                                state_dirty = dirty_before_insert;
                                report
                                    .failures
                                    .push((label.clone(), persist_reason.clone()));
                                log_pr_skip(
                                    repo_root,
                                    Some(label),
                                    Some(pr.number),
                                    &persist_reason,
                                );
                            }
                            Err(cleanup_reason) => {
                                // The successfully-added branch remains owned and
                                // recoverable. Do not drop it from in-memory state
                                // before the coordinator has actually removed its
                                // store, and expose both failures to the caller.
                                state_dirty = dirty_before_insert;
                                let reason = format!(
                                    "{persist_reason}; rollback cleanup failed: {cleanup_reason}"
                                );
                                report.failures.push((label.clone(), reason.clone()));
                                log_pr_skip(repo_root, Some(label), Some(pr.number), &reason);
                            }
                        }
                    }
                }
            }
            Err(reason) => {
                report.failures.push((label.clone(), reason.clone()));
                log_pr_skip(repo_root, Some(label), Some(pr.number), &reason);
            }
        }
    }

    for pr in &discovery.skipped_forks {
        log_pr_skip(repo_root, None, Some(*pr), "fork");
    }

    if state_dirty && let Err(error) = save_state(data_root, &state) {
        let reason = format!("failed to persist reconciled state: {error}");
        report
            .failures
            .push(("<state>".to_string(), reason.clone()));
        log_pr_skip(repo_root, None, None, &reason);
    }
    Ok(report)
}

/// Fetches a PR head, checks it out into a linked worktree, and mounts that
/// worktree on the injected code-index scheduler. Refuses before Git mutation
/// when the scheduler, retained graph, or Git worktree authority is missing.
#[hotpath::measure(label = "daemon.pr_autotrack.track", future = true)]
async fn track_pr(
    repo_root: &Path,
    data_root: &Path,
    pr: &DiscoveredPr,
    administration: PrStoreAdministration<'_>,
) -> std::result::Result<ManagedPr, String> {
    let Some(schedulers) = administration.schedulers else {
        return Err(scheduler_unavailable(
            "code-index scheduler authority is unavailable for PR worktree activation",
        ));
    };
    let Some(graph) = administration.graph else {
        return Err(scheduler_unavailable(
            "code-index scheduler authority is unavailable for PR worktree activation",
        ));
    };
    if !git_authority_available(repo_root).await {
        return Err("git authority is unavailable for PR worktree activation".to_string());
    }

    let label = pr_label(pr.number);
    let tracking_ref = pr_tracking_ref(pr.number);
    let worktree = data_root
        .join("pr-worktrees")
        .join(format!("pr-{}", pr.number));
    let repo = repo_root.to_path_buf();
    let wt = worktree.clone();
    let tref = tracking_ref.clone();
    let label_for_prep = label.clone();
    let expected_head = pr.head_sha.clone();
    let command_control = administration.command_control.clone();
    let pr_number = pr.number;
    match tokio::task::spawn_blocking(move || {
        prepare_pr_worktree(
            &repo,
            &wt,
            pr_number,
            &tref,
            &label_for_prep,
            &expected_head,
            &command_control,
        )
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            return cleanup_failed_track(
                repo_root,
                data_root,
                pr.number,
                &pr.head_sha,
                &label,
                administration,
                &reason,
            )
            .await;
        }
        Err(error) => {
            let reason = format!("worktree preparation join error: {error}");
            return cleanup_failed_track(
                repo_root,
                data_root,
                pr.number,
                &pr.head_sha,
                &label,
                administration,
                &reason,
            )
            .await;
        }
    }

    match activate_linked_worktree(schedulers, graph, &worktree).await {
        Ok(()) => Ok(ManagedPr {
            pr: pr.number,
            head_branch: pr.head_branch.clone(),
            head_sha: pr.head_sha.clone(),
            worktree,
            tracking_ref,
        }),
        Err(reason) => {
            cleanup_failed_track(
                repo_root,
                data_root,
                pr.number,
                &pr.head_sha,
                &label,
                administration,
                &reason,
            )
            .await
        }
    }
}

#[hotpath::measure(label = "daemon.pr_autotrack.activate_worktree", future = true)]
async fn activate_linked_worktree(
    schedulers: &CodeIndexSchedulerRegistryV1,
    graph: &crate::tracedecay::TraceDecay,
    worktree: &Path,
) -> std::result::Result<(), String> {
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| {
            scheduler_unavailable("project identity is unavailable for worktree activation")
        })?;
    let project_id = ProjectId::new(project_id.to_owned()).map_err(|error| {
        scheduler_unavailable(&format!(
            "invalid project identity for worktree activation: {error}"
        ))
    })?;
    let store_root = graph.store_layout().data_root.join("code-index-v1");
    let graph_runtime = graph.retained_store_runtime_registry();
    let semantic_lifecycle = graph_runtime
        .project_semantic_lifecycle(&project_id)
        .await
        .map_err(|error| {
            scheduler_unavailable(&format!(
                "project semantic lifecycle is unavailable for worktree activation: {error}"
            ))
        })?;
    let project_database = Arc::new(graph.db().clone());
    schedulers
        .mount_worktree_with_graph_runtime(
            project_id,
            worktree,
            store_root,
            None,
            graph_runtime.code_graph_seat_port(),
            project_database,
            tracedecay_code_index_runtime::code_index_scheduler::CodeGraphActivationPolicyV1::from_enabled(
                graph.get_config().native_graph_activation,
            ),
            Some(semantic_lifecycle),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            scheduler_unavailable(&format!(
                "code-index scheduler rejected worktree activation: {error}"
            ))
        })
}

/// Retires the scheduler mount for a managed PR worktree. Git artifacts stay
/// intact until [`untrack_pr`] or sweep cleanup runs after this returns Ok.
#[hotpath::measure(label = "daemon.pr_autotrack.remove_store", future = true)]
async fn remove_pr_store(
    _repo_root: &Path,
    data_root: &Path,
    label: &str,
    administration: PrStoreAdministration<'_>,
) -> std::result::Result<(), String> {
    let Some(schedulers) = administration.schedulers else {
        return Err(scheduler_unavailable(
            "code-index scheduler authority is unavailable for PR worktree retirement",
        ));
    };
    let Some(number) = pr_number_from_label(label) else {
        return Err("managed PR label does not name a PR worktree".to_string());
    };
    let worktree = data_root.join("pr-worktrees").join(format!("pr-{number}"));
    let root = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
    let roots = BTreeSet::from([root]);
    if !schedulers.retire_project_roots(&roots).await {
        return Err(scheduler_unavailable(
            "code-index scheduler did not finish PR worktree retirement",
        ));
    }
    Ok(())
}

fn pr_number_from_label(label: &str) -> Option<u64> {
    label
        .strip_prefix("tracedecay/autotrack/pr/")
        .or_else(|| label.strip_prefix("pr/"))
        .and_then(|number| number.parse().ok())
}

/// Rolls back a failed branch add without deleting owned Git artifacts until
/// the scheduler proves the corresponding worktree mount is gone.
async fn cleanup_failed_track(
    repo_root: &Path,
    data_root: &Path,
    pr: u64,
    head_sha: &str,
    label: &str,
    administration: PrStoreAdministration<'_>,
    original_reason: &str,
) -> std::result::Result<ManagedPr, String> {
    match remove_pr_store(repo_root, data_root, label, administration).await {
        Ok(()) => {
            cleanup_pr_worktree_off_runtime(
                repo_root,
                data_root,
                pr,
                head_sha,
                true,
                administration.command_control.clone(),
            )
            .await
            .map_err(|error| format!("{original_reason}; cleanup failed: {error}"))?;
            Err(original_reason.to_string())
        }
        Err(cleanup_reason) => Err(format!(
            "{original_reason}; failed to remove incomplete branch store: {cleanup_reason}"
        )),
    }
}

/// Fetches `refs/pull/<N>/head` into `tracking_ref` and adds a linked worktree
/// checked out on a local branch named `label` at that ref.
/// Untracks a managed PR: removes its branch store, its worktree, its local
/// tracking branch, and its ref. The Git artifacts are released only after the
/// coordinator reports that the store is gone (or was already absent).
#[hotpath::measure(label = "daemon.pr_autotrack.untrack", future = true)]
async fn untrack_pr(
    repo_root: &Path,
    data_root: &Path,
    label: &str,
    managed: &ManagedPr,
    administration: PrStoreAdministration<'_>,
) -> std::result::Result<(), String> {
    let expected_label = pr_label(managed.pr);
    let legacy_label = format!("pr/{}", managed.pr);
    let is_legacy = label == legacy_label;
    let expected_worktree = data_root
        .join("pr-worktrees")
        .join(format!("pr-{}", managed.pr));
    let expected_ref = pr_tracking_ref(managed.pr);
    if (label != expected_label && !is_legacy)
        || managed.worktree != expected_worktree
        || managed.tracking_ref != expected_ref
    {
        return Err("managed PR entry does not own the requested branch artifacts".to_string());
    }
    remove_pr_store(repo_root, data_root, label, administration).await?;
    cleanup_pr_worktree_off_runtime(
        repo_root,
        data_root,
        managed.pr,
        &managed.head_sha,
        !is_legacy,
        administration.command_control.clone(),
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Removes leaked PR worktrees from interrupted prior cycles.
///
/// Scans `pr-worktrees/` for `pr-<N>` checkouts whose PR is neither open
/// (`desired`) nor currently managed. Such a dir is an orphan left when the
/// daemon died between `git worktree add` and `save_state`: no state entry
/// claims it and the PR is not open, so it would otherwise sit on disk forever.
/// Its synthetic branch and fetch ref are cleaned up alongside the checkout.
/// Only called for a *complete* discovery (never when `partial`), so an open PR
/// that merely fell outside a truncated listing is never swept.
#[hotpath::measure(label = "daemon.pr_autotrack.sweep", future = true)]
async fn sweep_orphan_pr_worktrees(
    repo_root: &Path,
    data_root: &Path,
    desired: &BTreeMap<String, &DiscoveredPr>,
    state: &PrAutotrackState,
    administration: PrStoreAdministration<'_>,
) {
    let worktrees_dir = data_root.join("pr-worktrees");
    let entries = match tokio::task::spawn_blocking({
        let worktrees_dir = worktrees_dir.clone();
        move || {
            std::fs::read_dir(&worktrees_dir).map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>()
            })
        }
    })
    .await
    {
        Ok(Ok(entries)) => entries,
        Ok(Err(_)) | Err(_) => return,
    };
    let managed_prs: std::collections::BTreeSet<u64> =
        state.managed.values().map(|m| m.pr).collect();
    for name in entries {
        let Some(number) = name
            .to_str()
            .and_then(|n| n.strip_prefix("pr-"))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if managed_prs.contains(&number) || desired.contains_key(&pr_label(number)) {
            continue;
        }
        let label = pr_label(number);
        match remove_pr_store(repo_root, data_root, &label, administration).await {
            Ok(()) => {
                match cleanup_pr_worktree_off_runtime(
                    repo_root,
                    data_root,
                    number,
                    "",
                    true,
                    administration.command_control.clone(),
                )
                .await
                {
                    Ok(_) => log_daemon_event(
                        "pr_autotrack",
                        &[
                            ("project", repo_root.display().to_string()),
                            ("action", "swept".to_string()),
                            ("pr", number.to_string()),
                            ("reason", "orphan worktree".to_string()),
                        ],
                    ),
                    Err(error) => {
                        log_pr_skip(repo_root, Some(&label), Some(number), &error.to_string());
                    }
                }
            }
            Err(reason) => log_pr_skip(repo_root, Some(&label), Some(number), &reason),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
