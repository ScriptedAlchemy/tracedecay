use std::path::{Path, PathBuf};

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

pub const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

pub fn user_sessions_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}

pub async fn open_user_session_db(profile_root: &Path) -> Option<GlobalDb> {
    GlobalDb::open_at(&user_sessions_db_path(profile_root)).await
}

/// All registry paths that may identify project-owned transcript evidence.
pub async fn registered_project_roots() -> Vec<PathBuf> {
    try_registered_project_roots().await.unwrap_or_default()
}

/// Returns `None` when the registry cannot be opened. User-scope ingestion
/// must fail closed in that case: an empty root set is valid for a fresh
/// profile, while an unavailable registry cannot safely prove that evidence
/// is projectless.
pub async fn try_registered_project_roots() -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open().await?;
    registered_project_roots_from(&global).await
}

async fn try_registered_project_roots_at(profile_root: &Path) -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open_at(&profile_root.join("global.db")).await?;
    registered_project_roots_from(&global).await
}

pub(crate) async fn registered_project_roots_from(global: &GlobalDb) -> Option<Vec<PathBuf>> {
    let mut roots = global
        .list_project_paths()
        .await
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    for project in global.list_code_projects(usize::MAX).await {
        roots.push(PathBuf::from(project.canonical_root));
        roots.push(PathBuf::from(project.display_root));
    }
    roots.extend(
        global
            .list_project_alias_paths()
            .await
            .into_iter()
            .map(PathBuf::from),
    );
    roots.sort();
    roots.dedup();
    Some(roots)
}

/// Ingests Codex sessions that have no registered-project attribution into the
/// profile user session store. `session_id` bounds live hook work to one host
/// session; `None` performs historical backfill.
pub async fn ingest_user_codex_sessions(session_id: Option<String>) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_codex_sessions_at(&profile_root, session_id, registered_roots).await
}

pub(crate) async fn ingest_user_codex_sessions_at(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    let Some(source) = codex::CodexSource::new() else {
        return TranscriptIngestStats::default();
    };
    let source = source.for_user_scope(session_id, registered_roots);
    ingest_source(&db, &source, profile_root, None).await
}

pub async fn ingest_user_cursor_sessions() -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_cursor_sessions_at(&profile_root, registered_roots).await
}

