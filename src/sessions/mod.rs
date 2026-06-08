use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::global_db::GlobalDb;
use crate::sessions::source::{ingest_source, TranscriptIngestStats, TranscriptSource};

pub mod claude;
pub mod cline_like;
pub mod codex;
pub mod cursor;
pub mod source;
pub mod vibe;

/// Ingest transcripts from every hookless, path-discoverable agent whose
/// sessions belong to `project_root`, into the project-local `sessions.db`
/// (`db`). This is the serve-side counterpart to the Cursor hooks: these agents
/// register no end-of-turn hook, so their transcripts are reconciled by the
/// startup catch-up sweep instead. Fail-open and incremental (unchanged files
/// are a no-op).
pub async fn ingest_global_sources(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
    if let Some(source) = claude::ClaudeSource::new() {
        sources.push(Box::new(source));
    }
    if let Some(source) = codex::CodexSource::new() {
        sources.push(Box::new(source));
    }
    if let Some(source) = vibe::VibeSource::new() {
        sources.push(Box::new(source));
    }
    if let Some(source) = cline_like::ClineLikeSource::cline() {
        sources.push(Box::new(source));
    }
    if let Some(source) = cline_like::ClineLikeSource::roo_code() {
        sources.push(Box::new(source));
    }
    if let Some(source) = cline_like::ClineLikeSource::kilo() {
        sources.push(Box::new(source));
    }
    ingest_sources(db, project_root, &sources).await
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
