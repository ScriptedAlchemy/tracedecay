//! Daemon PR-branch auto-tracking (opt-in via `sync.auto_track_pr_branches`).
//!
//! # What this does
//!
//! When a project enables `sync.auto_track_pr_branches`, a daemon poll loop
//! discovers the open pull requests on the repo's `origin` remote. PR worktree
//! activation requires the daemon's retained code-index scheduler authority;
//! this module deliberately fails closed before Git or durable-state mutation
//! until that authority is injected into the poll runtime.
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
//! repositories; that is deliberately out of scope for the first cut.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";

fn scheduler_unavailable(detail: &str) -> String {
    format!("{CODE_INDEX_SCHEDULER_UNAVAILABLE}: {detail}")
}

use super::branch_admin::StoreAdministration;
use super::log_daemon_event;

mod runtime;
pub(super) use runtime::spawn_with_administration;
pub use runtime::{PrAutotrackTask, spawn, teardown_disabled_project};

#[derive(Clone, Copy)]
struct PrStoreAdministration;

impl PrStoreAdministration {
    fn with_control(
        _daemon: &StoreAdministration,
        _graph: &std::sync::Arc<crate::tracedecay::TraceDecay>,
        _command_control: &PrCommandControl,
    ) -> Self {
        Self
    }

    #[cfg(test)]
    fn state_only(_daemon: &StoreAdministration) -> Self {
        Self
    }
}

/// Filename of the PR-autotrack state sidecar, stored next to `branch-meta.json`
/// in the project's store data root.
const STATE_FILENAME: &str = "pr-autotrack.json";
/// Maximum number of *new* PR branches tracked per poll cycle, so a repo with
/// 100 open PRs ramps up gradually instead of forking 100 syncs at once.
const MAX_NEW_TRACKS_PER_CYCLE: usize = 10;
const PR_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PR_COMMAND_STDOUT_LIMIT: usize = 8 * 1024 * 1024;
const PR_COMMAND_STDERR_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
struct PrCommandControl {
    cancellation: Option<tracedecay_runtime_core::cancellation::CancellationToken>,
    command_timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

impl Default for PrCommandControl {
    fn default() -> Self {
        Self {
            cancellation: None,
            command_timeout: PR_COMMAND_TIMEOUT,
            max_stdout_bytes: PR_COMMAND_STDOUT_LIMIT,
            max_stderr_bytes: PR_COMMAND_STDERR_LIMIT,
        }
    }
}

fn default_pr_command_control() -> &'static PrCommandControl {
    static CONTROL: std::sync::OnceLock<PrCommandControl> = std::sync::OnceLock::new();
    CONTROL.get_or_init(PrCommandControl::default)
}

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
    /// True when discovery may be *incomplete* — e.g. `gh pr list` returned
    /// exactly its page limit, so PRs beyond it were not seen. Reconciliation
    /// suppresses removals against a partial discovery so a still-open PR that
    /// merely fell outside the listing window is never mistaken for closed and
    /// untracked. A failed discovery command is a different case: it never
    /// produces a `PrDiscovery` at all (see [`discover_open_prs`]).
    pub partial: bool,
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
///
/// `limit` is the `--limit` passed to `gh`: if the result count reaches it the
/// listing was truncated (there may be more open PRs), so the discovery is
/// flagged `partial` and reconciliation will not untrack anything this pass.
fn parse_gh_pr_list(json: &str, limit: usize) -> serde_json::Result<PrDiscovery> {
    let prs: Vec<GhPr> = serde_json::from_str(json)?;
    let mut discovery = PrDiscovery {
        partial: limit > 0 && prs.len() >= limit,
        ..Default::default()
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

fn run_git_with_control(
    repo_root: &Path,
    args: &[&str],
    control: &PrCommandControl,
) -> Result<std::process::Output, tracedecay_runtime_core::git::GitCommandError> {
    let mut command = std::process::Command::new(crate::git::git_program());
    command.args(args).current_dir(repo_root);
    disable_git_credential_prompt(&mut command);
    let bounds = tracedecay_runtime_core::git::GitCommandBounds {
        deadline: std::time::Instant::now() + control.command_timeout,
        cancel: control.cancellation.clone(),
        max_stdout_bytes: control.max_stdout_bytes,
        max_stderr_bytes: control.max_stderr_bytes,
    };
    tracedecay_runtime_core::git::bounded_command_output(command, None, &bounds)
}

fn successful_git_with_control(
    repo_root: &Path,
    args: &[&str],
    control: &PrCommandControl,
) -> Option<std::process::Output> {
    run_git_with_control(repo_root, args, control)
        .ok()
        .filter(|output| output.status.success())
}

/// Forbids interactive credential prompts on a spawned git/gh subprocess. The
/// daemon's single poll loop awaits each project sequentially, so one git
/// process blocking on `/dev/tty` for a password (uncached HTTPS credential,
/// passphrase-protected SSH key with no agent) would freeze PR-autotrack for
/// *every* registered project. Failing fast instead keeps the loop live; the
/// failure is then surfaced as a discovery error, never as "zero open PRs".
fn disable_git_credential_prompt(command: &mut std::process::Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo");
}

/// Whether a repo's `origin` remote points at GitHub. Memoized per repo root:
/// the remote URL is effectively constant for a checkout, so re-spawning
/// `git remote get-url origin` every poll cycle (once per project, every minute)
/// only re-decides a constant. A rare remote-URL change is picked up on the next
/// daemon restart.
fn origin_is_github(repo_root: &Path, control: &PrCommandControl) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, bool>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock()
        && let Some(&cached) = map.get(repo_root)
    {
        return cached;
    }
    let result = successful_git_with_control(repo_root, &["remote", "get-url", "origin"], control)
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|url| url.contains("github.com"));
    if let Ok(mut map) = cache.lock() {
        map.insert(repo_root.to_path_buf(), result);
    }
    result
}

