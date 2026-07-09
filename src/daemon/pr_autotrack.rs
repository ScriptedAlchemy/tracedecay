//! Daemon PR-branch auto-tracking (opt-in via `sync.auto_track_pr_branches`).
//!
//! # What this does
//!
//! When a project enables `sync.auto_track_pr_branches`, a daemon poll loop
//! discovers the open pull requests on the repo's `origin` remote and tracks
//! each PR head branch through the *existing* branch-tracking machinery so
//! `branch_diff` / `branch_search` / `branch_list` and graph queries work against
//! every open PR without anyone running `tracedecay branch add`. When a PR closes
//! or merges, its branch is untracked and its per-branch store is cleaned up.
//!
//! # Why worktrees
//!
//! [`crate::tracedecay::indexing`] syncs a branch DB by scanning the *working
//! tree* at the passed project root — it does not read blobs out of a git ref.
//! So to index a PR head accurately the head must be checked out somewhere. We
//! therefore fetch each PR head into a deterministic local ref
//! (`refs/tracedecay/pr/<N>`), check it out into a linked worktree on a local
//! branch named `pr/<N>` under the store's `pr-worktrees/` dir (a *named* branch,
//! not detached HEAD — the branch-drift guard in sync refuses a detached
//! worktree), and track that worktree exactly the way the
//! git-metadata watcher tracks any other linked worktree
//! ([`crate::tracedecay::TraceDecay::add_branch_tracking_with_options`]). A branch
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
//! repositories; that is deliberately out of scope for the first cut.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::config::SyncConfig;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use super::log_daemon_event;

/// Filename of the PR-autotrack state sidecar, stored next to `branch-meta.json`
/// in the project's store data root.
const STATE_FILENAME: &str = "pr-autotrack.json";
/// Maximum number of *new* PR branches tracked per poll cycle, so a repo with
/// 100 open PRs ramps up gradually instead of forking 100 syncs at once.
const MAX_NEW_TRACKS_PER_CYCLE: usize = 10;
/// Base cadence of the poll loop; per-project intervals are honored on top of
/// this floor via a last-run map.
const BASE_TICK: Duration = Duration::from_mins(1);

/// A PR head discovered on the origin remote that we can track (same-repo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPr {
    /// PR number.
    pub number: u64,
    /// The PR's head branch name (display only).
    pub head_branch: String,
    /// The exact remote head commit observed during discovery.
    pub head_sha: String,
}

/// The result of one discovery pass over a repo's `origin` remote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrDiscovery {
    /// Open, same-repo PR heads that can be tracked.
    pub open: Vec<DiscoveredPr>,
    /// PR numbers skipped because their head lives on a fork.
    pub skipped_forks: Vec<u64>,
}

/// A currently-managed PR branch, persisted in the state sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedPr {
    /// PR number.
    pub pr: u64,
    /// The PR's head branch name (display only).
    pub head_branch: String,
    /// Last remote head commit successfully indexed.
    #[serde(default)]
    pub head_sha: String,
    /// Path to the linked worktree on the owned synthetic branch.
    pub worktree: PathBuf,
    /// The deterministic local ref the PR head was fetched into.
    pub tracking_ref: String,
}

/// PR-autotrack persistent state: internal branch label → managed entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrAutotrackState {
    /// Managed PR branches keyed by their internal synthetic branch label.
    #[serde(default)]
    pub managed: BTreeMap<String, ManagedPr>,
}

/// The collision-proof internal tracking label for a PR.
fn pr_label(number: u64) -> String {
    format!("tracedecay/autotrack/pr/{number}")
}

/// The deterministic local ref a PR head is fetched into.
fn pr_tracking_ref(number: u64) -> String {
    format!("refs/tracedecay/pr/{number}")
}

fn state_path(data_root: &Path) -> PathBuf {
    data_root.join(STATE_FILENAME)
}

