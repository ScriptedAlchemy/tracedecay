use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{ingest_source, TranscriptSource};

pub mod claude;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_agent;
pub mod hermes;
pub mod kiro;
pub mod lcm;
pub mod providers;
pub mod shared;
pub mod source;
pub(crate) mod transcript_backfill;
pub mod vibe;

pub use providers::{ProviderScope, SessionProvider};

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
    for candidate in providers_for_ingest(provider) {
        match candidate {
            SessionProvider::Claude => push_source(&mut sources, claude::ClaudeSource::new()),
            SessionProvider::Codex => push_source(&mut sources, codex::CodexSource::new()),
            SessionProvider::Vibe => push_source(&mut sources, vibe::VibeSource::new()),
            SessionProvider::Cline => {
                push_source(&mut sources, cline_like::ClineLikeSource::cline())
            }
            SessionProvider::RooCode => {
                push_source(&mut sources, cline_like::ClineLikeSource::roo_code());
            }
            SessionProvider::Kilo => push_source(&mut sources, cline_like::ClineLikeSource::kilo()),
            SessionProvider::Kiro => push_source(&mut sources, kiro::KiroSource::new()),
            SessionProvider::Cursor | SessionProvider::Hermes => {}
        }
    }
    let stats = ingest_sources(db, project_root, &sources).await;
    let stats = if provider.is_none() || provider == Some(SessionProvider::Cursor) {
        // Cursor has live hook ingestion, but transcripts written before a
        // project was indexed (or while hooks were absent) need this catch-up
        // path; shared parse offsets make hook-ingested files no-ops.
        if let Some(source) = cursor::CursorSweepSource::new() {
            stats.merge(ingest_source(db, &source, project_root, None).await)
        } else {
            stats
        }
    } else {
        stats
    };
    if provider.is_none() || provider == Some(SessionProvider::Hermes) {
        // Hermes stores many sessions in one SQLite file per profile, so it
        // plugs in beside the file-based sources rather than `TranscriptSource`.
        stats.merge(hermes::ingest_for_project(db, project_root).await)
    } else {
        stats
    }
}

fn providers_for_ingest(selected: Option<SessionProvider>) -> &'static [SessionProvider] {
    match selected {
        None => &[
            SessionProvider::Claude,
            SessionProvider::Codex,
            SessionProvider::Vibe,
            SessionProvider::Cline,
            SessionProvider::RooCode,
            SessionProvider::Kilo,
            SessionProvider::Kiro,
        ],
        Some(SessionProvider::Claude) => &[SessionProvider::Claude],
        Some(SessionProvider::Codex) => &[SessionProvider::Codex],
        Some(SessionProvider::Vibe) => &[SessionProvider::Vibe],
        Some(SessionProvider::Cline) => &[SessionProvider::Cline],
        Some(SessionProvider::RooCode) => &[SessionProvider::RooCode],
        Some(SessionProvider::Kilo) => &[SessionProvider::Kilo],
        Some(SessionProvider::Kiro) => &[SessionProvider::Kiro],
        Some(SessionProvider::Cursor | SessionProvider::Hermes) => &[],
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

/// Scope filter for session-message full-text search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSearchScope {
    All,
    ParentsOnly,
    SubagentsOnly,
}