/// Upper bound on PRs fetched in one `gh pr list` call. Reaching it means the
/// listing was truncated, which flags the discovery `partial` (removals are then
/// suppressed) rather than silently dropping the tail as if those PRs closed.
const GH_PR_LIST_LIMIT: usize = 1000;

/// Whether the `gh` CLI is installed and runnable. Memoized process-wide: the
/// answer is a property of the host binary, not of any repo, so probing it every
/// poll cycle (once per enabled project, every minute) only re-decides a
/// constant. The daemon restarts to pick up a newly installed `gh`.
fn gh_available(control: &PrCommandControl) -> bool {
    if control
        .cancellation
        .as_ref()
        .is_some_and(tracedecay_runtime_core::cancellation::CancellationToken::is_cancelled)
    {
        return false;
    }
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let mut command = std::process::Command::new("gh");
        command.arg("--version");
        disable_git_credential_prompt(&mut command);
        let bounds = tracedecay_runtime_core::git::GitCommandBounds {
            deadline: std::time::Instant::now() + control.command_timeout,
            cancel: control.cancellation.clone(),
            max_stdout_bytes: control.max_stdout_bytes,
            max_stderr_bytes: control.max_stderr_bytes,
        };
        tracedecay_runtime_core::git::bounded_command_output(command, None, &bounds)
            .is_ok_and(|output| output.status.success())
    })
}

/// Discovers open PR head branches on the repo's `origin` remote.
///
/// Prefers `gh pr list` when `gh` is on PATH and `origin` is GitHub; otherwise
/// falls back to `git ls-remote` and SHA-matching. Same-repo PRs are returned in
/// `open`; fork PRs are recorded in `skipped_forks`.
///
/// Returns `Err` when the underlying discovery command *fails* (auth failure,
/// network outage, expired credentials). A failed command is never collapsed
/// into an empty `PrDiscovery`: the caller must skip reconciliation entirely, so
/// a transient `gh`/`git` failure can never masquerade as "every PR closed" and
/// mass-untrack the managed set. An empty `Ok` result means the remote genuinely
/// has no open PRs.
pub fn discover_open_prs(repo_root: &Path) -> Result<PrDiscovery, String> {
    discover_open_prs_with_control(repo_root, default_pr_command_control())
}

fn discover_open_prs_with_control(
    repo_root: &Path,
    control: &PrCommandControl,
) -> Result<PrDiscovery, String> {
    if origin_is_github(repo_root, control)
        && gh_available(control)
        && let Some(discovery) = discover_via_gh(repo_root, control)
    {
        return Ok(discovery);
    }
    // GitHub discovery was inapplicable, unavailable, or failed. `ls-remote`
    // propagates its own failure as `Err` rather than empty.
    discover_via_ls_remote(repo_root, control)
}

fn discover_via_gh(repo_root: &Path, control: &PrCommandControl) -> Option<PrDiscovery> {
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
    let bounds = tracedecay_runtime_core::git::GitCommandBounds {
        deadline: std::time::Instant::now() + control.command_timeout,
        cancel: control.cancellation.clone(),
        max_stdout_bytes: control.max_stdout_bytes,
        max_stderr_bytes: control.max_stderr_bytes,
    };
    let output = tracedecay_runtime_core::git::bounded_command_output(command, None, &bounds)
        .ok()
        .filter(|output| output.status.success())?;
    let json = String::from_utf8(output.stdout).ok()?;
    parse_gh_pr_list(&json, GH_PR_LIST_LIMIT).ok()
}

