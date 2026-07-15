//! Claude Code transcript source.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session-uuid>.jsonl` (with subagent transcripts
//! under `…/<session>/subagents/*.jsonl`). Each line carries a top-level `type`
//! (`"user"`/`"assistant"`/…), a `message` object (`role`, `content`, `model`,
//! `id`), an ISO-8601 `timestamp`, the session `cwd`, and `sessionId`/`uuid`.
//!
//! The accounting parser already reads these files for cost `turns`; this source
//! reuses the **same** append-only byte-offset machinery to also populate the
//! provider-neutral `session_messages` table. Files are scoped to the current
//! project by their recorded `cwd`, so a project only ingests its own sessions.
//!
//! Beyond `user`/`assistant` conversational turns, a handful of structured
//! record types carry high-signal telemetry that we surface as marker rows or
//! metadata (so `message_search`, git correlation, and LCM can find them):
//! `pr-link` records, `system` compaction boundaries, and model-fallback
//! records become dedicated marker rows; assistant attribution fields and
//! `toolUseResult` edited-file facts ride on the owning message row. See the
//! gate in [`message_from_line`] for the record types we deliberately drop.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::sessions::shared::{
    StoredCursor, TranscriptLocationMetadataKeys, path_belongs_to_project,
};
use crate::sessions::source::{
    JsonlFrameDeferral, ParsedTranscript, TranscriptCursorKey, TranscriptSource,
    collect_files_with_ext,
};
mod cursor;
mod frames;
mod parser;
mod record_metadata;
mod source_records;

use cursor::{claude_cursor_key, claude_source_component};
pub(crate) use frames::{
    ClaudeFrameCoverage, ClaudeSkippedFrame, ClaudeSkippedFrameReason, ClaudeSourceFrame,
    ClaudeSourceFrameScan, identify_claude_source, try_scan_claude_source_frames_with_resume,
};
#[cfg(test)]
pub(crate) use frames::{scan_claude_source_frames, try_scan_claude_source_frames};
#[cfg(test)]
use record_metadata::{SessionAccumulator, session_metadata};
#[cfg(test)]
use source_records::reasoning_from_line;
use source_records::record_cwd;
pub(crate) use source_records::transcript_cwd;
pub(crate) use source_records::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};

#[cfg(test)]
use cursor::{encode_claude_cursor_key, encode_claude_source_id};
#[cfg(test)]
use record_metadata::append_git_operation_metadata;
#[cfg(test)]
use serde_json::Map;
#[cfg(test)]
use source_records::message_from_line;

const PROVIDER: &str = "claude";

/// Shared cross-source telemetry-row `kind` vocabulary. Cursor/Codex adapters
/// tag their structured marker rows with the same strings so `message_search`
/// and LCM can filter marker rows uniformly regardless of which agent produced
/// the transcript.
const KIND_PR_LINK: &str = "pr_link";
const KIND_COMPACT_BOUNDARY: &str = "compact_boundary";
const KIND_MODEL_FALLBACK: &str = "model_fallback";
/// A separate reasoning row per assistant message, matching how Codex and Cursor
/// store the model's thinking as its own `kind="reasoning"` row instead of
/// leaving it buried inside the serialized assistant-message content blob.
const KIND_REASONING: &str = "reasoning";

/// Cap on the capped preview text carried on a marker row.
const MARKER_PREVIEW_BYTES: usize = 2000;

const CLAUDE_SESSION_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "claude_session_cwd",
        "claude_session_worktree",
        "claude_session_location_provenance",
    );
const CLAUDE_MESSAGE_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "claude_message_cwd",
        "claude_message_worktree",
        "claude_message_location_provenance",
    );
/// `~/.claude/projects/<slug>/<…>.jsonl` is at most a few levels deep.
/// Workflow-nested subagents add `subagents/workflows/wf_<id>/` (three more
/// components) so the scan must reach deeper than a top-level session.
const MAX_SCAN_DEPTH: u8 = 9;
/// `cwd` should appear on an early line; scan a few in case the first is a
/// `summary`/meta line without one.
pub(crate) const CWD_PROBE_LINES: usize = 8;

