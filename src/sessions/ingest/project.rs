use std::path::{Path, PathBuf};

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::ObservationCancellation;
use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::TranscriptSource;
use crate::sessions::{
    SessionProvider, claude_observation, git_correlation, vibe, workflow_ingest,
};
use tracedecay_domain::{ObservationScopeV1, ProjectId};

use super::failure::{ProviderRunFold, TranscriptCatchUpFailure, claude_catch_up_failure};
use super::project_provider::{PROJECT_CATCH_UP_PROVIDERS, ProjectProviderRun};
use super::scheduler::{
    default_ingest_pass_bounds, ingest_sources, merge_project_provider_backpressure,
};
use super::startup::TranscriptIngestOutcome;
use super::user::provider_selected;

const FILE_TRANSCRIPT_PROVIDERS: &[SessionProvider] = &[SessionProvider::Vibe];

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

/// Reconciles project-scoped provider evidence into the active project store.
/// Migrated providers use daemon-owned observation admission; Vibe remains on
/// the legacy compatibility path. Failures are isolated per provider.
pub async fn ingest_global_sources(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    ingest_global_sources_for_provider(db, project_root, None).await
}

pub async fn ingest_global_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Ok(Some(marker)) = crate::storage::read_repository_identity_marker(project_root) else {
        return TranscriptIngestStats::default();
    };
    let Ok(project_id) = ProjectId::new(marker.project_id) else {
        return TranscriptIngestStats::default();
    };
    ingest_project_sources_for_provider(db, project_root, Some(project_id), provider, true)
        .await
        .stats
}

/// Project-store half of catch-up. Cross-project search runs user ingestion
/// once, then calls this per destination; Hermes can be excluded because its
/// dedicated multi-destination driver scans each source database only once.
///
/// `project_id` must already be the typed registry or repository-marker identity.
pub(crate) async fn ingest_project_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    project_id: Option<ProjectId>,
    provider: Option<SessionProvider>,
    include_hermes: bool,
) -> TranscriptIngestOutcome {
    let Some(canonical_project_id) = project_id else {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_identity",
                "project_identity_missing",
                false,
            )],
        );
    };
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    match provider {
        None => {
            for provider in FILE_TRANSCRIPT_PROVIDERS {
                push_file_source(&mut sources, *provider);
            }
        }
        Some(provider) => push_file_source(&mut sources, provider),
    }
    let mut source_outcome = ingest_sources(db, project_root, &sources).await;
    let scope = ObservationScopeV1::Project {
        project_id: canonical_project_id.clone(),
    };
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
        db,
        canonical_project_id.clone(),
    ));
    let cancellation = ObservationCancellation::default();
    let provider_byte_cap = default_ingest_pass_bounds().bytes_per_unit;
    let mut provider_runs = ProviderRunFold::default();
    for &candidate in PROJECT_CATCH_UP_PROVIDERS {
        if !provider_selected(provider, candidate)
            || (candidate == SessionProvider::Hermes && !include_hermes)
        {
            continue;
        }
        provider_runs.record(
            ProjectProviderRun {
                db,
                project_root,
                project_id: &canonical_project_id,
                facade: &facade,
                scope: &scope,
                candidate,
                max_new_bytes: provider_byte_cap,
                cancellation: &cancellation,
            }
            .run()
            .await,
        );
    }

    match claude_observation::drain_projection_queue(&facade, &scope, &cancellation).await {
        Ok(projection_stats) => {
            provider_runs.stats = provider_runs.stats.merge(projection_stats.transcript);
        }
        Err(error) => {
            let failure = claude_catch_up_failure("projection", &error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "project observation projection drain failed"
            );
            provider_runs.failures.push(failure);
        }
    }
    source_outcome.coverage = merge_project_provider_backpressure(
        source_outcome.coverage,
        provider_runs.units_admitted,
        provider_runs.deferred_units,
    );
    if provider_runs.deferred_units > 0
        && !provider_runs
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_backpressured")
    {
        provider_runs
            .failures
            .push(TranscriptCatchUpFailure::pass_backpressured());
    }
    finalize_project_ingest(db, project_root).await;
    source_outcome.stats = source_outcome.stats.merge(provider_runs.stats);
    source_outcome.failures.extend(provider_runs.failures);
    source_outcome
}