/// Loads the PR-autotrack state, returning an empty state when absent/corrupt.
pub fn load_state(data_root: &Path) -> PrAutotrackState {
    let path = state_path(data_root);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return PrAutotrackState::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

fn save_state(data_root: &Path, state: &PrAutotrackState) -> std::io::Result<()> {
    let path = state_path(data_root);
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    crate::storage::PrivateStoreIo::write_file_atomically(&path, &temp, json.as_bytes())
}

/// A summary of managed PR branches for status surfaces (dashboard / CLI).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManagedPrSummary {
    /// Internal synthetic branch label.
    pub branch: String,
    /// PR number.
    pub pr: u64,
    /// PR head branch name.
    pub head_branch: String,
}

/// Returns the managed PR branches (sorted by PR number) for a project's store.
pub fn managed_summary(data_root: &Path) -> Vec<ManagedPrSummary> {
    let state = load_state(data_root);
    let mut out: Vec<ManagedPrSummary> = state
        .managed
        .into_iter()
        .map(|(branch, m)| ManagedPrSummary {
            branch,
            pr: m.pr,
            head_branch: m.head_branch,
        })
        .collect();
    out.sort_by_key(|s| s.pr);
    out
}

// ---------------------------------------------------------------------------
// Discovery (pure parsers + one impure orchestrator)
// ---------------------------------------------------------------------------

/// One entry from `gh pr list --json number,headRefName,headRefOid,state,isCrossRepository`.
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

/// Parses `gh pr list` JSON into a discovery result. Open same-repo PRs go to
/// `open`; open cross-repository PRs are recorded as skipped forks; non-open PRs
/// are ignored.
fn parse_gh_pr_list(json: &str) -> serde_json::Result<PrDiscovery> {
    let prs: Vec<GhPr> = serde_json::from_str(json)?;
    let mut discovery = PrDiscovery::default();
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

/// Parses `git ls-remote --heads origin` into a `sha → branch` map.
fn parse_ls_remote_heads(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let Some((sha, refname)) = split_ls_remote_line(line) else {
            continue;
        };
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            map.insert(sha.to_string(), branch.to_string());
        }
    }
    map
}

/// Parses `git ls-remote origin 'refs/pull/*/head'` into `(pr_number, sha)`
/// pairs, ignoring `refs/pull/*/merge` and malformed lines.
fn parse_ls_remote_pull_heads(output: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let Some((sha, refname)) = split_ls_remote_line(line) else {
            continue;
        };
        let Some(rest) = refname.strip_prefix("refs/pull/") else {
            continue;
        };
        let Some(num_str) = rest.strip_suffix("/head") else {
            continue;
        };
        if let Ok(number) = num_str.parse::<u64>() {
            out.push((number, sha.to_string()));
        }
    }
    out
}

fn split_ls_remote_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let sha = parts.next()?;
    let refname = parts.next()?;
    if sha.is_empty() || refname.is_empty() {
        return None;
    }
    Some((sha, refname))
}

/// Maps PR head SHAs to branch names via the origin's `refs/heads/*` SHA index.
/// A PR whose head SHA matches a head ref is a same-repo PR (tracked under
/// `head_branch`); one that matches nothing is treated as a fork and skipped.
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
    discovery.open.sort_by_key(|d| d.number);
    discovery.skipped_forks.sort_unstable();
    discovery
}

fn run_git(repo_root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new(crate::git::git_program())
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
}

fn origin_is_github(repo_root: &Path) -> bool {
    run_git(repo_root, &["remote", "get-url", "origin"])
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|url| url.contains("github.com"))
}

