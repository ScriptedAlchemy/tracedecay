use std::process::Stdio;
use std::time::Duration;

use crate::observation::ObservationCancellation;

use super::*;

const MAX_REFLOG_ENTRIES: usize = 10_000;

#[derive(Clone)]
pub struct BoundedGitControl {
    cancellation: ObservationCancellation,
    command_timeout: Duration,
}

impl BoundedGitControl {
    pub fn new(cancellation: ObservationCancellation, command_timeout: Duration) -> Self {
        Self {
            cancellation,
            command_timeout,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedBackfillInterruption {
    Cancelled,
    CommandTimedOut,
}

#[derive(Debug)]
pub struct BoundedBackfillOutcome {
    pub stats: BackfillStats,
    pub committed: bool,
    pub interruption: Option<BoundedBackfillInterruption>,
}

pub async fn run_bounded_backfill<S, E>(
    session_store: &S,
    analytics_events: &[E],
    opts: &BackfillOptions,
    control: &BoundedGitControl,
) -> Result<BoundedBackfillOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    E: AnalyticsSessionTimestampSource,
{
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let rows = session_activity_rows(&snapshot, opts.limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;
    drop(snapshot);

    let mut analytics_ts: std::collections::HashMap<(String, String), Vec<i64>> =
        std::collections::HashMap::new();
    for event in analytics_events {
        if let Some(timestamp) = event.as_analytics_session_timestamp() {
            analytics_ts
                .entry((timestamp.provider, timestamp.session_id))
                .or_default()
                .push(timestamp.timestamp);
        }
    }

    let mut stats = BackfillStats::default();
    let mut committed = false;
    for row in &rows {
        if control.cancellation.is_cancelled() {
            return Ok(BoundedBackfillOutcome {
                stats,
                committed,
                interruption: Some(BoundedBackfillInterruption::Cancelled),
            });
        }
        stats.sessions_scanned = stats.sessions_scanned.saturating_add(1);
        match backfill_one_bounded(
            session_store,
            opts,
            row,
            &analytics_ts,
            &mut stats,
            &mut committed,
            control,
        )
        .await
        {
            Ok(()) => {}
            Err(BoundedSessionError::Skip(reason)) => stats.record_skip(reason),
            Err(BoundedSessionError::Interrupted(interruption)) => {
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    interruption: Some(interruption),
                });
            }
        }
    }
    Ok(BoundedBackfillOutcome {
        stats,
        committed,
        interruption: None,
    })
}

enum BoundedSessionError {
    Skip(BackfillSkipReason),
    Interrupted(BoundedBackfillInterruption),
}

impl From<BoundedBackfillInterruption> for BoundedSessionError {
    fn from(value: BoundedBackfillInterruption) -> Self {
        Self::Interrupted(value)
    }
}

async fn backfill_one_bounded<S: GitCorrelationSessionStore>(
    session_store: &S,
    opts: &BackfillOptions,
    row: &SessionActivityRow,
    analytics_ts: &std::collections::HashMap<(String, String), Vec<i64>>,
    stats: &mut BackfillStats,
    committed: &mut bool,
    control: &BoundedGitControl,
) -> Result<(), BoundedSessionError> {
    let (mut win_start, win_end) = row.window().ok_or(BoundedSessionError::Skip(
        BackfillSkipReason::NoActivityWindow,
    ))?;
    if win_end < opts.since {
        return Err(BoundedSessionError::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    win_start = win_start.max(opts.since);
    if win_start > win_end {
        return Err(BoundedSessionError::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    if row.project_path.trim().is_empty() {
        return Err(BoundedSessionError::Skip(BackfillSkipReason::NotAWorktree));
    }
    let worktree_path = std::path::Path::new(row.project_path.trim());
    let worktree_root =
        tracedecay_runtime_core::worktree::discover_git_worktree_root(worktree_path)
            .ok_or(BoundedSessionError::Skip(BackfillSkipReason::NotAWorktree))?;
    let worktree = normalize_worktree(&worktree_root.to_string_lossy());

    let reflog = bounded_git_output(
        &worktree_root,
        &[
            "reflog".to_owned(),
            "show".to_owned(),
            format!("-n{MAX_REFLOG_ENTRIES}"),
            "--date=unix".to_owned(),
            "HEAD".to_owned(),
        ],
        control,
    )
    .await?
    .ok_or(BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
    let timeline = branch_timeline_from_reflog(&reflog);
    let current_branch = bounded_git_output(
        &worktree_root,
        &[
            "rev-parse".to_owned(),
            "--abbrev-ref".to_owned(),
            "HEAD".to_owned(),
        ],
        control,
    )
    .await?
    .ok_or(BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
    let branch = current_branch.trim();
    let current_branch = (!branch.is_empty() && branch != "HEAD").then(|| branch.to_owned());

    let analytics_within = analytics_ts
        .get(&(row.provider.clone(), row.session_id.clone()))
        .into_iter()
        .flatten()
        .copied()
        .filter(|timestamp| *timestamp >= win_start && *timestamp <= win_end)
        .collect::<Vec<_>>();
    let segments = window_branch_segments(win_start, win_end, &timeline, current_branch.as_deref());

    for segment in &segments {
        if control.cancellation.is_cancelled() {
            return Err(BoundedBackfillInterruption::Cancelled.into());
        }
        for timestamp in [segment.start, segment.end].into_iter().chain(
            analytics_within
                .iter()
                .copied()
                .filter(|timestamp| *timestamp >= segment.start && *timestamp <= segment.end),
        ) {
            if !opts.dry_run {
                let transaction = session_store
                    .open_write_transaction()
                    .await
                    .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
                super::super::record_span_observation_in_transaction(
                    &transaction,
                    &SpanObservation {
                        provider: row.provider.clone(),
                        session_id: row.session_id.clone(),
                        thread_id: None,
                        branch: segment.branch.clone(),
                        worktree: worktree.clone(),
                        ts: timestamp,
                        source: SpanSource::Backfill,
                    },
                    opts.merge_gap_secs,
                )
                .await
                .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
                GitCorrelationWriteTxn::commit(transaction)
                    .await
                    .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
                *committed = true;
            }
        }
        stats.spans_written = stats.spans_written.saturating_add(1);

        let Some(branch) = segment.branch.as_deref() else {
            continue;
        };
        let log = bounded_git_output(
            &worktree_root,
            &[
                "log".to_owned(),
                branch.to_owned(),
                format!("-n{}", opts.max_commits_per_repo),
                "--pretty=%H %ct".to_owned(),
                format!("--since={}", segment.start),
            ],
            control,
        )
        .await?
        .ok_or(BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
        for (sha, committed_at) in parse_commit_log(&log, opts.max_commits_per_repo) {
            if committed_at < segment.start || committed_at > segment.end {
                continue;
            }
            if control.cancellation.is_cancelled() {
                return Err(BoundedBackfillInterruption::Cancelled.into());
            }
            if opts.dry_run {
                stats.commits_attributed = stats.commits_attributed.saturating_add(1);
                continue;
            }
            let transaction = session_store
                .open_write_transaction()
                .await
                .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
            let inserted = super::super::upsert_commit_session(
                &transaction,
                &CommitSessionRecord {
                    commit_sha: sha,
                    provider: row.provider.clone(),
                    session_id: row.session_id.clone(),
                    branch: Some(branch.to_owned()),
                    worktree: Some(worktree.clone()),
                    committed_at,
                    span_overlap_kind: SpanOverlapKind::WithinSpan,
                    span_id: None,
                    relation: CommitRelation::Observed,
                    evidence: CommitEvidence::ReflogOverlap,
                    confidence: 30,
                    evidence_message_id: None,
                },
            )
            .await
            .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
            GitCorrelationWriteTxn::commit(transaction)
                .await
                .map_err(|_| BoundedSessionError::Skip(BackfillSkipReason::GitError))?;
            *committed = true;
            if inserted {
                stats.commits_attributed = stats.commits_attributed.saturating_add(1);
            }
        }
    }
    Ok(())
}

async fn bounded_git_output(
    worktree: &std::path::Path,
    args: &[String],
    control: &BoundedGitControl,
) -> Result<Option<String>, BoundedBackfillInterruption> {
    if control.cancellation.is_cancelled() {
        return Err(BoundedBackfillInterruption::Cancelled);
    }
    let mut command = tokio::process::Command::new(tracedecay_runtime_core::git::git_program());
    command
        .args(args)
        .current_dir(worktree)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return Ok(None),
    };
    let output = child.wait_with_output();
    tokio::pin!(output);
    let deadline = tokio::time::Instant::now() + control.command_timeout;
    loop {
        tokio::select! {
            result = &mut output => {
                let output = match result {
                    Ok(output) if output.status.success() => output,
                    _ => return Ok(None),
                };
                return Ok(String::from_utf8(output.stdout).ok());
            }
            () = tokio::time::sleep(Duration::from_millis(10)) => {
                if control.cancellation.is_cancelled() {
                    return Err(BoundedBackfillInterruption::Cancelled);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(BoundedBackfillInterruption::CommandTimedOut);
                }
            }
        }
    }
}
