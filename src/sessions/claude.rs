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
    StoredCursor, TranscriptLocationMetadataKeys, path_belongs_to_project, title_from_messages,
};
use crate::sessions::source::{
    JsonlFrameDeferral, ParsedTranscript, SessionDraft, TranscriptCursorKey, TranscriptSource,
    collect_files_with_ext,
};

mod cursor;
mod frames;
mod record_metadata;
mod source_records;

use cursor::{claude_cursor_key, claude_source_component};
pub(crate) use frames::{
    ClaudeFrameCoverage, ClaudeSkippedFrame, ClaudeSkippedFrameReason, ClaudeSourceFrame,
    ClaudeSourceFrameScan, identify_claude_source, scan_claude_source_frames,
};
use record_metadata::{SessionAccumulator, accumulate_session_facts, session_metadata};
pub(crate) use source_records::transcript_cwd;
pub(crate) use source_records::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};
use source_records::{
    reasoning_from_line, record_cwd, structured_marker_from_line, system_hook_message_from_line,
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
                    reason: ClaudeSkippedFrameReason::OutOfScope,
                });
            }
        }
        scan.frames = retained;
        scan.skipped_frames.extend(excluded.iter().copied());
        scan.scope = Some(frames::ClaudeFrameScope {
            project_root: project_root.to_path_buf(),
            session_cwd,
        });
        Some(excluded)
    }

    /// Fold already scoped and sanitized frames through the existing V1 mapper.
    pub(crate) fn fold_scanned_frames(
        &self,
        scan: &ClaudeSourceFrameScan,
        project_root: &Path,
    ) -> Option<ParsedTranscript> {
        let scope = scan
            .scope
            .as_ref()
            .filter(|scope| scope.project_root == project_root)?;
        let subagent = claude_subagent_identity(&scan.identity.source_path);
        let session_id = scan.identity.session_id.clone();
        let source_path = Path::new(&scan.identity.source_id);
        let project = self.user_scope.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let mut accumulator = SessionAccumulator::default();
        let mut messages = Vec::new();

        for frame in &scan.frames {
            let record = frame.sanitized_record()?;
            accumulate_session_facts(record, &mut accumulator);
            let offset = i64::try_from(frame.offset).ok()?;
            let context = ClaudeRecordContext {
                session_id: &session_id,
                project_key: &project,
                project_path: &project,
                file_generation: scan.file_generation,
                offset: frame.offset,
                session_cwd: scope.session_cwd.as_deref(),
            };
            let mut message = match map_sanitized_claude_record(record, &context) {
                ClaudeRecordDisposition::Message { draft, message } => {
                    drop(draft);
                    Some(*message)
                }
                ClaudeRecordDisposition::NonConversational { record_type } => {
                    drop(record_type);
                    system_hook_message_from_line(
                        record,
                        &session_id,
                        source_path,
                        offset,
                        scope.session_cwd.as_deref(),
                    )
                }
            };
            if message.is_none() {
                message = structured_marker_from_line(
                    record,
                    &session_id,
                    source_path,
                    offset,
                    &mut accumulator,
                );
            }
            if let Some(reasoning) = reasoning_from_line(record, &session_id, source_path, offset) {
                messages.push(reasoning);
            }
            if let Some(message) = message {
                messages.push(message);
            }
        }

        let draft = SessionDraft {
            session_id,
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(
                scope.session_cwd.as_deref(),
                subagent.as_ref(),
                &accumulator,
            ))
            .ok(),
            parent_session_id: subagent.as_ref().map(|info| info.parent_session_id.clone()),
            is_subagent: subagent.is_some(),
            agent_id: subagent.as_ref().map(|info| info.agent_id.clone()),
            parent_tool_use_id: subagent
                .as_ref()
                .and_then(|info| info.parent_tool_use_id.clone()),
        };
        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: scan.next_cursor.state,
        })
    }
}