fn gh_available() -> bool {
    std::process::Command::new("gh")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Discovers open PR head branches on the repo's `origin` remote.
///
/// Prefers `gh pr list` when `gh` is on PATH and `origin` is GitHub; otherwise
/// falls back to `git ls-remote` and SHA-matching. Same-repo PRs are returned in
/// `open`; fork PRs are recorded in `skipped_forks`.
pub fn discover_open_prs(repo_root: &Path) -> PrDiscovery {
    if origin_is_github(repo_root) && gh_available() {
        if let Some(discovery) = discover_via_gh(repo_root) {
            return discovery;
        }
    }
    discover_via_ls_remote(repo_root)
}

fn discover_via_gh(repo_root: &Path) -> Option<PrDiscovery> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,headRefName,headRefOid,state,isCrossRepository",
        ])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let json = String::from_utf8(output.stdout).ok()?;
    parse_gh_pr_list(&json).ok()
}

fn discover_via_ls_remote(repo_root: &Path) -> PrDiscovery {
    let pull_heads = run_git(repo_root, &["ls-remote", "origin", "refs/pull/*/head"])
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|out| parse_ls_remote_pull_heads(&out))
        .unwrap_or_default();
    let head_shas = run_git(repo_root, &["ls-remote", "--heads", "origin"])
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|out| parse_ls_remote_heads(&out))
        .unwrap_or_default();
    map_pull_heads_to_branches(&pull_heads, &head_shas)
}

// ---------------------------------------------------------------------------
// Lifecycle reconciliation
// ---------------------------------------------------------------------------

/// A summary of what one reconcile pass changed, for logging and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Internal labels newly tracked or recovered this pass.
    pub tracked: Vec<String>,
    /// Labels untracked this pass (PR closed/merged).
    pub untracked: Vec<String>,
    /// PR numbers skipped as forks.
    pub skipped_forks: Vec<u64>,
    /// True when the per-cycle new-track cap held some additions back.
    pub capped: bool,
    /// Tracking or persistence failures surfaced to callers.
    pub failures: Vec<(String, String)>,
}

/// Reconciles the managed PR set against a discovery result.
///
/// Additions are bounded by `cap` new tracks per call; removals (closed/merged
/// PRs) are always processed. Idempotent: PRs already managed and still open are
/// left untouched. State is persisted before returning.
pub async fn reconcile_project(
    repo_root: &Path,
    data_root: &Path,
    discovery: &PrDiscovery,
    cap: usize,
) -> ReconcileReport {
    let mut state = load_state(data_root);
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
    let stale: Vec<String> = state
        .managed
        .keys()
        .filter(|label| !desired.contains_key(*label))
        .cloned()
        .collect();
    for label in stale {
        if let Some(managed) = state.managed.get(&label).cloned() {
            untrack_pr(repo_root, data_root, &label, &managed).await;
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
            report.capped = true;
            break;
        }
        if let Some(managed) = current {
            // A changed remote head invalidates the entire branch graph. Drop
            // the owned store before rebuilding so stale data is never served.
            untrack_pr(repo_root, data_root, label, &managed).await;
            state.managed.remove(label);
            state_dirty = true;
        }
        match track_pr(repo_root, data_root, pr).await {
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
                        state.managed.remove(label);
                        state_dirty = dirty_before_insert;
                        untrack_pr(repo_root, data_root, label, &managed).await;
                        let reason = format!("failed to persist managed state: {error}");
                        report.failures.push((label.clone(), reason.clone()));
                        log_daemon_event(
                            "pr_autotrack",
                            &[
                                ("project", repo_root.display().to_string()),
                                ("action", "skipped".to_string()),
                                ("branch", label.clone()),
                                ("pr", pr.number.to_string()),
                                ("reason", reason),
                            ],
                        );
                    }
                }
            }
            Err(reason) => {
                report.failures.push((label.clone(), reason.clone()));
                log_daemon_event(
                    "pr_autotrack",
                    &[
                        ("project", repo_root.display().to_string()),
                        ("action", "skipped".to_string()),
                        ("branch", label.clone()),
                        ("pr", pr.number.to_string()),
                        ("reason", reason),
                    ],
                );
            }
        }
    }

    for pr in &discovery.skipped_forks {
        log_daemon_event(
            "pr_autotrack",
            &[
                ("project", repo_root.display().to_string()),
                ("action", "skipped".to_string()),
                ("pr", pr.to_string()),
                ("reason", "fork".to_string()),
            ],
        );
    }

    if state_dirty && let Err(error) = save_state(data_root, &state) {
        let reason = format!("failed to persist reconciled state: {error}");
        report
            .failures
            .push(("<state>".to_string(), reason.clone()));
        log_daemon_event(
            "pr_autotrack",
            &[
                ("project", repo_root.display().to_string()),
                ("action", "skipped".to_string()),
                ("reason", reason),
            ],
        );
    }
    report
}

