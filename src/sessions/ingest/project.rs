use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{TranscriptSource, try_ingest_source, try_ingest_source_with_store};
use crate::sessions::{
    SessionProvider, claude, claude_observation, cline_like, codex, cursor, cursor_composer,
    git_correlation, hermes, kiro, vibe, workflow_ingest,
};
use crate::store::GlobalDbTranscriptStore;

use super::failure::{
    TranscriptCatchUpFailure, classify_transcript_ingest_failure, claude_catch_up_failure,
};
use super::startup::TranscriptIngestOutcome;
use super::user::{ingest_user_global_sources_for_provider, provider_selected};

const FILE_TRANSCRIPT_PROVIDERS: &[SessionProvider] = &[
    SessionProvider::Codex,
    SessionProvider::Vibe,
    SessionProvider::Cline,
    SessionProvider::RooCode,
    SessionProvider::Kilo,
    SessionProvider::Kiro,
];

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

/// Ingest transcripts from every path-discoverable agent whose sessions
/// belong to `project_root`, into the active project session store (`db`).
/// Hookless agents (Claude, Codex, ...) are reconciled exclusively by this
/// startup catch-up sweep; Cursor additionally has live end-of-turn hooks,
/// and its sweep entry shares the hooks' parse offsets so neither path ever
/// re-ingests the other's work. Fail-open and incremental (unchanged files
/// are a no-op).
pub async fn ingest_global_sources(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    ingest_global_sources_for_provider(db, project_root, None).await
}

pub async fn ingest_global_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let _ = ingest_user_global_sources_for_provider(provider).await;
    let project_id = crate::storage::read_repository_identity_marker(project_root)
        .ok()
        .flatten()
        .map(|marker| marker.project_id);
    ingest_project_sources_for_provider(db, project_root, project_id.as_deref(), provider, true)
        .await
        .stats
}

/// Project-store half of catch-up. Cross-project search runs user ingestion
/// once, then calls this per destination; Hermes can be excluded because its
/// dedicated multi-destination driver scans each source database only once.
pub(crate) async fn ingest_project_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    project_id: Option<&str>,
    provider: Option<SessionProvider>,
    include_hermes: bool,
) -> TranscriptIngestOutcome {
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    match provider {
        None => {
            for provider in FILE_TRANSCRIPT_PROVIDERS {
                push_file_source(&mut sources, *provider);
            }
        }
        Some(provider) => push_file_source(&mut sources, provider),
    }
    let source_outcome = ingest_sources(db, project_root, &sources).await;
    let mut stats = source_outcome.stats;
    let mut failures = source_outcome.failures;
    if provider_selected(provider, SessionProvider::Claude) {
        if let Some(project_id) = project_id {
            match ingest_project_claude_observations(db, project_root, project_id).await {
                Ok(observation_stats) => {
                    stats = stats.merge(observation_stats.transcript);
                }
                Err(error) => {
                    let failure = claude_catch_up_failure("observation", &error);
                    tracing::warn!(
                        reason_code = failure.reason_code,
                        retryable = failure.retryable,
                        "project Claude observation catch-up failed"
                    );
                    failures.push(failure);
                }
            }
        } else {
            let failure = TranscriptCatchUpFailure::new(
                "claude",
                "observation",
                "project_observation_authority_unavailable",
                false,
            );
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "project Claude observation catch-up skipped"
            );
            failures.push(failure);
        }
    }
    let stats = if provider.is_none() || provider == Some(SessionProvider::Cursor) {
        // Cursor's richer composer store (state.vscdb + per-session chat
        // store.db) is authoritative: ingest it first, capturing the set of
        // composer-owned session ids. Then run the JSONL sweep skipping those
        // ids so the two Cursor sources never double-ingest the ~94% of
        // sessions that appear in both. The JSONL sweep still has live hook
        // ingestion and shared parse offsets, so it catches up any session the
        // composer store does not own (e.g. cursor-agent CLI transcripts).
        let (composer_stats, owned) =
            if let Some(source) = cursor_composer::CursorComposerSource::new() {
                let outcome = source
                    .ingest(
                        db,
                        project_root,
                        cursor_composer::DEFAULT_COMPOSER_ENVELOPE_CAP,
                    )
                    .await;
                (
                    TranscriptIngestStats {
                        sessions_upserted: outcome.sessions_upserted,
                        messages_upserted: outcome.messages_upserted,
                    },
                    outcome.owned_session_ids,
                )
            } else {
                (
                    TranscriptIngestStats::default(),
                    std::collections::HashSet::new(),
                )
            };
        let stats = stats.merge(composer_stats);
        if let Some(source) = cursor::CursorSweepSource::new() {
            let source = source.with_skip_session_ids(owned);
            match try_ingest_source(db, &source, project_root, None).await {
                Ok(source_stats) => stats.merge(source_stats),
                Err(error) => {
                    let failure = classify_transcript_ingest_failure("cursor", "sweep", &error);
                    tracing::warn!(reason_code = failure.reason_code, retryable = failure.retryable, error = %error, "project Cursor transcript catch-up failed");
                    failures.push(failure);
                    stats
                }
            }
        } else {
            stats
        }
    } else {
        stats
    };
    let stats =
        if include_hermes && (provider.is_none() || provider == Some(SessionProvider::Hermes)) {
            // Hermes stores many sessions in one SQLite file per profile, so it
            // plugs in beside the file-based sources rather than `TranscriptSource`.
            stats.merge(hermes::ingest_for_project(db, project_root).await)
        } else {
            stats
        };
    finalize_project_ingest(db, project_root).await;
    TranscriptIngestOutcome::new(stats, failures)
}