/// Claude Code transcript locator + parser.
pub struct ClaudeSource {
    projects_dir: PathBuf,
    user_scope: Option<UserClaudeScope>,
}

struct UserClaudeScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl ClaudeSource {
    /// Source rooted at the real `~/.claude/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.claude/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            projects_dir: home.join(".claude").join("projects"),
            user_scope: None,
        }
    }

    /// Restricts ingestion to transcript rows that cannot be attributed to any
    /// registered project. `session_id` bounds a live hook ingest; `None`
    /// performs a historical sweep.
    #[must_use]
    pub fn for_user_scope(
        mut self,
        session_id: Option<String>,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        self.user_scope = Some(UserClaudeScope {
            session_id,
            registered_roots,
        });
        self
    }

    /// Retain exactly the frames owned by this source scope and record the
    /// excluded verified ranges for cursor-only persistence.
    pub(crate) fn retain_scoped_frames(
        &self,
        scan: &mut ClaudeSourceFrameScan,
        project_root: &Path,
    ) -> Option<Vec<ClaudeSkippedFrame>> {
        if matches!(
            scan.coverage,
            ClaudeFrameCoverage::Deferred {
                reason: JsonlFrameDeferral::Backlog { .. },
                ..
            }
        ) {
            return None;
        }
        let subagent = claude_subagent_identity(&scan.identity.source_path);
        if self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .is_some_and(|expected| {
                expected != scan.identity.session_id
                    && subagent
                        .as_ref()
                        .is_none_or(|info| expected != info.parent_session_id)
            })
        {
            return None;
        }

        let scan_start = match scan.coverage {
            ClaudeFrameCoverage::Complete { start_offset, .. }
            | ClaudeFrameCoverage::Deferred { start_offset, .. } => start_offset,
        };
        let session_cwd = (scan_start > 0)
            .then(|| transcript_cwd(&scan.identity.source_path))
            .flatten()
            .or_else(|| {
                if scan_start == 0 {
                    scan.frames
                        .iter()
                        .filter_map(ClaudeSourceFrame::scope_value)
                        .find_map(record_cwd)
                } else {
                    None
                }
            })
            .or_else(|| {
                subagent
                    .as_ref()
                    .and_then(|info| transcript_cwd(&info.parent_transcript_path))
            });
        let mut retained = Vec::with_capacity(scan.frames.len());
        let mut excluded = Vec::new();
        for frame in scan.frames.drain(..) {
            let record = frame.scope_value()?;
            let line_cwd = record_cwd(record).or_else(|| session_cwd.clone());
            let include = self.user_scope.as_ref().map_or_else(
                || {
                    line_cwd
                        .as_deref()
                        .is_some_and(|cwd| path_belongs_to_project(cwd, project_root))
                },
                |scope| {
                    line_cwd.as_deref().is_none_or(|cwd| {
                        !scope
                            .registered_roots
                            .iter()
                            .any(|root| path_belongs_to_project(cwd, root))
                    })
                },
            );
            if include {
                retained.push(frame);
            } else {
                excluded.push(ClaudeSkippedFrame {
                    offset: frame.offset,
                    end_offset: frame.end_offset,
                    resume_fingerprint: frame.resume_fingerprint,
                    reason: ClaudeSkippedFrameReason::OutOfScope,
                });
            }
        }
        scan.frames = retained;
        scan.skipped_frames.extend(excluded.iter().copied());
        scan.scope = Some(frames::ClaudeFrameScope {
            project_root: project_root.to_path_buf(),
        });
        Some(excluded)
    }
}