/// Fetches a PR head, checks it out into a detached linked worktree, and tracks
/// that worktree under the `pr/<N>` label. Returns the managed record.
async fn track_pr(
    repo_root: &Path,
    data_root: &Path,
    pr: &DiscoveredPr,
) -> std::result::Result<ManagedPr, String> {
    let label = pr_label(pr.number);
    let tracking_ref = pr_tracking_ref(pr.number);
    let worktree = data_root
        .join("pr-worktrees")
        .join(format!("pr-{}", pr.number));

    let graph_ready = crate::branch_meta::load_branch_meta(data_root)
        .and_then(|meta| crate::branch::resolve_branch_db_path(data_root, &label, &meta))
        .is_some_and(|path| path.is_file());
    let branch_ref = format!("refs/heads/{label}");
    let branch_ready = ref_points_to(repo_root, &branch_ref, &pr.head_sha);
    let tracking_ref_ready = ref_points_to(repo_root, &tracking_ref, &pr.head_sha);
    let worktree_ready = ref_points_to(&worktree, "HEAD", &pr.head_sha)
        && crate::branch::current_branch(&worktree).as_deref() == Some(label.as_str());
    let validated_orphan =
        branch_ready && tracking_ref_ready && (!worktree.exists() || worktree_ready);
    if graph_ready || validated_orphan {
        let _ = crate::branch::remove_tracked_branch_store(data_root, &label);
        cleanup_pr_worktree(repo_root, data_root, pr.number, &pr.head_sha, true);
    }

    let repo = repo_root.to_path_buf();
    let wt = worktree.clone();
    let tref = tracking_ref.clone();
    let label_for_prep = label.clone();
    let expected_head = pr.head_sha.clone();
    // git operations are blocking; keep them off the reactor.
    let prep = tokio::task::spawn_blocking(move || {
        prepare_pr_worktree(&repo, &wt, &tref, &label_for_prep, &expected_head)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?;
    prep?;

    match TraceDecay::add_branch_tracking_with_options(
        &worktree,
        &label,
        TraceDecayOpenOptions::default(),
    )
    .await
    {
        Ok(crate::branch::BranchAddOutcome::Added) => Ok(ManagedPr {
            pr: pr.number,
            head_branch: pr.head_branch.clone(),
            head_sha: pr.head_sha.clone(),
            worktree,
            tracking_ref,
        }),
        Ok(outcome) => {
            // Deferred may leave branch metadata behind. AlreadyTracked can
            // be an orphan from an interrupted prior cycle. Neither proves a
            // completed sync, so clear only our internal label and retry later.
            let _ = crate::branch::remove_tracked_branch_store(data_root, &label);
            cleanup_pr_worktree(repo_root, data_root, pr.number, &pr.head_sha, true);
            let reason = match outcome {
                crate::branch::BranchAddOutcome::NotIndexed => "project not indexed",
                crate::branch::BranchAddOutcome::AlreadyTracked => {
                    "internal PR branch was already tracked"
                }
                crate::branch::BranchAddOutcome::Deferred => "branch tracking deferred",
                crate::branch::BranchAddOutcome::Added => unreachable!(),
            };
            Err(reason.to_string())
        }
        Err(e) => {
            cleanup_pr_worktree(repo_root, data_root, pr.number, &pr.head_sha, true);
            Err(e.to_string())
        }
    }
}

/// Fetches `refs/pull/<N>/head` into `tracking_ref` and adds a linked worktree
/// checked out on a local branch named `label` (`pr/<N>`) at that ref.
///
/// The worktree must be on a *named* branch matching the tracking label — a
/// detached HEAD trips the branch-drift guard in sync (the DB serves `pr/<N>`
/// but the working tree would report detached HEAD). Idempotent: a stale
/// worktree at `worktree` is removed first, and `-B` resets the branch.
fn prepare_pr_worktree(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    expected_head: &str,
) -> std::result::Result<(), String> {
    let pr_ref_spec = {
        // tracking_ref is refs/tracedecay/pr/<N>; derive the pull ref from it.
        let n = tracking_ref
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        format!("+refs/pull/{n}/head:{tracking_ref}")
    };
    let fetch = run_git(repo_root, &["fetch", "--no-tags", "origin", &pr_ref_spec]);
    if fetch.is_none() {
        return Err("fetch of PR head failed".to_string());
    }
    let fetched_head = run_git(repo_root, &["rev-parse", tracking_ref])
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_string());
    if fetched_head.as_deref() != Some(expected_head) {
        return Err("PR head changed during reconciliation".to_string());
    }
    let branch_ref = format!("refs/heads/{label}");
    if run_git(repo_root, &["show-ref", "--verify", "--quiet", &branch_ref]).is_some() {
        return Err("internal PR worktree branch already exists".to_string());
    }

    if let Some(parent) = worktree.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Clear any stale worktree registration/dir so `worktree add` is idempotent.
    remove_worktree(repo_root, worktree);

    let wt_str = worktree.to_string_lossy();
    let add = run_git(
        repo_root,
        &[
            "worktree",
            "add",
            "-b",
            label,
            "--force",
            &wt_str,
            tracking_ref,
        ],
    );
    if add.is_none() {
        return Err("worktree add failed".to_string());
    }
    Ok(())
}

/// Untracks a managed PR: removes its branch store, its worktree, its local
/// tracking branch, and its ref.
async fn untrack_pr(repo_root: &Path, data_root: &Path, label: &str, managed: &ManagedPr) {
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
        return;
    }
    let data = data_root.to_path_buf();
    let label_owned = label.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        crate::branch::remove_tracked_branch_store(&data, &label_owned)
    })
    .await;
    // `pr/<N>` is the pre-namespace persisted format. Remove its owned store
    // and worktree once, but never delete that ambiguous local branch name.
    cleanup_pr_worktree(
        repo_root,
        data_root,
        managed.pr,
        &managed.head_sha,
        !is_legacy,
    );
}

