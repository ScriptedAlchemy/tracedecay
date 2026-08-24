//! Provider routing and transcript-ingest orchestration.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use libsql::Connection;

use super::{
    claude, cline_like, codex, cursor, cursor_composer, git_correlation, hermes, kiro, vibe,
    workflow_ingest,
};
use crate::SessionProvider;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{TranscriptIngestStore, TranscriptSource, ingest_source};

/// Root-provided store and host adapters required by the reusable ingest policy.
pub trait SessionIngestStore:
    TranscriptIngestStore + hermes::HermesStore + workflow_ingest::WorkflowIngestStore + Sync
{
    fn session_connection(&self) -> &Connection;

    fn ingest_hermes_for_project(
        &self,
        project_root: &Path,
    ) -> impl Future<Output = TranscriptIngestStats> + Send;

    fn ingest_hermes_for_user(
        &self,
        registered_roots: &[PathBuf],
    ) -> impl Future<Output = TranscriptIngestStats> + Send;
}

const FILE_TRANSCRIPT_PROVIDERS: &[SessionProvider] = &[
    SessionProvider::Claude,
    SessionProvider::Codex,
    SessionProvider::Vibe,
    SessionProvider::Cline,
    SessionProvider::RooCode,
    SessionProvider::Kilo,
    SessionProvider::Kiro,
];

fn provider_selected(scope: Option<SessionProvider>, candidate: SessionProvider) -> bool {
    scope.is_none() || scope == Some(candidate)
}

/// Ingests user-scoped Codex transcripts into an already-open user session DB.
pub async fn ingest_user_codex_sessions<S: TranscriptIngestStore>(
    db: &S,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(source) = codex::CodexSource::new() else {
        return TranscriptIngestStats::default();
    };
    let source = source.for_user_scope(session_id, registered_roots);
    ingest_source(db, &source, profile_root, None).await
}

/// Ingests user-scoped Cursor transcripts into an already-open user session DB.
pub async fn ingest_user_cursor_sessions<S: TranscriptIngestStore>(
    db: &S,
    profile_root: &Path,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let (composer_stats, owned) = if let Some(source) = cursor_composer::CursorComposerSource::new()
    {
        let outcome = source
            .ingest_user(
                db,
                &registered_roots,
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
            std::collections::HashSet::default(),
        )
    };
    let Some(source) = cursor::CursorSweepSource::new() else {
        return composer_stats;
    };
    let source = source
        .with_skip_session_ids(owned)
        .for_user_scope(&registered_roots);
    composer_stats.merge(ingest_source(db, &source, profile_root, None).await)
}

/// Keeps one profile-level session store current for the selected providers.
pub async fn ingest_user_sources_for_provider<S: SessionIngestStore>(
    db: &S,
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    if provider_selected(provider, SessionProvider::Codex) {
        stats =
            stats.merge(ingest_user_codex_sessions(db, profile_root, None, roots.clone()).await);
    }
    if provider_selected(provider, SessionProvider::Cursor) {
        stats = stats.merge(ingest_user_cursor_sessions(db, profile_root, roots.clone()).await);
    }
    if provider_selected(provider, SessionProvider::Hermes) {
        stats = stats.merge(db.ingest_hermes_for_user(&roots).await);
    }
    if provider_selected(provider, SessionProvider::Claude) {
        stats =
            stats.merge(claude::ingest_user_sessions(db, profile_root, None, roots.clone()).await);
    }
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    if provider_selected(provider, SessionProvider::Vibe)
        && let Some(source) = vibe::VibeSource::new()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Cline)
        && let Some(source) = cline_like::ClineLikeSource::cline()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::RooCode)
        && let Some(source) = cline_like::ClineLikeSource::roo_code()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Kilo)
        && let Some(source) = cline_like::ClineLikeSource::kilo()
    {
        sources.push(Box::new(source.for_user_scope(roots.clone())));
    }
    if provider_selected(provider, SessionProvider::Kiro)
        && let Some(source) = kiro::KiroSource::new()
    {
        sources.push(Box::new(source.for_user_scope(roots)));
    }
    for source in sources {
        stats = stats.merge(ingest_source(db, source.as_ref(), profile_root, None).await);
    }
    stats
}

/// Ingests one project's file-backed providers and optional Hermes history.
pub async fn ingest_project_sources_for_provider<S: SessionIngestStore>(
    db: &S,
    project_root: &Path,
    provider: Option<SessionProvider>,
    include_hermes: bool,
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
    let stats =
        if include_hermes && (provider.is_none() || provider == Some(SessionProvider::Hermes)) {
            stats.merge(db.ingest_hermes_for_project(project_root).await)
        } else {
            stats
        };
    finalize_project_ingest(db, project_root).await;
    stats
}

