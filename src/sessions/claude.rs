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

use crate::privacy::protect_sensitive_structural_id;
use crate::sessions::shared::{
    StoredCursor, TranscriptLocationMetadataKeys, path_belongs_to_project,
};
use crate::sessions::snapshot_observation::{
    MAX_SNAPSHOT_METADATA_BYTES, read_snapshot_text_bounded,
};
use crate::sessions::source::{
    FileDiscoveryLimit, FileDiscoveryReport, JsonlFrameDeferral, ParsedTranscript,
    TranscriptCursorKey, TranscriptDiscoveryBounds, TranscriptSource, bound_path_list,
    collect_files_with_ext_bounded, path_byte_len,
};
mod canonical_projection;
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
use record_metadata::append_git_operation_metadata;
#[cfg(test)]
use serde_json::Map;
#[cfg(test)]
use source_records::message_from_line;
#[cfg(test)]
use tracedecay_capture::claude::{
    encode_cursor_key as encode_claude_cursor_key, encode_source_id as encode_claude_source_id,
};

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
        let expected_session_id = self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .map(protect_sensitive_structural_id)
            .transpose()
            .ok()?;
        let parent_session_id = subagent
            .as_ref()
            .map(|info| protect_sensitive_structural_id(&info.parent_session_id))
            .transpose()
            .ok()?;
        if expected_session_id.is_some_and(|expected| {
            expected != scan.identity.session_id
                && parent_session_id.as_deref() != Some(expected.as_str())
        }) {
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
                        .map(ClaudeSourceFrame::scope_value)
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
            let record = frame.scope_value();
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

/// Profile ingestion through an already registered host-admission facade.
pub async fn ingest_user_sessions_with_admission(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    admission: &crate::application::host_admission::HostAdmissionFacade<'_>,
) -> crate::sessions::shared::TranscriptIngestStats {
    match crate::sessions::claude_observation::ingest_user_sessions_with_admission(
        profile_root,
        session_id,
        registered_roots,
        admission,
        None,
        crate::application::observation::ObservationCancellation::default(),
    )
    .await
    {
        Ok(stats) => stats.transcript,
        Err(error) => {
            let failure = crate::sessions::classify_claude_observation_failure(&error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "registered Claude ingest failed"
            );
            crate::sessions::shared::TranscriptIngestStats::default()
        }
    }
}

fn discover_claude_session_scoped_paths(
    projects_dir: &Path,
    session_id: &str,
    bounds: TranscriptDiscoveryBounds,
) -> FileDiscoveryReport {
    let mut paths = Vec::new();
    let mut truncated = None;
    let mut skipped_oversized_entries = 0u64;
    let mut bytes_charged = 0u64;
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return FileDiscoveryReport {
            paths,
            truncated,
            skipped_oversized_entries,
            bytes_charged,
        };
    };
    // Stream project slug entries; never collect the full read_dir into a Vec.
    for project_entry in projects.flatten() {
        if truncated.is_some() {
            break;
        }
        let Ok(file_type) = project_entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let project = projects_dir.join(project_entry.file_name());
        let transcript = project.join(format!("{session_id}.jsonl"));
        let remaining = TranscriptDiscoveryBounds {
            max_files: bounds.max_files.saturating_sub(paths.len()),
            max_discovery_bytes: bounds.max_discovery_bytes.saturating_sub(bytes_charged),
            ..bounds
        };
        if remaining.max_files == 0 || remaining.max_discovery_bytes == 0 {
            truncated = Some(FileDiscoveryLimit::FileCount);
            break;
        }
        if transcript.is_file() {
            let path_bytes = path_byte_len(&transcript);
            if path_bytes > remaining.max_path_bytes {
                skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            } else {
                let charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
                if bytes_charged.saturating_add(charge) > bounds.max_discovery_bytes {
                    truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
                    break;
                }
                bytes_charged = bytes_charged.saturating_add(charge);
                paths.push(transcript);
            }
        }
        let subagent_bounds = TranscriptDiscoveryBounds {
            max_files: bounds.max_files.saturating_sub(paths.len()),
            max_discovery_bytes: bounds.max_discovery_bytes.saturating_sub(bytes_charged),
            ..bounds
        };
        if subagent_bounds.max_files == 0 || subagent_bounds.max_discovery_bytes == 0 {
            truncated = Some(FileDiscoveryLimit::FileCount);
            break;
        }
        let subagents = collect_files_with_ext_bounded(
            &project.join(session_id).join("subagents"),
            "jsonl",
            MAX_SCAN_DEPTH,
            subagent_bounds,
        );
        bytes_charged = bytes_charged.saturating_add(subagents.bytes_charged);
        skipped_oversized_entries =
            skipped_oversized_entries.saturating_add(subagents.skipped_oversized_entries);
        paths.extend(subagents.paths);
        if let Some(limit) = subagents.truncated {
            truncated = Some(limit);
            break;
        }
    }
    paths.sort();
    paths.dedup();
    // Re-apply bounds after sort/dedup so materialization stays inside the cap.
    let mut report = bound_path_list(paths, bounds);
    report.truncated = report.truncated.or(truncated);
    report.skipped_oversized_entries = report
        .skipped_oversized_entries
        .saturating_add(skipped_oversized_entries);
    report.bytes_charged = report.bytes_charged.max(bytes_charged);
    report
}

impl TranscriptSource for ClaudeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk())
            .paths
    }

    fn discover_transcript_paths(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        if let Some(session_id) = self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
        {
            return discover_claude_session_scoped_paths(&self.projects_dir, session_id, bounds);
        }
        // Scan every project slug; `parse_new` filters by recorded `cwd` so each
        // project only ingests its own sessions without us having to replicate
        // Claude's slug-encoding scheme.
        collect_files_with_ext_bounded(&self.projects_dir, "jsonl", MAX_SCAN_DEPTH, bounds)
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
    let Ok(Some(text)) =
        read_snapshot_text_bounded(PROVIDER, &meta_path, MAX_SNAPSHOT_METADATA_BYTES)
    else {
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
