use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{TranscriptSource, ingest_source};

pub mod claude;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_agent;
pub mod cursor_composer;
pub mod git_correlation;
pub mod hermes;
pub mod kiro;
pub mod lcm;
pub(crate) mod message_noise;
pub mod providers;
pub mod shared;
pub mod source;
// `pub` (not `pub(crate)`) only so integration tests can reach the three
// `#[doc(hidden)]` process-safety test helpers; every other item stays
// `pub(crate)`.
pub mod transcript_backfill;
pub mod vibe;
pub mod workflow_index;
pub mod workflow_ingest;
pub mod workflow_state;

pub use providers::{ProviderScope, SessionProvider};
pub use shared::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;

const FILE_TRANSCRIPT_PROVIDERS: &[SessionProvider] = &[
    SessionProvider::Claude,
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
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    match provider {
        None => {
            for provider in FILE_TRANSCRIPT_PROVIDERS {
                push_file_source(&mut sources, *provider);
            }
        }
        Some(provider) => push_file_source(&mut sources, provider),
    }
    let stats = ingest_sources(db, project_root, &sources).await;
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
            stats.merge(ingest_source(db, &source, project_root, None).await)
        } else {
            stats
        }
    } else {
        stats
    };
    let stats = if provider.is_none() || provider == Some(SessionProvider::Hermes) {
        // Hermes stores many sessions in one SQLite file per profile, so it
        // plugs in beside the file-based sources rather than `TranscriptSource`.
        stats.merge(hermes::ingest_for_project(db, project_root).await)
    } else {
        stats
    };
    // Now that messages have landed, attribute any commits that fell inside a
    // recorded session span. Fail-open: a git or DB hiccup never blocks ingest.
    attribute_commits_after_ingest(db).await;
    // Index Claude Code workflow runs + their agents last, so the parent
    // sessions' git spans already exist and each run inherits them. Fail-open:
    // a workflow-ingest hiccup only logs at debug, never blocks session ingest.
    // Runs live in their own tables, so they do not affect `stats`.
    let _ = workflow_ingest::ingest_workflow_runs(db, project_root).await;
    stats
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
fn parse_git_log_commits(stdout: &str) -> Vec<git_correlation::ScannedCommit> {
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

fn push_file_source(sources: &mut Vec<Box<dyn TranscriptSource>>, provider: SessionProvider) {
    match provider {
        SessionProvider::Claude => push_source(sources, claude::ClaudeSource::new()),
        SessionProvider::Codex => push_source(sources, codex::CodexSource::new()),
        SessionProvider::Vibe => push_source(sources, vibe::VibeSource::new()),
        SessionProvider::Cline => push_source(sources, cline_like::ClineLikeSource::cline()),
        SessionProvider::RooCode => push_source(sources, cline_like::ClineLikeSource::roo_code()),
        SessionProvider::Kilo => push_source(sources, cline_like::ClineLikeSource::kilo()),
        SessionProvider::Kiro => push_source(sources, kiro::KiroSource::new()),
        SessionProvider::Cursor | SessionProvider::Hermes => {}
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
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in sources {
        stats = stats.merge(ingest_source(db, source.as_ref(), project_root, None).await);
    }
    stats
}

/// Provider-neutral metadata for an indexed agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub provider: String,
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
}

/// Provider-neutral message payload extracted from an agent transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessageRecord {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<String>,
    pub source_path: Option<String>,
    pub source_offset: Option<i64>,
    pub metadata_json: Option<String>,
}

/// Search hit for session-message full-text lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}

/// Inclusive timestamp bounds for session-message full-text search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchTimeRange {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

/// Relationship and time filters for session-message full-text search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSearchFilters<'a> {
    pub scope: SessionSearchScope,
    pub message_type: SessionMessageType,
    pub parent_session_id: Option<&'a str>,
    pub time_range: SessionSearchTimeRange,
}

impl Default for SessionSearchFilters<'_> {
    fn default() -> Self {
        Self {
            scope: SessionSearchScope::All,
            message_type: SessionMessageType::All,
            parent_session_id: None,
            time_range: SessionSearchTimeRange::default(),
        }
    }
}

/// Scope filter for session-message full-text search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSearchScope {
    All,
    ParentsOnly,
    SubagentsOnly,
}

impl SessionSearchScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "parents_only" => Some(Self::ParentsOnly),
            "subagents_only" => Some(Self::SubagentsOnly),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ParentsOnly => "parents_only",
            Self::SubagentsOnly => "subagents_only",
        }
    }
}

/// Semantic message filter shared by full-text and LCM retrieval. Providers
/// sometimes encode tool results with role `user`, so this is intentionally
/// stronger than the raw role filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMessageType {
    #[default]
    All,
    DirectUser,
    ToolResult,
}

impl SessionMessageType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod git_scan_tests {
    use super::*;

    #[test]
    fn parse_git_log_commits_reads_sha_and_time_skipping_malformed() {
        let stdout = concat!(
            "ABCDEF1234567890 1700000000\n",
            "\n",
            "missing-time\n",
            "cafebabe not-a-number\n",
            "deadbeefdeadbeef 1700000200\n",
        );
        let commits = parse_git_log_commits(stdout);
        assert_eq!(
            commits,
            vec![
                git_correlation::ScannedCommit {
                    sha: "abcdef1234567890".to_string(),
                    committed_at: 1_700_000_000,
                },
                git_correlation::ScannedCommit {
                    sha: "deadbeefdeadbeef".to_string(),
                    committed_at: 1_700_000_200,
                },
            ]
        );
    }

    #[test]
    fn parse_git_log_commits_empty_is_empty() {
        assert!(parse_git_log_commits("").is_empty());
    }
}
