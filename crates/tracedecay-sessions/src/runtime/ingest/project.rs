use std::future::Future;
use std::path::{Path, PathBuf};

use super::authority::{IngestAdmissionBinding, SessionIngestAuthority};
use crate::observation::ObservationCancellation;
use crate::repository_provenance::RepositoryProvenanceAdmissionContext;
use crate::runtime::git_correlation::GitCorrelationSessionStore;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::{SessionProvider, claude_observation, git_correlation};
use tracedecay_domain::{BrainId, ObservationScopeV1, ProjectId, UserProfileId};
use tracedecay_store::StoreShardScopeV1;

use super::failure::{
    IngestPassCoverage, IngestPassOutcome, ProviderRunFold, TranscriptCatchUpFailure,
    claude_catch_up_failure,
};
use super::project_provider::{PROJECT_CATCH_UP_PROVIDERS, ProjectProviderRun};
use super::scheduler::{default_ingest_pass_bounds, merge_project_provider_backpressure};
use super::startup::TranscriptIngestOutcome;
use super::user::provider_selected;

tokio::task_local! {
    static TRANSCRIPT_SOURCE_HOME: PathBuf;
}

pub async fn with_transcript_source_home<F>(home: PathBuf, future: F) -> F::Output
where
    F: Future,
{
    TRANSCRIPT_SOURCE_HOME.scope(home, future).await
}

pub fn home_dir() -> Option<PathBuf> {
    TRANSCRIPT_SOURCE_HOME
        .try_with(Clone::clone)
        .ok()
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
                .map(PathBuf::from)
                .or_else(dirs::home_dir)
        })
}

/// Project-store half of catch-up. Cross-project search runs user ingestion
/// once, then calls this per destination; Hermes can be excluded because its
/// dedicated multi-destination driver scans each source database only once.
///
/// `project_id` must already be the typed registry or repository-marker identity.
pub async fn ingest_project_sources_for_provider<A: SessionIngestAuthority>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    project_root: &Path,
    project_id: Option<ProjectId>,
    provider: Option<SessionProvider>,
    include_hermes: bool,
) -> TranscriptIngestOutcome {
    ingest_project_sources_for_provider_with_cancellation(
        brain_id,
        profile_id,
        registered,
        project_root,
        project_id,
        provider,
        include_hermes,
        &ObservationCancellation::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn ingest_project_sources_for_provider_with_cancellation<A: SessionIngestAuthority>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    project_root: &Path,
    project_id: Option<ProjectId>,
    provider: Option<SessionProvider>,
    include_hermes: bool,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestOutcome {
    ingest_project_sources_for_provider_inner(
        (brain_id, profile_id, registered),
        project_root,
        project_id,
        provider,
        include_hermes,
        cancellation,
    )
    .await
}

/// Standalone callers do not own a registered runtime and therefore fail
/// closed for observation providers. Daemon-owned callers must use
/// [`ingest_project_sources_for_provider`] with their retained registry mount.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn ingest_project_sources_for_provider_without_registered_authority<
    A: SessionIngestAuthority,
>(
    _db: &A,
    _project_root: &Path,
    project_id: Option<ProjectId>,
    provider: Option<SessionProvider>,
    _include_hermes: bool,
) -> TranscriptIngestOutcome {
    if project_id.is_none() {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_identity",
                "project_identity_missing",
                false,
            )],
        );
    }
    TranscriptIngestOutcome::new(
        TranscriptIngestStats::default(),
        vec![TranscriptCatchUpFailure::registered_authority_unavailable(
            provider.map_or("all", SessionProvider::id),
        )],
    )
}