fn cleanup_pr_worktree(
    repo_root: &Path,
    data_root: &Path,
    pr: u64,
    expected_head: &str,
    remove_synthetic_branch: bool,
) {
    let worktree = data_root.join("pr-worktrees").join(format!("pr-{pr}"));
    let tracking_ref = pr_tracking_ref(pr);
    let owned_head = if expected_head.is_empty() {
        let ref_head = ref_sha(repo_root, &tracking_ref);
        let worktree_head = ref_sha(&worktree, "HEAD");
        match (ref_head, worktree_head) {
            (Some(ref_head), Some(worktree_head)) if ref_head == worktree_head => Some(ref_head),
            _ => None,
        }
    } else {
        Some(expected_head.to_string())
    };
    remove_worktree(repo_root, &worktree);
    let label = pr_label(pr);
    let branch_ref = format!("refs/heads/{label}");
    if let Some(owned_head) = owned_head {
        if remove_synthetic_branch && ref_points_to(repo_root, &branch_ref, &owned_head) {
            let _ = run_git(repo_root, &["branch", "-D", &label]);
        }
        if ref_points_to(repo_root, &tracking_ref, &owned_head) {
            let _ = run_git(repo_root, &["update-ref", "-d", &tracking_ref]);
        }
    }
}

fn ref_points_to(repo_root: &Path, reference: &str, expected_head: &str) -> bool {
    ref_sha(repo_root, reference).is_some_and(|sha| sha == expected_head)
}