/// Ingests projectless Claude transcript evidence into the profile session
/// store. Registered-project rows are excluded even when a Claude session
/// crosses workspace boundaries.
pub async fn ingest_user_sessions(
    db: &crate::global_db::GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> crate::sessions::shared::TranscriptIngestStats {
    let Some(source) = ClaudeSource::new() else {
        return crate::sessions::shared::TranscriptIngestStats::default();
    };
    let source = source.for_user_scope(session_id, registered_roots);
    crate::sessions::source::ingest_source(db, &source, profile_root, None).await
}

impl TranscriptSource for ClaudeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
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
        let identity = identify_claude_source(path)?;
        let mut scan = scan_claude_source_frames(identity, prev, max_new_bytes)?;
        if scan.previous_cursor.state != prev || scan.previous_cursor.key != self.cursor_key(path) {
            return None;
        }
        if let ClaudeFrameCoverage::Deferred { reason, .. } = scan.coverage {
            tracing::debug!(
                provider = PROVIDER,
                line_offset = reason.offset(),
                reason = reason.reason_code(),
                "deferring transcript input at strict JSONL frame"
            );
        }
        self.retain_scoped_frames(&mut scan, project_root)?;

        // The legacy V1 sweep predates the observation sanitizer. Reuse the
        // privacy parser's already parsed Value without parsing again; the V2
        // coordinator replaces this step with the sanitizer-produced Value.
        for frame in &mut scan.frames {
            let value = frame.parsed_record()?.value().clone();
            frame.take_parsed_record()?;
            if !frame.set_sanitized_record(value) {
                return None;
            }
        }
        self.fold_scanned_frames(&scan, project_root)
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

    Some(ClaudeSubagentInfo {
        parent_session_id,
        agent_id,
        parent_transcript_path,
        agent_type: meta.agent_type,
        description: meta.description,
        parent_tool_use_id: meta.parent_tool_use_id,
        spawn_depth: meta.spawn_depth,
        workflow_run_id,
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounded_scan_carries_identity_cursor_generation_and_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-42.jsonl");
        let complete = b"{\"type\":\"summary\"}\n";
        std::fs::write(&path, [complete.as_slice(), b"{\"partial\":"].concat()).unwrap();

        let identity = identify_claude_source(&path).unwrap();
        let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

        assert_eq!(scan.identity.provider, "claude");
        assert_eq!(scan.identity.session_id, "session-42");
        assert_eq!(scan.identity.source_id, "claude:session-42");
        assert_eq!(scan.identity.source_path, path);
        assert_eq!(scan.previous_cursor.state, StoredCursor::default());
        assert_eq!(scan.previous_cursor.key, scan.next_cursor.key);
        assert_eq!(scan.file_generation, scan.next_cursor.state.file_id);
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.frames[0].offset, 0);
        assert_eq!(scan.frames[0].end_offset, complete.len() as u64);
        assert_eq!(
            scan.frames[0].parsed_record().unwrap().value()["type"],
            "summary"
        );
        assert_eq!(
            scan.coverage,
            ClaudeFrameCoverage::Deferred {
                start_offset: 0,
                covered_through: complete.len() as u64,
                reason: JsonlFrameDeferral::Partial {
                    offset: complete.len() as u64,
                },
            }
        );
        assert_eq!(scan.next_cursor.state.position, complete.len() as u64);
    }

    #[test]
    fn bounded_scan_reports_backlog_without_advancing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-42.jsonl");
        let contents = b"{\"type\":\"summary\"}\n";
        std::fs::write(&path, contents).unwrap();

        let identity = identify_claude_source(&path).unwrap();
        let scan = scan_claude_source_frames(identity, StoredCursor::default(), Some(1)).unwrap();

        assert!(scan.frames.is_empty());
        assert_eq!(scan.next_cursor.state.position, 0);
        assert_eq!(
            scan.coverage,
            ClaudeFrameCoverage::Deferred {
                start_offset: 0,
                covered_through: 0,
                reason: JsonlFrameDeferral::Backlog {
                    offset: 0,
                    unread_bytes: contents.len() as u64,
                    max_new_bytes: 1,
                },
            }
        );
    }

    #[test]
    fn bounded_scan_blocks_oversized_frame_and_suffix_at_one_mib() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-42.jsonl");
        let oversized = format!(
            "{{\"payload\":\"{}\"}}\n",
            "x".repeat(crate::privacy::PR5_MAX_CLAUDE_RECORD_BYTES)
        );
        std::fs::write(&path, format!("{oversized}{{\"type\":\"summary\"}}\n")).unwrap();

        let identity = identify_claude_source(&path).unwrap();
        let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

        assert!(scan.frames.is_empty());
        assert_eq!(scan.next_cursor.state.position, 0);
        assert!(matches!(
            scan.coverage,
            ClaudeFrameCoverage::Deferred {
                covered_through: 0,
                reason: JsonlFrameDeferral::Oversized { offset: 0 },
                ..
            }
        ));
    }

    #[test]
    fn bounded_scan_exposes_whitespace_ranges_without_parsing_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-42.jsonl");
        let record = b"{\"type\":\"summary\"}\n";
        std::fs::write(&path, [b"\n".as_slice(), record, b" \t\n"].concat()).unwrap();

        let identity = identify_claude_source(&path).unwrap();
        let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.skipped_frames.len(), 2);
        assert_eq!(
            scan.skipped_frames[0],
            ClaudeSkippedFrame {
                offset: 0,
                end_offset: 1,
                reason: ClaudeSkippedFrameReason::Whitespace,
            }
        );
        assert_eq!(
            scan.skipped_frames[1],
            ClaudeSkippedFrame {
                offset: (1 + record.len()) as u64,
                end_offset: (1 + record.len() + 3) as u64,
                reason: ClaudeSkippedFrameReason::Whitespace,
            }
        );
    }

    #[test]
    fn canonical_mapper_emits_one_conversational_message() {
        let record = json!({
            "type": "user",
            "uuid": "user-1",
            "message": {"role": "user", "content": "hello"},
        });
        let context = ClaudeRecordContext {
            session_id: "session-1",
            project_key: "project-1",
            project_path: "/project-1",
            file_generation: 42,
            offset: 9,
            session_cwd: Some(Path::new("/project-1")),
        };

        let ClaudeRecordDisposition::Message { draft, message } =
            map_sanitized_claude_record(&record, &context)
        else {
            panic!("conversational row must map");
        };
        assert_eq!(draft.session_id, "session-1");
        assert_eq!(message.message_id, "user-1");
        assert_eq!(message.kind.as_deref(), Some("message"));
        assert_eq!(message.source_path.as_deref(), Some("claude:session-1"));
        let metadata: Value =
            serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["source_generation"], 42);

        assert!(matches!(
            map_sanitized_claude_record(&json!({"type": "summary"}), &context),
            ClaudeRecordDisposition::NonConversational { .. }
        ));
    }

    #[test]
    fn cursor_key_round_trips_native_bytes_without_collisions() {
        let native_path: Vec<u8> = r"C:\Users\zack\.claude\projects\session.jsonl"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let other_native_path: Vec<u8> = r"C:\Users\other.jsonl"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let key = encode_claude_cursor_key("windows-utf16le", &native_path);
        let encoded = key
            .strip_prefix("tracedecay-claude-cursor-v1-windows-utf16le-")
            .expect("versioned platform prefix");

        assert_eq!(hex::decode(encoded).unwrap(), native_path);
        assert_ne!(
            key,
            encode_claude_cursor_key("windows-utf16le", &other_native_path)
        );
        assert_ne!(
            key,
            encode_claude_cursor_key("unix-bytes", &native_path),
            "platform tag is part of the durable identity"
        );

        let source_id = encode_claude_source_id("windows-utf16le", &native_path);
        let encoded_source = source_id
            .strip_prefix("tracedecay-claude-source-v1-windows-utf16le-")
            .expect("versioned source prefix");
        assert_eq!(hex::decode(encoded_source).unwrap(), native_path);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_that_render_identically_have_distinct_cursor_keys() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let first = PathBuf::from(OsString::from_vec(b"session-\xff.jsonl".to_vec()));
        let second = PathBuf::from(OsString::from_vec(b"session-\xfe.jsonl".to_vec()));
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());

        let source = ClaudeSource::with_home(Path::new("/unused"));
        assert_ne!(
            source.cursor_key(&first).durable_text(),
            source.cursor_key(&second).durable_text()
        );
        let first_identity = identify_claude_source(&first).unwrap();
        let second_identity = identify_claude_source(&second).unwrap();
        assert_ne!(first_identity.session_id, second_identity.session_id);
        assert_ne!(first_identity.source_id, second_identity.source_id);
        assert!(!first_identity.source_id.contains('/'));
    }

    #[test]
    fn unicode_paths_keep_the_legacy_cursor_key() {
        let path = Path::new("/tmp/claude-session.jsonl");
        let source = ClaudeSource::with_home(Path::new("/unused"));

        assert_eq!(
            source.cursor_key(path).durable_text(),
            path.to_string_lossy()
        );
    }

    #[test]
    fn structured_git_operation_becomes_host_commit_evidence() {
        let mut metadata = Map::new();
        append_git_operation_metadata(
            &mut metadata,
            &json!({
                "gitBranch": "feature/attribution",
                "toolUseResult": {
                    "gitOperation": {
                        "commit": {"sha": "ABCDEF12", "kind": "commit"}
                    }
                }
            }),
        );
        assert_eq!(metadata["produced_commit_candidates"], json!(["abcdef12"]));
        assert_eq!(metadata["produced_commit_evidence"], "host_event");
        assert_eq!(metadata["git_branch"], "feature/attribution");
    }

    #[test]
    fn unstructured_user_content_cannot_spoof_commit_evidence() {
        let mut metadata = Map::new();
        append_git_operation_metadata(
            &mut metadata,
            &json!({"message": {"content": "gitOperation commit abcdef12"}}),
        );
        assert!(metadata.is_empty());
    }

    fn assistant_record(content: &Value) -> Value {
        json!({
            "type": "assistant",
            "sessionId": "sess",
            "uuid": "u-assistant",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": "msg_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": content.clone(),
            }
        })
    }

    #[test]
    fn thinking_blocks_are_split_from_the_visible_message_row() {
        let record = assistant_record(&json!([
            {"type": "thinking", "thinking": "First I inspect the parser."},
            {"type": "thinking", "thinking": "Then I add the row."},
            {"type": "tool_use", "name": "Read", "input": {"file_path": "src/lib.rs"}},
            {"type": "text", "text": "Done."}
        ]));
        let path = Path::new("/tmp/sess.jsonl");

        let mut accumulator = SessionAccumulator::default();
        let message = message_from_line(&record, "sess", path, 10, None, &mut accumulator)
            .expect("assistant message row");
        assert_eq!(message.message_id, "msg_1");
        assert_eq!(message.kind.as_deref(), Some("message"));
        assert!(!message.text.contains("First I inspect the parser"));
        assert!(!message.text.contains("Then I add the row"));
        assert!(message.text.contains("src/lib.rs"));
        assert!(message.text.contains("Done."));
        assert_eq!(message.tool_names.as_deref(), Some("Read"));

        let reasoning =
            reasoning_from_line(&record, "sess", path, 10).expect("reasoning row for thinking");
        assert_eq!(reasoning.message_id, "msg_1:thinking");
        assert_eq!(reasoning.kind.as_deref(), Some("reasoning"));
        assert_eq!(reasoning.role, "assistant");
        assert_eq!(reasoning.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(reasoning.ordinal, 10);
        assert_eq!(reasoning.timestamp, Some(1_767_225_605));
        assert_eq!(
            reasoning.text,
            "First I inspect the parser.\n\nThen I add the row."
        );
        let metadata: Value = serde_json::from_str(reasoning.metadata_json.as_deref().unwrap())
            .expect("reasoning metadata json");
        assert_eq!(metadata["source"], "claude_thinking");
        assert_eq!(metadata["parent_message_id"], "msg_1");
        assert_eq!(metadata["thinking_blocks"], 2);
        assert!(metadata.get("redacted_thinking_blocks").is_none());
    }

    #[test]
    fn redacted_only_thinking_records_no_reasoning_row() {
        // Matches Codex's encrypted-reasoning convention: no plaintext, no row.
        let record = assistant_record(&json!([
            {"type": "redacted_thinking", "data": "ENCRYPTED_SHOULD_NOT_INDEX"},
            {"type": "text", "text": "Answer."}
        ]));
        assert!(reasoning_from_line(&record, "sess", Path::new("/tmp/sess.jsonl"), 3).is_none());
    }

    #[test]
    fn mixed_thinking_and_redacted_records_the_redacted_count_but_no_plaintext() {
        let record = assistant_record(&json!([
            {"type": "thinking", "thinking": "Visible reasoning."},
            {"type": "redacted_thinking", "data": "ENCRYPTED_SHOULD_NOT_INDEX"}
        ]));
        let reasoning = reasoning_from_line(&record, "sess", Path::new("/tmp/sess.jsonl"), 4)
            .expect("reasoning row for the plaintext block");
        assert_eq!(reasoning.text, "Visible reasoning.");
        assert!(!reasoning.text.contains("ENCRYPTED"));
        let metadata: Value =
            serde_json::from_str(reasoning.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["thinking_blocks"], 1);
        assert_eq!(metadata["redacted_thinking_blocks"], 1);
    }

    #[test]
    fn assistant_message_without_thinking_records_no_reasoning_row() {
        let record = assistant_record(&json!([{"type": "text", "text": "Just an answer."}]));
        assert!(reasoning_from_line(&record, "sess", Path::new("/tmp/sess.jsonl"), 7).is_none());
    }

    #[test]
    fn reasoning_row_id_falls_back_to_record_uuid_when_message_id_is_absent() {
        let record = json!({
            "type": "assistant",
            "sessionId": "sess",
            "uuid": "u-fallback",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "thinking", "thinking": "Reasoning without a message id."}]
            }
        });
        let reasoning = reasoning_from_line(&record, "sess", Path::new("/tmp/sess.jsonl"), 9)
            .expect("reasoning row");
        assert_eq!(reasoning.message_id, "u-fallback:thinking");
        let metadata: Value =
            serde_json::from_str(reasoning.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["parent_message_id"], "u-fallback");
    }

    #[test]
    fn user_record_never_produces_a_reasoning_row() {
        let record = json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "thinking", "thinking": "nope"}]}
        });
        assert!(reasoning_from_line(&record, "sess", Path::new("/tmp/sess.jsonl"), 1).is_none());
    }
}