fn discover_via_ls_remote(
    repo_root: &Path,
    control: &PrCommandControl,
) -> Result<PrDiscovery, String> {
    let pull_out = successful_git_with_control(
        repo_root,
        &["ls-remote", "origin", "refs/pull/*/head"],
        control,
    )
    .and_then(|o| String::from_utf8(o.stdout).ok())
    .ok_or_else(|| "git ls-remote of PR head refs failed".to_string())?;
    let heads_out =
        successful_git_with_control(repo_root, &["ls-remote", "--heads", "origin"], control)
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .ok_or_else(|| "git ls-remote of head refs failed".to_string())?;
    let pull_heads = parse_ls_remote_pull_heads(&pull_out);
    let head_shas = parse_ls_remote_heads(&heads_out);
    Ok(map_pull_heads_to_branches(&pull_heads, &head_shas))
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
    /// True when removals were skipped because the discovery was `partial`
    /// (possibly truncated) — no managed PR is untracked on an incomplete view.
    pub removals_suppressed: bool,
    /// Tracking or persistence failures surfaced to callers.
    pub failures: Vec<(String, String)>,
}

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

/// Reconciles the managed PR set against a discovery result.
///
/// Additions are bounded by `cap` new tracks per call; removals (closed/merged
/// PRs) are always processed. Idempotent: PRs already managed and still open are
/// left untouched. State is persisted before returning.
pub async fn reconcile_project(
    _graph: std::sync::Arc<crate::tracedecay::TraceDecay>,
    _repo_root: &Path,
    _data_root: &Path,
    _discovery: &PrDiscovery,
    _cap: usize,
) -> ReconcileReport {
    ReconcileReport {
        failures: vec![(
            "project".to_owned(),
            scheduler_unavailable(
                "code-index scheduler authority is unavailable for PR worktree activation",
            ),
        )],
        ..ReconcileReport::default()
    }
}

async fn reconcile_project_with_administration(
    repo_root: &Path,
    data_root: &Path,
    discovery: &PrDiscovery,
    cap: usize,
    administration: PrStoreAdministration,
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
    report
}

/// Refuses PR activation until the daemon injects its retained code-index
/// scheduler. The refusal precedes fetch, checkout, or durable-state mutation.
async fn track_pr(
    _repo_root: &Path,
    _data_root: &Path,
    _pr: &DiscoveredPr,
    _administration: PrStoreAdministration,
) -> std::result::Result<ManagedPr, String> {
    Err(scheduler_unavailable(
        "code-index scheduler authority is unavailable for PR worktree activation",
    ))
}

/// Refuses retirement without the same retained scheduler authority. Managed
/// state and Git artifacts remain intact for a later authoritative retry.
async fn remove_pr_store(
    _repo_root: &Path,
    _data_root: &Path,
    _label: &str,
    _administration: PrStoreAdministration,
) -> std::result::Result<(), String> {
    Err(scheduler_unavailable(
        "code-index scheduler authority is unavailable for PR worktree retirement",
    ))
}

/// Untracks a managed PR: removes its branch store, its worktree, its local
/// tracking branch, and its ref. The Git artifacts are released only after the
/// coordinator reports that the store is gone (or was already absent).
async fn untrack_pr(
    repo_root: &Path,
    data_root: &Path,
    label: &str,
    managed: &ManagedPr,
    administration: PrStoreAdministration,
) -> std::result::Result<(), String> {
    let expected_label = pr_label(managed.pr);
    let legacy_label = format!("pr/{}", managed.pr);
    let expected_worktree = data_root
        .join("pr-worktrees")
        .join(format!("pr-{}", managed.pr));
    let expected_ref = pr_tracking_ref(managed.pr);
    if (label != expected_label && label != legacy_label)
        || managed.worktree != expected_worktree
        || managed.tracking_ref != expected_ref
    {
        return Err("managed PR entry does not own the requested branch artifacts".to_string());
    }
    remove_pr_store(repo_root, data_root, label, administration).await
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
async fn sweep_orphan_pr_worktrees(
    repo_root: &Path,
    data_root: &Path,
    desired: &BTreeMap<String, &DiscoveredPr>,
    state: &PrAutotrackState,
    administration: PrStoreAdministration,
) {
    let worktrees_dir = data_root.join("pr-worktrees");
    let Ok(entries) = std::fs::read_dir(&worktrees_dir) else {
        return;
    };
    let managed_prs: std::collections::BTreeSet<u64> =
        state.managed.values().map(|m| m.pr).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
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
                log_daemon_event(
                    "pr_autotrack",
                    &[
                        ("project", repo_root.display().to_string()),
                        ("action", "swept".to_string()),
                        ("pr", number.to_string()),
                        ("reason", "orphan worktree".to_string()),
                    ],
                );
            }
            Err(reason) => log_pr_skip(repo_root, Some(&label), Some(number), &reason),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