/// Compatibility entry for projectless Claude transcript ingestion.
///
/// Durable writes always pass through the observation coordinator so callers
/// cannot bypass sanitization receipts, observation cursors, or projections.
pub async fn ingest_user_sessions(
    db: &crate::global_db::GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> crate::sessions::shared::TranscriptIngestStats {
    match try_ingest_user_sessions(db, profile_root, session_id, registered_roots).await {
        Ok(stats) => stats,
        Err(error) => {
            let failure = crate::sessions::classify_claude_observation_failure(&error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "Claude compatibility ingest failed"
            );
            crate::sessions::shared::TranscriptIngestStats::default()
        }
    }
}

async fn try_ingest_user_sessions(
    db: &crate::global_db::GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> Result<
    crate::sessions::shared::TranscriptIngestStats,
    crate::sessions::claude_observation::ClaudeObservationIngestError,
> {
    let Some(source) = ClaudeSource::new() else {
        return Ok(crate::sessions::shared::TranscriptIngestStats::default());
    };
    let source = source.for_user_scope(session_id, registered_roots);
    try_ingest_user_sessions_with_source(db, profile_root, &source).await
}

pub(crate) async fn try_ingest_user_sessions_with_source(
    db: &crate::global_db::GlobalDb,
    profile_root: &Path,
    source: &ClaudeSource,
) -> Result<
    crate::sessions::shared::TranscriptIngestStats,
    crate::sessions::claude_observation::ClaudeObservationIngestError,
> {
    crate::sessions::claude_observation::ingest_source_with_observations(
        db,
        source,
        profile_root,
        tracedecay_domain::ObservationScopeV1::Profile,
        None,
        crate::application::observation::ObservationCancellation::default(),
    )
    .await
    .map(|stats| stats.transcript)
}

impl TranscriptSource for ClaudeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        if let Some(session_id) = self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
        {
            let mut paths = Vec::new();
            let Ok(projects) = std::fs::read_dir(&self.projects_dir) else {
                return paths;
            };
            for project in projects.flatten().map(|entry| entry.path()) {
                if !project.is_dir() {
                    continue;
                }
                let transcript = project.join(format!("{session_id}.jsonl"));
                if transcript.is_file() {
                    paths.push(transcript);
                }
                paths.extend(collect_files_with_ext(
                    &project.join(session_id).join("subagents"),
                    "jsonl",
                    MAX_SCAN_DEPTH,
                ));
            }
            paths.sort();
            paths.dedup();
            return paths;
        }
        // Scan every project slug; `parse_new` filters by recorded `cwd` so each
        // project only ingests its own sessions without us having to replicate
        // Claude's slug-encoding scheme.
        collect_files_with_ext(&self.projects_dir, "jsonl", MAX_SCAN_DEPTH)
    }

    fn cursor_key(&self, transcript_path: &Path) -> TranscriptCursorKey {
        claude_cursor_key(transcript_path)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        self.try_parse_new(path, prev, project_root, max_new_bytes)
            .ok()
            .flatten()
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> crate::sessions::source::TranscriptIngestResult<Option<ParsedTranscript>> {
        parser::try_parse_claude_transcript(self, path, prev, project_root, max_new_bytes)
    }
}
struct ClaudeSubagentInfo {
    parent_session_id: String,
    agent_id: String,
    parent_transcript_path: PathBuf,
    /// `agentType` from the sibling meta.json (e.g. "Explore", "general").
    agent_type: Option<String>,
    /// `description` from the sibling meta.json (the spawn prompt summary).
    description: Option<String>,
    /// `toolUseId` from the sibling meta.json: the parent `tool_use` that
    /// spawned this subagent. Maps to the `parent_tool_use_id` session column.
    parent_tool_use_id: Option<String>,
    /// `spawnDepth` from the sibling meta.json (0 for a top-level subagent).
    spawn_depth: Option<i64>,
    /// The `wf_<id>` run id when this subagent lives under
    /// `subagents/workflows/wf_<id>/`; `None` for a directly-spawned subagent.
    workflow_run_id: Option<String>,
}

/// Facts folded from `agent-<id>.meta.json` (all optional / fail-open).
#[derive(Default)]
struct ClaudeSubagentMeta {
    agent_type: Option<String>,
    description: Option<String>,
    parent_tool_use_id: Option<String>,
    spawn_depth: Option<i64>,
}

/// Detect whether `path` is a subagent transcript and, if so, resolve its
/// identity, parent linkage, optional workflow-run id, and meta.json facts.
///
/// A subagent transcript lives somewhere under a `subagents/` directory owned by
/// its parent session:
///
/// * directly spawned: `…/<parent>/subagents/agent-<id>.jsonl`
/// * workflow-nested:   `…/<parent>/subagents/workflows/wf_<run>/agent-<id>.jsonl`
///
/// The parent is always the directory immediately above `subagents/`, so we walk
/// ancestors for a `subagents` component instead of demanding it be the file's
/// immediate parent. That immediate-parent assumption was a bug: workflow-nested
/// subagents failed it and were ingested as orphan standalone sessions.
fn claude_subagent_identity(path: &Path) -> Option<ClaudeSubagentInfo> {
    let session_id = claude_source_component(path.file_stem()?);

    // Find the `subagents/` ancestor. `ancestors()` yields `path` first, so the
    // file itself can never match the directory name.
    let subagents_dir = path
        .ancestors()
        .find(|anc| anc.file_name().and_then(|name| name.to_str()) == Some("subagents"))?;
    let parent_session_dir = subagents_dir.parent()?;
    let parent_session_id = claude_source_component(parent_session_dir.file_name()?);

    // Capture the workflow run id (`wf_<run>`) when the subagent is nested under
    // `subagents/workflows/wf_<run>/`.
    let workflow_run_id = path
        .ancestors()
        .filter_map(|anc| anc.file_name().and_then(|name| name.to_str()))
        .find(|name| name.starts_with("wf_"))
        .map(str::to_string);

    let agent_id = session_id
        .strip_prefix("agent-")
        .unwrap_or(&session_id)
        .to_string();
    // The parent transcript is the `<parent>.jsonl` sibling of the `<parent>`
    // directory that owns `subagents/`.
    let mut parent_filename = parent_session_dir.file_name()?.to_os_string();
    parent_filename.push(".jsonl");
    let parent_transcript_path = parent_session_dir.parent()?.join(parent_filename);

    let meta = read_subagent_meta(path);
    let sanitize = crate::privacy::sanitize_provider_metadata_text;
    let retain_identifier = |value: Option<String>| {
        value.and_then(|value| {
            (sanitize(&value).as_deref() == Some(value.as_str())).then_some(value)
        })
    };

    Some(ClaudeSubagentInfo {
        parent_session_id,
        agent_id,
        parent_transcript_path,
        agent_type: meta.agent_type.as_deref().and_then(sanitize),
        description: meta.description.as_deref().and_then(sanitize),
        parent_tool_use_id: retain_identifier(meta.parent_tool_use_id),
        spawn_depth: meta.spawn_depth,
        workflow_run_id: retain_identifier(workflow_run_id),
    })
}

/// Read the sibling `agent-<id>.meta.json` next to a subagent transcript. Fail
/// open: a missing or malformed file yields empty facts rather than an error.
fn read_subagent_meta(transcript_path: &Path) -> ClaudeSubagentMeta {
    let mut meta_filename = transcript_path
        .file_stem()
        .unwrap_or_default()
        .to_os_string();
    meta_filename.push(".meta.json");
    let meta_path = transcript_path.with_file_name(meta_filename);
    let Ok(text) = std::fs::read_to_string(&meta_path) else {
        return ClaudeSubagentMeta::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return ClaudeSubagentMeta::default();
    };
    let string_field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    ClaudeSubagentMeta {
        agent_type: string_field("agentType"),
        description: string_field("description"),
        parent_tool_use_id: string_field("toolUseId"),
        spawn_depth: value.get("spawnDepth").and_then(Value::as_i64),
    }
}

#[cfg(test)]
mod tests;