/// Refreshes git correlation and workflow state after optimized ingest paths.
pub async fn finalize_project_ingest<S: SessionIngestStore>(db: &S, project_root: &Path) {
    attribute_commits_after_ingest(db).await;
    let _ = workflow_ingest::ingest_workflow_runs(db, project_root).await;
}

async fn attribute_commits_after_ingest<S: SessionIngestStore>(db: &S) {
    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let result =
        git_correlation::run_commit_attribution_sweep(db.session_connection(), gap, |target| {
            git_scan_commits(target, gap)
        })
        .await;
    if let Err(err) = result {
        tracing::debug!(error = %err, "commit attribution sweep skipped");
    }
}

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
    let mut command = std::process::Command::new(tracedecay_runtime_core::git::git_program());
    command
        .current_dir(worktree)
        .arg("log")
        .arg(format!("--since={since}"))
        .arg(format!("--until={until}"))
        .arg("--pretty=format:%H %ct");
    if let Some(branch) = target.branch.as_deref().filter(|branch| !branch.is_empty()) {
        command.arg(branch);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_git_log_commits(&String::from_utf8_lossy(&output.stdout))
}

fn parse_git_log_commits(stdout: &str) -> Vec<git_correlation::ScannedCommit> {
    stdout
        .lines()
        .filter_map(|line| {
            let (sha, ts) = line.trim().split_once(' ')?;
            let committed_at: i64 = ts.trim().parse().ok()?;
            let sha = sha.trim().to_ascii_lowercase();
            (!sha.is_empty()).then_some(git_correlation::ScannedCommit { sha, committed_at })
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

/// Drives sources against one project store.
pub async fn ingest_sources<S: TranscriptIngestStore>(
    db: &S,
    project_root: &Path,
    sources: &[Box<dyn TranscriptSource>],
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in sources {
        stats = stats.merge(ingest_source(db, source.as_ref(), project_root, None).await);
    }
    stats
}

const STARTUP_USER_INGEST_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct StartupUserIngestState {
    running: bool,
    last_completed: Option<std::time::Instant>,
}

static STARTUP_USER_INGESTS: OnceLock<
    Mutex<std::collections::HashMap<PathBuf, StartupUserIngestState>>,
> = OnceLock::new();

/// Single-flight guard for profile-wide startup ingestion.
pub struct StartupUserIngestGuard {
    profile_root: PathBuf,
    completed: bool,
}

impl StartupUserIngestGuard {
    pub fn claim(profile_root: PathBuf) -> Option<Self> {
        let ingests =
            STARTUP_USER_INGESTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        let mut ingests = ingests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = ingests.entry(profile_root.clone()).or_default();
        if state.running
            || state
                .last_completed
                .is_some_and(|completed| completed.elapsed() < STARTUP_USER_INGEST_COOLDOWN)
        {
            return None;
        }
        state.running = true;
        Some(Self {
            profile_root,
            completed: false,
        })
    }

    pub fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for StartupUserIngestGuard {
    fn drop(&mut self) {
        let Some(ingests) = STARTUP_USER_INGESTS.get() else {
            return;
        };
        let mut ingests = ingests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = ingests.entry(self.profile_root.clone()).or_default();
        state.running = false;
        if self.completed {
            state.last_completed = Some(std::time::Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_scoped_user_catch_up_excludes_unrelated_providers() {
        assert!(provider_selected(
            Some(SessionProvider::Hermes),
            SessionProvider::Hermes
        ));
        for unrelated in [
            SessionProvider::Codex,
            SessionProvider::Cursor,
            SessionProvider::Claude,
            SessionProvider::Vibe,
            SessionProvider::Cline,
            SessionProvider::RooCode,
            SessionProvider::Kilo,
            SessionProvider::Kiro,
        ] {
            assert!(!provider_selected(Some(SessionProvider::Hermes), unrelated));
        }
        assert!(provider_selected(None, SessionProvider::Codex));
        assert!(provider_selected(None, SessionProvider::Hermes));
    }

    #[test]
    fn parse_git_log_commits_reads_sha_and_time_skipping_malformed() {
        let stdout = concat!(
            "ABCDEF1234567890 1700000000\n",
            "\n",
            "missing-time\n",
            "cafebabe not-a-number\n",
            "deadbeefdeadbeef 1700000200\n",
        );
        assert_eq!(
            parse_git_log_commits(stdout),
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

    #[test]
    fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
        let profile = tempfile::tempdir().unwrap().path().to_path_buf();
        let first = StartupUserIngestGuard::claim(profile.clone()).expect("first claim");
        assert!(StartupUserIngestGuard::claim(profile.clone()).is_none());

        drop(first);
        let mut retry = StartupUserIngestGuard::claim(profile.clone())
            .expect("an incomplete claim must release immediately");
        retry.complete();
        drop(retry);

        assert!(
            StartupUserIngestGuard::claim(profile).is_none(),
            "a completed sweep should suppress the startup herd during cooldown"
        );
    }
}
