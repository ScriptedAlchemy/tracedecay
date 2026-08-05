use std::process::Stdio;
use std::time::Duration;

use crate::observation::ObservationCancellation;

use super::*;

const GIT_OUTPUT_PAGE_SIZE: usize = 256;

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
    SourceUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitHistoryIndexFrontier {
    pub activity_timestamp: i64,
    pub source_rowid: i64,
}

#[derive(Debug)]
pub struct BoundedBackfillOutcome {
    pub stats: BackfillStats,
    pub committed: bool,
    pub frontier: GitHistoryIndexFrontier,
    pub remaining_sessions: u64,
    pub interruption: Option<BoundedBackfillInterruption>,
}

pub async fn run_bounded_history_index_page<S>(
    session_store: &S,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
) -> Result<BoundedBackfillOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
{
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let stored_activity = super::super::read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY)
        .await?
        .unwrap_or_else(|| opts.since.saturating_sub(1));
    let stored_rowid = super::super::read_meta_value(&snapshot, GIT_HISTORY_ROWID_FRONTIER_KEY)
        .await?
        .unwrap_or(0);
    let mut frontier = GitHistoryIndexFrontier {
        activity_timestamp: stored_activity.max(opts.since.saturating_sub(1)),
        source_rowid: if stored_activity >= opts.since.saturating_sub(1) {
            stored_rowid
        } else {
            0
        },
    };
    let requested = opts.limit_sessions.saturating_add(1);
    let mut rows = session_activity_page_after(
        &snapshot,
        frontier.activity_timestamp,
        frontier.source_rowid,
        requested,
    )
    .await
    .map_err(GitCorrelationError::Db)?;
    drop(snapshot);
    let has_more = bounded_page_has_more(rows.len(), opts.limit_sessions);
    rows.truncate(opts.limit_sessions);
    let analytics_ts = std::collections::HashMap::new();

    let mut stats = BackfillStats::default();
    let mut committed = false;
    for row in &rows {
        if control.cancellation.is_cancelled() {
            return Ok(BoundedBackfillOutcome {
                stats,
                committed,
                frontier,
                remaining_sessions: 1,
                interruption: Some(BoundedBackfillInterruption::Cancelled),
            });
        }
        stats.sessions_scanned = stats.sessions_scanned.saturating_add(1);
        let git = match prepare_git_evidence(&row.session, opts, control).await {
            Ok(PreparedGitEvidenceOutcome::Ready(git)) => Some(git),
            Ok(PreparedGitEvidenceOutcome::Skip(reason)) => {
                stats.record_skip(reason);
                None
            }
            Err(BoundedBackfillInterruption::SourceUnavailable) => {
                stats.record_skip(BackfillSkipReason::GitError);
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    frontier,
                    remaining_sessions: 1,
                    interruption: Some(BoundedBackfillInterruption::SourceUnavailable),
                });
            }
            Err(interruption) => {
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    frontier,
                    remaining_sessions: 1,
                    interruption: Some(interruption),
                });
            }
        };
        if let Some(git) = git {
            match super::backfill_one_session(
                session_store,
                &git,
                opts,
                &row.session,
                &analytics_ts,
                &mut stats,
                &mut committed,
            )
            .await
            {
                Ok(()) => {}
                Err(BackfillSkipReason::GitError) => {
                    stats.record_skip(BackfillSkipReason::GitError);
                    return Ok(BoundedBackfillOutcome {
                        stats,
                        committed,
                        frontier,
                        remaining_sessions: 1,
                        interruption: Some(BoundedBackfillInterruption::SourceUnavailable),
                    });
                }
                Err(reason) => stats.record_skip(reason),
            }
        }
        let candidate_frontier = GitHistoryIndexFrontier {
            activity_timestamp: row.activity_timestamp,
            source_rowid: row.source_rowid,
        };
        if !opts.dry_run {
            let persisted = async {
                let transaction = session_store.open_write_transaction().await?;
                let persisted_frontier =
                    super::advance_history_frontier(&transaction, candidate_frontier).await?;
                GitCorrelationWriteTxn::commit(transaction).await?;
                Ok::<_, GitCorrelationError>(persisted_frontier)
            }
            .await;
            frontier = match persisted {
                Ok(frontier) => frontier,
                Err(_) => {
                    return Ok(BoundedBackfillOutcome {
                        stats,
                        committed,
                        frontier,
                        remaining_sessions: 1,
                        interruption: Some(BoundedBackfillInterruption::SourceUnavailable),
                    });
                }
            };
            committed = true;
        }
    }
    Ok(BoundedBackfillOutcome {
        stats,
        committed,
        frontier,
        remaining_sessions: u64::from(has_more),
        interruption: None,
    })
}