async fn ingest_project_claude_observations(
    db: &GlobalDb,
    project_root: &Path,
    project_id: &str,
) -> std::result::Result<
    claude_observation::ClaudeObservationIngestStats,
    claude_observation::ClaudeObservationIngestError,
> {
    let Some(source) = claude::ClaudeSource::new() else {
        return Ok(claude_observation::ClaudeObservationIngestStats::default());
    };
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_string())?;
    claude_observation::ingest_source_with_observations(
        db,
        &source,
        project_root,
        tracedecay_domain::ObservationScopeV1::Project { project_id },
        None,
        crate::application::observation::ObservationCancellation::default(),
    )
    .await
}

/// Refreshes derived session data after a caller performs its own optimized
/// transcript ingest (for example, one shared Hermes source sweep).
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
        SessionProvider::Codex => push_source(sources, codex::CodexSource::new()),
        SessionProvider::Vibe => push_source(sources, vibe::VibeSource::new()),
        SessionProvider::Cline => push_source(sources, cline_like::ClineLikeSource::cline()),
        SessionProvider::RooCode => push_source(sources, cline_like::ClineLikeSource::roo_code()),
        SessionProvider::Kilo => push_source(sources, cline_like::ClineLikeSource::kilo()),
        SessionProvider::Kiro => push_source(sources, kiro::KiroSource::new()),
        // Claude is persisted only through the sanitized-observation vertical;
        // Cursor and Hermes have dedicated source drivers.
        SessionProvider::Claude | SessionProvider::Cursor | SessionProvider::Hermes => {}
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

/// Drive a set of sources against `db` for `project_root`. Separated from
/// [`ingest_global_sources`] so tests can supply sources rooted at a temporary
/// home directory instead of the real `~`.
pub(crate) async fn ingest_sources(
    db: &GlobalDb,
    project_root: &Path,
    sources: &[Box<dyn TranscriptSource>],
) -> TranscriptIngestOutcome {
    let store = GlobalDbTranscriptStore::new(db);
    let mut outcome = TranscriptIngestOutcome::new(TranscriptIngestStats::default(), Vec::new());
    for source in sources {
        let provider = source.provider();
        match try_ingest_source_with_store(&store, source.as_ref(), project_root, None).await {
            Ok(source_stats) => {
                outcome.stats = outcome.stats.merge(source_stats);
            }
            Err(error) => {
                let failure = classify_transcript_ingest_failure(provider, "transcript", &error);
                tracing::warn!(provider, reason_code = failure.reason_code, retryable = failure.retryable, error = %error, "project transcript catch-up failed");
                outcome.push_failure(failure);
            }
        }
    }
    outcome
}