fn ref_sha(repo_root: &Path, reference: &str) -> Option<String> {
    run_git(repo_root, &["rev-parse", reference])
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_string())
}

fn remove_worktree(repo_root: &Path, worktree: &Path) {
    let wt_str = worktree.to_string_lossy();
    // `worktree remove` unregisters and deletes the checkout; prune tidies any
    // dangling administrative entry if the dir was removed out from under git.
    let _ = run_git(repo_root, &["worktree", "remove", "--force", &wt_str]);
    let _ = run_git(repo_root, &["worktree", "prune"]);
    if worktree.exists() {
        let _ = std::fs::remove_dir_all(worktree);
    }
}

// ---------------------------------------------------------------------------
// Poll loop (daemon wiring)
// ---------------------------------------------------------------------------

/// Spawns the PR-autotrack poll loop. Cheap and inert when no registered project
/// has the feature enabled — each tick only reads per-project config.
pub fn spawn(global_db_path: Option<PathBuf>) {
    tokio::spawn(async move {
        run(global_db_path).await;
    });
}

async fn run(global_db_path: Option<PathBuf>) {
    let mut last_poll: HashMap<PathBuf, Instant> = HashMap::new();
    loop {
        tick(global_db_path.as_deref(), &mut last_poll).await;
        tokio::time::sleep(BASE_TICK).await;
    }
}

async fn tick(global_db_path: Option<&Path>, last_poll: &mut HashMap<PathBuf, Instant>) {
    let window = 14 * 86_400;
    let cap = 64;
    let db = match global_db_path {
        Some(path) => crate::global_db::GlobalDb::open_at(path).await,
        None => crate::global_db::GlobalDb::open().await,
    };
    let Some(db) = db else {
        return;
    };
    for record in db.code_projects_seen_within(window, cap).await {
        let root = PathBuf::from(&record.canonical_root);
        if !root.is_dir() {
            continue;
        }
        let cfg = crate::config::load_sync_config(&root);
        if !cfg.auto_track_pr_branches {
            continue;
        }
        let interval = Duration::from_secs(cfg.effective_auto_track_pr_poll_secs());
        let due = last_poll.get(&root).is_none_or(|t| t.elapsed() >= interval);
        if !due {
            continue;
        }
        last_poll.insert(root.clone(), Instant::now());
        poll_project(root, cfg).await;
    }
}

/// Runs one discovery + reconcile pass for a project and logs a poll summary.
async fn poll_project(repo_root: PathBuf, _cfg: SyncConfig) {
    let opts = TraceDecayOpenOptions::default();
    let Some(layout) = TraceDecay::initialized_store_layout_with_options(&repo_root, &opts).await
    else {
        return; // not indexed — nothing to attach PR branches to yet
    };
    let data_root = layout.data_root;

    let repo_for_discovery = repo_root.clone();
    let Ok(discovery) =
        tokio::task::spawn_blocking(move || discover_open_prs(&repo_for_discovery)).await
    else {
        return;
    };

    let report =
        reconcile_project(&repo_root, &data_root, &discovery, MAX_NEW_TRACKS_PER_CYCLE).await;
    let managed = load_state(&data_root).managed.len();
    log_daemon_event(
        "pr_autotrack",
        &[
            ("project", repo_root.display().to_string()),
            ("action", "poll".to_string()),
            ("tracked_now", managed.to_string()),
            ("new_tracked", report.tracked.len().to_string()),
            ("untracked", report.untracked.len().to_string()),
            ("skipped_forks", report.skipped_forks.len().to_string()),
        ],
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