const fn bounded_page_has_more(row_count: usize, page_size: usize) -> bool {
    row_count > page_size
}

enum PreparedGitEvidenceOutcome {
    Ready(PreparedGitEvidence),
    Skip(BackfillSkipReason),
}

struct PreparedGitEvidence {
    worktree: std::path::PathBuf,
    reflog: String,
    current_branch: Option<String>,
    commit_logs: std::collections::HashMap<(String, i64), String>,
}

impl GitReflogSource for PreparedGitEvidence {
    fn reflog(&self, worktree: &std::path::Path) -> Option<String> {
        (worktree == self.worktree).then(|| self.reflog.clone())
    }

    fn current_branch(&self, worktree: &std::path::Path) -> Option<String> {
        (worktree == self.worktree)
            .then(|| self.current_branch.clone())
            .flatten()
    }

    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String> {
        if worktree != self.worktree {
            return None;
        }
        self.commit_logs.get(&(branch.to_owned(), since)).cloned()
    }
}

async fn prepare_git_evidence(
    row: &SessionActivityRow,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
) -> Result<PreparedGitEvidenceOutcome, BoundedBackfillInterruption> {
    let Some((mut window_start, window_end)) = row.window() else {
        return Ok(PreparedGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    };
    if window_end < opts.since {
        return Ok(PreparedGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    window_start = window_start.max(opts.since);
    if window_start > window_end {
        return Ok(PreparedGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    if row.project_path.trim().is_empty() {
        return Ok(PreparedGitEvidenceOutcome::Skip(
            BackfillSkipReason::NotAWorktree,
        ));
    }
    let Some(worktree) = tracedecay_runtime_core::worktree::discover_git_worktree_root(
        std::path::Path::new(row.project_path.trim()),
    ) else {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    };
    let reflog = bounded_git_paged_output(
        &worktree,
        &[
            "reflog".to_owned(),
            "show".to_owned(),
            "--date=unix".to_owned(),
            "HEAD".to_owned(),
        ],
        control,
    )
    .await?
    .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let current_branch = bounded_git_output(
        &worktree,
        &[
            "rev-parse".to_owned(),
            "--abbrev-ref".to_owned(),
            "HEAD".to_owned(),
        ],
        control,
    )
    .await?
    .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let branch = current_branch.trim();
    let current_branch = (!branch.is_empty() && branch != "HEAD").then(|| branch.to_owned());
    let timeline = branch_timeline_from_reflog(&reflog);
    let segments = window_branch_segments(
        window_start,
        window_end,
        &timeline,
        current_branch.as_deref(),
    );
    let mut commit_logs = std::collections::HashMap::new();
    for segment in segments {
        if control.cancellation.is_cancelled() {
            return Err(BoundedBackfillInterruption::Cancelled);
        }
        let Some(branch) = segment.branch else {
            continue;
        };
        let log = bounded_git_paged_output(
            &worktree,
            &[
                "log".to_owned(),
                branch.clone(),
                "--pretty=%H %ct".to_owned(),
                format!("--since={}", segment.start),
            ],
            control,
        )
        .await?
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        commit_logs.insert((branch, segment.start), log);
    }
    Ok(PreparedGitEvidenceOutcome::Ready(PreparedGitEvidence {
        worktree,
        reflog,
        current_branch,
        commit_logs,
    }))
}

async fn bounded_git_paged_output(
    worktree: &std::path::Path,
    base_args: &[String],
    control: &BoundedGitControl,
) -> Result<Option<String>, BoundedBackfillInterruption> {
    let mut output = String::new();
    let mut skip = 0_usize;
    loop {
        let mut args = base_args.to_vec();
        args.push(format!("-n{}", GIT_OUTPUT_PAGE_SIZE.saturating_add(1)));
        args.push(format!("--skip={skip}"));
        let Some(page) = bounded_git_output(worktree, &args, control).await? else {
            return Ok(None);
        };
        if !append_git_output_page(&mut output, &page) {
            return Ok(Some(output));
        }
        skip = skip.saturating_add(GIT_OUTPUT_PAGE_SIZE);
    }
}

fn append_git_output_page(output: &mut String, page: &str) -> bool {
    let mut lines = page.lines();
    for line in lines.by_ref().take(GIT_OUTPUT_PAGE_SIZE) {
        output.push_str(line);
        output.push('\n');
    }
    lines.next().is_some()
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

#[cfg(test)]
mod tests {
    use super::{
        BackfillOptions, BoundedBackfillInterruption, BoundedGitControl, GIT_OUTPUT_PAGE_SIZE,
        SessionActivityRow, append_git_output_page, bounded_page_has_more, prepare_git_evidence,
    };

    #[test]
    fn full_git_output_page_reports_that_another_page_remains() {
        let page = (0..=GIT_OUTPUT_PAGE_SIZE)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = String::new();

        assert!(append_git_output_page(&mut output, &page));
        assert_eq!(output.lines().count(), GIT_OUTPUT_PAGE_SIZE);
        assert!(!output.contains(&format!("line-{GIT_OUTPUT_PAGE_SIZE}\n")));
    }

    #[test]
    fn paged_git_output_preserves_history_beyond_ten_thousand_rows() {
        let mut output = String::new();
        for page_index in 0..40 {
            let page = (0..=GIT_OUTPUT_PAGE_SIZE)
                .map(|line_index| {
                    format!("line-{}", page_index * GIT_OUTPUT_PAGE_SIZE + line_index)
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(append_git_output_page(&mut output, &page));
        }
        assert_eq!(output.lines().count(), 10_240);

        let terminal_page = "line-10240\nline-10241";
        assert!(!append_git_output_page(&mut output, terminal_page));
        assert_eq!(output.lines().count(), 10_242);
        assert!(output.ends_with("line-10241\n"));
    }

    #[test]
    fn bounded_history_page_reports_unconsumed_session_suffix() {
        assert!(bounded_page_has_more(51, 50));
        assert!(!bounded_page_has_more(50, 50));
    }

    #[tokio::test]
    async fn missing_worktree_is_retryable_and_does_not_become_a_skip() {
        let missing = std::env::temp_dir().join(format!(
            "tracedecay-missing-git-history-worktree-{}",
            std::process::id()
        ));
        assert!(!missing.exists());
        let row = SessionActivityRow {
            provider: "codex".to_owned(),
            session_id: "session.fixture".to_owned(),
            project_path: missing.to_string_lossy().into_owned(),
            started_at: Some(100),
            ended_at: Some(200),
            message_min_ts: None,
            message_max_ts: None,
        };
        let control = BoundedGitControl::new(
            crate::observation::ObservationCancellation::default(),
            std::time::Duration::from_secs(1),
        );

        assert!(matches!(
            prepare_git_evidence(&row, &BackfillOptions::default(), &control).await,
            Err(BoundedBackfillInterruption::SourceUnavailable)
        ));
    }
}