async fn ingest_project_sources_for_provider_inner<A: SessionIngestAuthority>(
    registered: (&BrainId, &UserProfileId, &A),
    project_root: &Path,
    project_id: Option<ProjectId>,
    provider: Option<SessionProvider>,
    include_hermes: bool,
    cancellation: &ObservationCancellation,
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
    let (brain_id, profile_id, registered) = registered;
    let shard = &registered.shard_id();
    if shard.brain_id != *brain_id
        || shard.profile_id != *profile_id
        || shard.scope
            != (StoreShardScopeV1::ProjectSessions {
                project_id: canonical_project_id.clone(),
            })
    {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                provider.map_or("all", SessionProvider::id),
                "project_sessions_authority",
                "project_sessions_authority_mismatch",
                false,
            )],
        );
    }
    // File-transcript multi-source scheduling is retired: every catch-up
    // provider is observation/port driven. Start at complete coverage and fold
    // provider-run backpressure onto that baseline.
    let mut source_outcome = IngestPassOutcome {
        stats: TranscriptIngestStats::default(),
        failures: Vec::new(),
        coverage: IngestPassCoverage::Complete,
        scheduling_state_written: false,
        units_admitted: 0,
        units_completed: 0,
        units_failed: 0,
        byte_bounds_enforced: true,
    };
    let scope = ObservationScopeV1::Project {
        project_id: canonical_project_id.clone(),
    };
    let repository_provenance =
        tracedecay_runtime_core::storage::read_repository_identity_marker(project_root)
            .ok()
            .flatten()
            .and_then(|marker| {
                RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                    project_root,
                    &canonical_project_id,
                    &marker,
                )
            });
    let facade = registered.admission(IngestAdmissionBinding::Project {
        brain_id,
        profile_id,
        project_id: &canonical_project_id,
        repository_provenance,
    });
    let facade = facade.as_ref();
    let provider_byte_cap = default_ingest_pass_bounds().bytes_per_unit;
    let mut provider_runs = ProviderRunFold::default();
    let selected: Vec<SessionProvider> = PROJECT_CATCH_UP_PROVIDERS
        .iter()
        .copied()
        .filter(|candidate| {
            provider_selected(provider, *candidate)
                && (!candidate.scans_all_destinations() || include_hermes)
        })
        .collect();
    let mut attempted = 0usize;
    let mut cancelled = false;
    for candidate in selected.iter().copied() {
        if cancellation.is_cancelled() {
            cancelled = true;
            provider_runs
                .failures
                .push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
        attempted = attempted.saturating_add(1);
        provider_runs.record(
            ProjectProviderRun {
                project_root,
                project_id: &canonical_project_id,
                facade,
                scope: &scope,
                candidate,
                max_new_bytes: provider_byte_cap,
                cancellation,
            }
            .run()
            .await,
        );
        if cancellation.is_cancelled() {
            cancelled = true;
            provider_runs
                .failures
                .push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
    }

    if !cancelled {
        match Box::pin(claude_observation::drain_projection_queue(
            facade,
            &scope,
            cancellation,
        ))
        .await
        {
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
    }
    if cancelled {
        provider_runs.deferred_units = provider_runs.deferred_units.saturating_add(
            u64::try_from(selected.len().saturating_sub(attempted))
                .unwrap_or(u64::MAX)
                .max(1),
        );
    }
    source_outcome.coverage = merge_project_provider_backpressure(
        source_outcome.coverage,
        source_outcome.units_admitted,
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
    if !cancelled {
        finalize_project_ingest(registered, &canonical_project_id, project_root).await;
    }
    source_outcome.stats = source_outcome.stats.merge(provider_runs.stats);
    source_outcome.failures.extend(provider_runs.failures);
    source_outcome.into_transcript_outcome()
}

pub(super) async fn finalize_project_ingest<A: SessionIngestAuthority>(
    db: &A,
    project_id: &ProjectId,
    project_root: &Path,
) {
    // Now that messages have landed, attribute any commits that fell inside a
    // recorded session span. Fail-open: a git or DB hiccup never blocks ingest.
    attribute_commits_after_ingest(db).await;
    // Index Claude Code workflow runs + their agents last, so the parent
    // sessions' git spans already exist and each run inherits them. Fail-open:
    // a workflow-ingest hiccup only logs at debug, never blocks session ingest.
    // Runs live in their own tables, so they do not affect `stats`.
    if let Some(home) = super::home_dir() {
        let _ = crate::runtime::workflow_ingest::ingest_workflow_runs_with_sink(
            &db.workflow_sink(),
            project_id,
            project_root,
            &home.join(".claude").join("projects"),
        )
        .await;
    }
}

/// Runs the bounded commit-attribution sweep against the correlation store.
/// For each `(branch, worktree)` pair touched since the last sweep, scans that
/// branch's git log inside the pair's span window (widened by the merge gap)
/// and attributes overlapping commits to their sessions. Fail-open.
async fn attribute_commits_after_ingest<A: SessionIngestAuthority>(db: &A) {
    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let result = commit_attribution_sweep(&db.git_correlation_store(), gap).await;
    if let Err(error) = result {
        tracing::debug!(%error, "commit attribution sweep skipped");
    }
}

/// One bounded sweep inside a single write transaction, so the sweep watermark
/// can only advance together with the attribution rows it describes.
async fn commit_attribution_sweep<S: GitCorrelationSessionStore>(
    store: &S,
    gap_secs: i64,
) -> Result<usize, git_correlation::GitCorrelationError> {
    let transaction = store.open_write_transaction().await?;
    let attributed =
        git_correlation::run_commit_attribution_sweep(&transaction, gap_secs, |target| {
            git_scan_commits(target, gap_secs)
        })
        .await?;
    git_correlation::GitCorrelationWriteTxn::commit(transaction).await?;
    Ok(attributed)
}

/// Reads commits on one span target's branch within its (gap-widened) window
/// via `git log`.
///
/// Reports [`TargetScan::Unavailable`] — not an empty commit list — when the
/// recorded worktree is gone or `git log` fails, so the sweep holds its
/// watermark and retries the target rather than treating "could not look" as
/// "nothing there" and never revisiting those spans.
pub(super) fn git_scan_commits(
    target: &git_correlation::SpanScanTarget,
    gap_secs: i64,
) -> git_correlation::TargetScan {
    let worktree = Path::new(&target.worktree);
    if !worktree.is_dir() {
        return git_correlation::TargetScan::Unavailable;
    }
    let since = target.window_start.saturating_sub(gap_secs);
    let until = target.window_end.saturating_add(gap_secs);
    let mut command = std::process::Command::new(tracedecay_runtime_core::git::git_program());
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
        return git_correlation::TargetScan::Unavailable;
    };
    if !output.status.success() {
        return git_correlation::TargetScan::Unavailable;
    }
    git_correlation::TargetScan::Scanned(parse_git_log_commits(&String::from_utf8_lossy(
        &output.stdout,
    )))
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