async fn ingest_user_cursor_sessions_at(
    profile_root: &Path,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    let (composer_stats, owned) = if let Some(source) = cursor_composer::CursorComposerSource::new()
    {
        let outcome = source
            .ingest_user(
                &db,
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
    composer_stats.merge(ingest_source(&db, &source, profile_root, None).await)
}

pub async fn ingest_user_global_sources() -> TranscriptIngestStats {
    ingest_user_global_sources_for_provider(None).await
}

fn provider_selected(scope: Option<SessionProvider>, candidate: SessionProvider) -> bool {
    scope.is_none() || scope == Some(candidate)
}

/// Keeps the profile-level session store current without touching providers
/// outside an explicitly requested message-search scope.
pub async fn ingest_user_global_sources_for_provider(
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(&profile_root, provider, roots).await
}

pub(crate) async fn ingest_user_global_sources_for_provider_at(
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Some(roots) = try_registered_project_roots_at(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(profile_root, provider, roots).await
}

async fn ingest_user_global_sources_for_provider_with_roots(
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    if provider_selected(provider, SessionProvider::Codex) {
        stats = stats.merge(ingest_user_codex_sessions_at(profile_root, None, roots.clone()).await);
    }
    if provider_selected(provider, SessionProvider::Cursor) {
        stats = stats.merge(ingest_user_cursor_sessions_at(profile_root, roots.clone()).await);
    }
    let Some(db) = open_user_session_db(profile_root).await else {
        return stats;
    };
    if provider_selected(provider, SessionProvider::Hermes) {
        stats = stats.merge(hermes::ingest_user_sessions(&db, &roots).await);
    }
    if provider_selected(provider, SessionProvider::Claude) {
        stats =
            stats.merge(claude::ingest_user_sessions(&db, profile_root, None, roots.clone()).await);
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
        let source_stats = ingest_source(&db, source.as_ref(), profile_root, None).await;
        stats = stats.merge(source_stats);
    }
    if stats.messages_upserted > 0 {
        crate::hooks::schedule_user_session_review(
            provider.map_or("all", SessionProvider::id),
            None,
        );
    }
    stats
}

const STARTUP_USER_INGEST_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct StartupUserIngestState {
    running: bool,
    last_completed: Option<std::time::Instant>,
}

static STARTUP_USER_INGESTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, StartupUserIngestState>>,
> = std::sync::OnceLock::new();

struct StartupUserIngestGuard {
    profile_root: PathBuf,
    completed: bool,
}

impl StartupUserIngestGuard {
    fn claim(profile_root: PathBuf) -> Option<Self> {
        let ingests = STARTUP_USER_INGESTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
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

/// Coalesces the profile-wide user transcript sweep shared by every project
/// server created during daemon startup. Live hooks still call
/// [`ingest_user_global_sources`] directly, so the cooldown cannot hide a
/// completed turn.
pub(crate) async fn ingest_user_global_sources_for_startup() -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(mut guard) = StartupUserIngestGuard::claim(profile_root) else {
        return TranscriptIngestStats::default();
    };
    let stats = ingest_user_global_sources().await;
    guard.completed = true;
    stats
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
    ingest_project_sources_for_provider(db, project_root, provider, true).await
}

/// Project-store half of catch-up. Cross-project search runs user ingestion
/// once, then calls this per destination; Hermes can be excluded because its
/// dedicated multi-destination driver scans each source database only once.
pub(crate) async fn ingest_project_sources_for_provider(
    db: &GlobalDb,
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
    let stats =
        if include_hermes && (provider.is_none() || provider == Some(SessionProvider::Hermes)) {
            // Hermes stores many sessions in one SQLite file per profile, so it
            // plugs in beside the file-based sources rather than `TranscriptSource`.
            stats.merge(hermes::ingest_for_project(db, project_root).await)
        } else {
            stats
        };
    finalize_project_ingest(db, project_root).await;
    stats
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

/// Daemon-startup variant that coalesces the profile-wide user sweep while
/// still running the active project's independent ingestion pass.
pub(crate) async fn ingest_global_sources_for_startup(
    db: &GlobalDb,
    project_root: &Path,
) -> TranscriptIngestStats {
    let user = ingest_user_global_sources_for_startup().await;
    user.merge(ingest_project_sources_for_provider(db, project_root, None, true).await)
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

pub use tracedecay_sessions::{
    SessionMessageRecord, SessionMessageSearchResult, SessionMessageType, SessionRecord,
    SessionSearchFilters, SessionSearchScope, SessionSearchTimeRange,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod git_scan_tests {
    use super::*;

    #[tokio::test]
    async fn registered_project_roots_include_modern_registry_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().join("repo");
        let worktree = temp.path().join("repo-worktree");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let canonical = std::fs::canonicalize(canonical).unwrap();
        let worktree = std::fs::canonicalize(worktree).unwrap();
        let db = GlobalDb::open_at(&temp.path().join("global.db"))
            .await
            .unwrap();
        db.upsert_code_project("project-1", &canonical, None, None, None)
            .await
            .unwrap();
        db.upsert_project_alias(&worktree, "project-1")
            .await
            .unwrap();

        let roots = registered_project_roots_from(&db).await.unwrap();

        assert!(roots.contains(&canonical));
        assert!(roots.contains(&worktree));
    }

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

    #[test]
    fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
        let profile = tempfile::tempdir().unwrap().path().to_path_buf();
        let first = StartupUserIngestGuard::claim(profile.clone()).expect("first claim");
        assert!(StartupUserIngestGuard::claim(profile.clone()).is_none());

        drop(first);
        let mut retry = StartupUserIngestGuard::claim(profile.clone())
            .expect("an incomplete claim must release immediately");
        retry.completed = true;
        drop(retry);

        assert!(
            StartupUserIngestGuard::claim(profile).is_none(),
            "a completed sweep should suppress the startup herd during cooldown"
        );
    }
}