pub(crate) async fn finalize_project_ingest(db: &GlobalDb, project_root: &Path) {
    // Now that messages have landed, attribute any commits that fell inside a
    // recorded session span. Fail-open: a git or DB hiccup never blocks ingest.
    attribute_commits_after_ingest(db).await;
    // Index Claude Code workflow runs + their agents last, so the parent
    // sessions' git spans already exist and each run inherits them. Fail-open:
    // a workflow-ingest hiccup only logs at debug, never blocks session ingest.
    // Runs live in their own tables, so they do not affect `stats`.
    let _ = workflow_ingest::ingest_workflow_runs(db, project_root).await;
}

/// Runs the bounded commit-attribution sweep against the correlation store.
/// For each `(branch, worktree)` pair touched since the last sweep, scans that
/// branch's git log inside the pair's span window (widened by the merge gap)
/// and attributes overlapping commits to their sessions. Fail-open.
async fn attribute_commits_after_ingest(db: &GlobalDb) {
    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let result = db
        .git_run_commit_attribution_sweep(gap, |target| git_scan_commits(target, gap))
        .await;
    if let Err(err) = result {
        tracing::debug!(error = %err, "commit attribution sweep skipped");
    }
}

/// Reads commits on one span target's branch within its (gap-widened) window
/// via `git log`. Returns an empty list on any error so the sweep simply
/// attributes nothing for that target rather than failing. The worktree value
/// is a recorded span path; if it no longer exists on disk the scan yields
/// nothing.
fn git_scan_commits(
    target: &git_correlation::SpanScanTarget,
    gap_secs: i64,
) -> Vec<git_correlation::ScannedCommit> {
    let worktree = Path::new(&target.worktree);
    if !worktree.is_dir() {
        return Vec::new();
    }
    let since = target.window_start.saturating_sub(gap_secs);
    let until = target.window_end.saturating_add(gap_secs);
    let mut command = std::process::Command::new(crate::git::git_program());
    command
        .current_dir(worktree)
        .arg("log")
        .arg(format!("--since={since}"))
        .arg(format!("--until={until}"))
        .arg("--pretty=format:%H %ct");
    // Scope to the recorded branch when known; detached-HEAD spans scan HEAD.
    match target.branch.as_deref() {
        Some(branch) if !branch.is_empty() => {
            command.arg(branch);
        }
        _ => {}
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_git_log_commits(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `%H %ct` lines from `git log` into scanned commits, skipping
/// malformed rows.
pub(super) fn parse_git_log_commits(stdout: &str) -> Vec<git_correlation::ScannedCommit> {
    stdout
        .lines()
        .filter_map(|line| {
            let (sha, ts) = line.trim().split_once(' ')?;
            let committed_at: i64 = ts.trim().parse().ok()?;
            let sha = sha.trim().to_ascii_lowercase();
            if sha.is_empty() {
                return None;
            }
            Some(git_correlation::ScannedCommit { sha, committed_at })
        })
        .collect()
}

pub(super) fn push_file_source(
    sources: &mut Vec<Box<dyn TranscriptSource>>,
    provider: SessionProvider,
) {
    match provider {
        SessionProvider::Vibe => push_source(sources, vibe::VibeSource::new()),
        SessionProvider::Claude
        | SessionProvider::Codex
        | SessionProvider::Cursor
        | SessionProvider::Hermes
        | SessionProvider::Cline
        | SessionProvider::RooCode
        | SessionProvider::Kilo
        | SessionProvider::Kiro => {}
    }
}

fn push_source<T>(sources: &mut Vec<Box<dyn TranscriptSource>>, source: Option<T>)
where
    T: TranscriptSource + 'static,
{
    if let Some(source) = source {
        sources.push(Box::new(source));
    }
}
