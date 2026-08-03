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

use serde_json::{Map, Value};

use tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp;

use crate::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptLocation,
    TranscriptLocationMetadataKeys, append_location_metadata_cached, append_tool_calls_metadata,
    append_tool_event_metadata, append_usage_metadata, content_storage_text_and_tools,
    preview_truncated, title_from_messages,
};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptIngestStore, TranscriptSource,
    collect_files_with_ext, ingest_source, stream_new_jsonl,
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

fn parse_timestamp(value: &str) -> Option<u64> {
    u64::try_from(parse_rfc3339_timestamp(value)?).ok()
}

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
    project_matchers: ProjectRootMatcherCache,
}

struct UserClaudeScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl ClaudeSource {
    /// Source rooted at the real `~/.claude/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = super::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.claude/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            projects_dir: home.join(".claude").join("projects"),
            user_scope: None,
            project_matchers: ProjectRootMatcherCache::default(),
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
}

/// Ingests projectless Claude transcript evidence into the profile session
/// store. Registered-project rows are excluded even when a Claude session
/// crosses workspace boundaries.
pub async fn ingest_user_sessions<S>(
    db: &S,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> crate::runtime::shared::TranscriptIngestStats
where
    S: TranscriptIngestStore,
{
    let Some(source) = ClaudeSource::new() else {
        return crate::runtime::shared::TranscriptIngestStats::default();
    };
    let source = source.for_user_scope(session_id, registered_roots);
    ingest_source(db, &source, profile_root, None).await
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

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let subagent = claude_subagent_identity(path);
        // Cheap session scoping: the first/parent cwd describes where the
        // session began, but individual Claude rows can carry their own cwd.
        // Filter messages per row so sessions that cross worktrees are split
        // into the right project stores without losing transcript truth.
        let session_cwd = transcript_cwd(path).or_else(|| {
            subagent
                .as_ref()
                .and_then(|info| transcript_cwd(&info.parent_transcript_path))
        });

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let session_id = subagent.as_ref().map_or_else(
            || {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            |info| info.session_id.clone(),
        );
        if self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .is_some_and(|expected| {
                expected != session_id
                    && subagent
                        .as_ref()
                        .is_none_or(|info| expected != info.parent_session_id)
            })
        {
            return None;
        }

        // Session-level facts folded across every new line (PR links seen in the
        // session, the set of files edited) so the draft can carry a compact
        // summary alongside the per-row marker rows / metadata.
        let mut accumulator = SessionAccumulator::default();
        let mut messages = Vec::new();
        let project_matcher = self
            .user_scope
            .is_none()
            .then(|| self.project_matchers.get(project_root));
        let registered_root_matchers = self
            .user_scope
            .as_ref()
            .map(|scope| {
                scope
                    .registered_roots
                    .iter()
                    .map(|root| self.project_matchers.get(root))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for line in &new.lines {
            let record = &line.value;
            let line_cwd = record_cwd(record).or_else(|| session_cwd.clone());
            let include = if self.user_scope.is_none() {
                line_cwd.as_deref().map_or(Some(false), |cwd| {
                    project_matcher
                        .as_ref()
                        .map(|matcher| matcher.contains_status(cwd).definitive())
                        .unwrap_or(Some(false))
                })
            } else {
                line_cwd.as_deref().map_or(Some(true), |cwd| {
                    let mut unknown = false;
                    for matcher in &registered_root_matchers {
                        match matcher.contains_status(cwd) {
                            ProjectMembership::Match => return Some(false),
                            ProjectMembership::NoMatch => {}
                            ProjectMembership::Unknown => unknown = true,
                        }
                    }
                    (!unknown).then_some(true)
                })
            }?;
            if !include {
                continue;
            }
            // Conversational turns and system hook signals first; structured
            // marker rows (pr-link/compaction/model-fallback) only when neither
            // matched. Split into separate statements so the `&mut accumulator`
            // borrows never overlap.
            let mut message = message_from_line(
                record,
                &session_id,
                path,
                line.offset,
                session_cwd.as_deref(),
                &mut accumulator,
                &self.project_matchers,
            )
            .or_else(|| {
                system_hook_message_from_line(
                    record,
                    &session_id,
                    path,
                    line.offset,
                    session_cwd.as_deref(),
                )
            });
            if message.is_none() {
                message = structured_marker_from_line(
                    record,
                    &session_id,
                    path,
                    line.offset,
                    &mut accumulator,
                );
            }
            // Additive reasoning row for assistant thinking blocks. Emitted
            // before the message row so the thinking precedes the visible answer
            // in ordinal+insertion order (both share this line's byte offset).
            if let Some(reasoning) = reasoning_from_line(record, &session_id, path, line.offset) {
                messages.push(reasoning);
            }
            if let Some(message) = message {
                messages.push(message);
            }
        }
        // No early return when `messages` is empty: this source scans every
        // ~/.claude/projects slug and relies on the per-row cwd filter above,
        // so transcripts belonging to other projects legitimately parse to
        // zero messages. Returning the (empty) transcript lets `ingest_one`
        // persist the advanced cursor; returning `None` would pin the cursor
        // at 0 and re-read + re-filter the whole file on every sweep.

        let project = self.user_scope.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id,
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(
                session_cwd.as_deref(),
                subagent.as_ref(),
                &accumulator,
                &self.project_matchers,
            ))
            .ok(),
            parent_session_id: subagent.as_ref().map(|info| info.parent_session_id.clone()),
            is_subagent: subagent.is_some(),
            agent_id: subagent.as_ref().map(|info| info.agent_id.clone()),
            // `parent_tool_use_id` comes from the sibling agent-<id>.meta.json
            // (the tool_use that spawned this subagent); absent for standalone
            // sessions and subagents whose meta file is missing.
            parent_tool_use_id: subagent
                .as_ref()
                .and_then(|info| info.parent_tool_use_id.clone()),
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

/// Identity + spawn provenance for a subagent transcript, assembled from the
/// on-disk layout and the sibling `agent-<id>.meta.json`.
struct ClaudeSubagentInfo {
    session_id: String,
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
    let session_id = path.file_stem()?.to_str()?.to_string();

    // Find the `subagents/` ancestor. `ancestors()` yields `path` first, so the
    // file itself can never match the directory name.
    let subagents_dir = path
        .ancestors()
        .find(|anc| anc.file_name().and_then(|name| name.to_str()) == Some("subagents"))?;
    let parent_session_dir = subagents_dir.parent()?;
    let parent_session_id = parent_session_dir.file_name()?.to_str()?.to_string();

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
    let parent_transcript_path = parent_session_dir.parent().map_or_else(
        || PathBuf::from(format!("{parent_session_id}.jsonl")),
        |grandparent| grandparent.join(format!("{parent_session_id}.jsonl")),
    );

    let meta = read_subagent_meta(path, &session_id);

    Some(ClaudeSubagentInfo {
        session_id,
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
fn read_subagent_meta(transcript_path: &Path, session_id: &str) -> ClaudeSubagentMeta {
    let meta_path = transcript_path.with_file_name(format!("{session_id}.meta.json"));
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

/// Session-level facts folded across a transcript's new lines.
#[derive(Default)]
struct SessionAccumulator {
    /// Distinct PR links seen (`{pr_number, pr_url, pr_repository}`), deduped by
    /// url+number so an append that re-reads a boundary line stays idempotent.
    pr_links: Vec<Value>,
    /// Distinct files edited (`{path, change_type, hunks}`), deduped by path.
    edited_files: Vec<Value>,
}

impl SessionAccumulator {
    fn push_pr_link(&mut self, link: Value) {
        let key = (
            link.get("pr_url")
                .and_then(Value::as_str)
                .map(str::to_string),
            link.get("pr_number").cloned(),
        );
        let exists = self.pr_links.iter().any(|existing| {
            (
                existing
                    .get("pr_url")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                existing.get("pr_number").cloned(),
            ) == key
        });
        if !exists {
            self.pr_links.push(link);
        }
    }

    fn push_edited_file(&mut self, path: &str, change_type: &str, hunks: usize) {
        if self
            .edited_files
            .iter()
            .any(|existing| existing.get("path").and_then(Value::as_str) == Some(path))
        {
            return;
        }
        let mut entry = Map::new();
        entry.insert("path".to_string(), Value::String(path.to_string()));
        entry.insert(
            "change_type".to_string(),
            Value::String(change_type.to_string()),
        );
        entry.insert("hunks".to_string(), Value::from(hunks as i64));
        self.edited_files.push(Value::Object(entry));
    }
}

/// Reads the session `cwd` from an early line of a Claude transcript.
pub(crate) fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(CWD_PROBE_LINES).map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    return Some(PathBuf::from(cwd));
                }
            }
        }
    }
    None
}

/// Map one Claude transcript line to a provider-neutral message, or `None` for
/// lines that carry no conversational text (tool-result-only, meta lines, …).
///
/// Gate: only `user`/`assistant` records become conversational rows here. Other
/// record types fall through to [`system_hook_message_from_line`] and
/// [`structured_marker_from_line`]. Two record families are deliberately dropped
/// with no row at all, because they are pure bloat/redundancy:
///
/// * **hook attachments** — records that inject a hook's `hookAdditionalContext`
///   / attachment payload into the transcript. The signal we care about (hook
///   errors / prevented continuation) is already captured as a compact
///   `hook_event` row; the attachment body just duplicates content that lives on
///   the owning turn.
/// * **queue-operation records** — queued/removed user-turn bookkeeping. These
///   are ephemeral UI state; the actual user turn is ingested when it is sent.
fn message_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    session_cwd: Option<&Path>,
    accumulator: &mut SessionAccumulator,
    location_cache: &ProjectRootMatcherCache,
) -> Option<SessionMessageRecord> {
    let kind = record.get("type").and_then(Value::as_str)?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let message = record.get("message").unwrap_or(record);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_string();

    let content = message.get("content").unwrap_or(message);
    let indexed_content = if role == "assistant" {
        content.as_array().map(|blocks| {
            Value::Array(
                blocks
                    .iter()
                    .filter(|block| {
                        !matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .cloned()
                    .collect(),
            )
        })
    } else {
        None
    };
    let content_for_index = indexed_content.as_ref().unwrap_or(content);
    let (text, tool_names) = content_storage_text_and_tools(
        content_for_index,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }

    let message_id = conversational_message_id(message, record, session_id, offset);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        role,
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(
            kind,
            record,
            message,
            content,
            session_cwd,
            accumulator,
            location_cache,
        ))
        .ok(),
    })
}

/// Stable id for a conversational (`user`/`assistant`) row: the message `id`,
/// else the record `uuid`, else a synthesized `{session}:{offset}`. Shared by
/// the message row and the reasoning row so a reasoning row's
/// `{base}:thinking` id always links back to its owning assistant message.
fn conversational_message_id(
    message: &Value,
    record: &Value,
    session_id: &str,
    offset: i64,
) -> String {
    message
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| record.get("uuid").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), ToString::to_string)
}

/// Emit a separate `kind="reasoning"` row for an assistant message that carries
/// one or more `thinking` blocks, so the model's reasoning is kind-filterable
/// and searchable on its own row — matching how Codex
/// ([`crate::runtime::codex`]) and Cursor ([`crate::runtime::cursor_composer`])
/// store reasoning as a dedicated row (role "assistant", `kind="reasoning"`)
/// rather than leaving the thinking text embedded in the serialized
/// assistant-message content blob.
///
/// Multiple `thinking` blocks are concatenated in transcript order. A
/// `redacted_thinking` block carries no plaintext, so — mirroring Codex's
/// encrypted-reasoning convention, where
/// `response_item_reasoning_summary_text` declines to emit a row when there is
/// no plaintext summary — it never fabricates a body: a message whose only
/// reasoning is redacted yields no row (the block count is recorded as metadata
/// only when a plaintext row already exists).
///
/// Purely additive: the assistant message row itself is untouched (its content
/// blob still carries the thinking blocks verbatim in lossless storage).
fn reasoning_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = record.get("message").unwrap_or(record);
    let blocks = message.get("content").and_then(Value::as_array)?;

    let mut thinking_parts = Vec::new();
    let mut redacted_blocks = 0usize;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                if let Some(text) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    thinking_parts.push(text.to_string());
                }
            }
            Some("redacted_thinking") => redacted_blocks += 1,
            _ => {}
        }
    }
    // No plaintext thinking: mirror Codex, which records nothing for encrypted
    // reasoning rather than fabricating a body from redacted content.
    if thinking_parts.is_empty() {
        return None;
    }
    let text = thinking_parts.join("\n\n");

    let base_id = conversational_message_id(message, record, session_id, offset);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
        .unwrap_or("assistant")
        .to_string();
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_thinking".to_string()),
    );
    // Parent linkage back to the assistant message row that owns this reasoning.
    metadata.insert(
        "parent_message_id".to_string(),
        Value::String(base_id.clone()),
    );
    metadata.insert(
        "thinking_blocks".to_string(),
        Value::from(thinking_parts.len() as i64),
    );
    if redacted_blocks > 0 {
        metadata.insert(
            "redacted_thinking_blocks".to_string(),
            Value::from(redacted_blocks as i64),
        );
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        // `{base}:thinking` keeps re-ingest idempotent and can never collide
        // with the owning message row's `{base}` id under the
        // `(provider, message_id)` primary key.
        message_id: format!("{base_id}:thinking"),
        session_id: session_id.to_string(),
        role,
        timestamp: record_timestamp(record),
        ordinal: offset,
        text,
        kind: Some(KIND_REASONING.to_string()),
        model,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Map a `type=="system"` hook-summary record to a compact, signal-only
/// `hook_event` row, or `None` for non-system records and routine hook
/// summaries that carry no error/interruption signal.
fn system_hook_message_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    _session_cwd: Option<&Path>,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("system") {
        return None;
    }

    let hook_errors: Vec<&Value> = record
        .get("hookErrors")
        .and_then(Value::as_array)
        .map(|errors| errors.iter().collect())
        .unwrap_or_default();
    let stop_reason = record
        .get("stopReason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty());
    let prevented_continuation = record
        .get("preventedContinuation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if hook_errors.is_empty() && stop_reason.is_none() && !prevented_continuation {
        return None;
    }

    let subtype = record.get("subtype").and_then(Value::as_str).unwrap_or("");
    let tool_use_id = record.get("toolUseID").and_then(Value::as_str);

    let mut lines = vec![format!("Claude hook event: {subtype}")];
    if let Some(tool_use_id) = tool_use_id {
        lines.push(format!("tool_use_id: {tool_use_id}"));
    }
    if let Some(stop_reason) = stop_reason {
        lines.push(format!("stop_reason: {stop_reason}"));
    }
    if prevented_continuation {
        lines.push("prevented_continuation: true".to_string());
    }
    if !hook_errors.is_empty() {
        let joined = hook_errors
            .iter()
            .map(|error| {
                error
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_string)
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!("hook_errors: {joined}"));
    }
    let joined = lines.join("\n");
    let text = preview_truncated(&joined, MARKER_PREVIEW_BYTES);

    let message_id = record
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), ToString::to_string);
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_system_record".to_string()),
    );
    metadata.insert("subtype".to_string(), Value::String(subtype.to_string()));
    if let Some(tool_use_id) = tool_use_id {
        metadata.insert(
            "tool_use_id".to_string(),
            Value::String(tool_use_id.to_string()),
        );
    }
    if let Some(hook_count) = record.get("hookCount") {
        metadata.insert("hook_count".to_string(), hook_count.clone());
    }
    if let Some(level) = record.get("level").and_then(Value::as_str) {
        metadata.insert("level".to_string(), Value::String(level.to_string()));
    }
    if prevented_continuation {
        metadata.insert("prevented_continuation".to_string(), Value::Bool(true));
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        // role "tool" keeps transient hook telemetry out of LCM policy anchors, which pin role system/developer.
        role: "tool".to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("hook_event".to_string()),
        model: None,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Map a structured, non-conversational Claude record to a marker row:
/// `pr-link` records, `system` compaction boundaries, and model-fallback
/// records. Returns `None` for every other record type (leaving the cursor to
/// advance without emitting a row).
fn structured_marker_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    accumulator: &mut SessionAccumulator,
) -> Option<SessionMessageRecord> {
    match record.get("type").and_then(Value::as_str)? {
        "pr-link" => pr_link_row(record, session_id, path, offset, accumulator),
        "system" => compact_boundary_row(record, session_id, path, offset)
            .or_else(|| model_fallback_row(record, session_id, path, offset)),
        _ => None,
    }
}

/// Common ISO-8601 timestamp read for a top-level record.
fn record_timestamp(record: &Value) -> Option<i64> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64)
}

/// Build a marker row for a `type=="pr-link"` record and fold the PR into the
/// session accumulator. Emits both so the git-correlation join has a per-turn
/// anchor (`message_search`) *and* a session-level `pr_links[]` summary.
fn pr_link_row(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    accumulator: &mut SessionAccumulator,
) -> Option<SessionMessageRecord> {
    let pr_number = record.get("prNumber").filter(|value| !value.is_null());
    let pr_url = record
        .get("prUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty());
    let pr_repository = record
        .get("prRepository")
        .and_then(Value::as_str)
        .filter(|repo| !repo.is_empty());
    // A pr-link with no identifying fields is noise; drop it.
    if pr_number.is_none() && pr_url.is_none() && pr_repository.is_none() {
        return None;
    }

    let number_display = pr_number.map(render_scalar).unwrap_or_default();
    let mut text = String::from("Claude PR link:");
    if let Some(repo) = pr_repository {
        text.push(' ');
        text.push_str(repo);
    }
    if !number_display.is_empty() {
        text.push_str(" #");
        text.push_str(&number_display);
    }
    if let Some(url) = pr_url {
        text.push(' ');
        text.push_str(url);
    }

    let mut link = Map::new();
    if let Some(number) = pr_number {
        link.insert("pr_number".to_string(), number.clone());
    }
    if let Some(url) = pr_url {
        link.insert("pr_url".to_string(), Value::String(url.to_string()));
    }
    if let Some(repo) = pr_repository {
        link.insert("pr_repository".to_string(), Value::String(repo.to_string()));
    }
    accumulator.push_pr_link(Value::Object(link.clone()));

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_pr_link".to_string()),
    );
    for (key, value) in &link {
        metadata.insert(key.clone(), value.clone());
    }

    let message_id = marker_message_id(record, session_id, KIND_PR_LINK, offset);
    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        // Telemetry, not conversation: role "tool" keeps it out of LCM anchors.
        role: "tool".to_string(),
        timestamp: record_timestamp(record),
        ordinal: offset,
        text: preview_truncated(&text, MARKER_PREVIEW_BYTES),
        kind: Some(KIND_PR_LINK.to_string()),
        model: None,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Build a `compact_boundary` marker row from a `system` record that carries
/// `compactMetadata` (a context-compaction boundary). LCM uses this to tell a
/// post-compaction summary apart from an original turn.
fn compact_boundary_row(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let subtype = record.get("subtype").and_then(Value::as_str);
    let compact_metadata = record
        .get("compactMetadata")
        .filter(|value| value.is_object());
    if subtype != Some("compact_boundary") && compact_metadata.is_none() {
        return None;
    }

    let trigger = compact_metadata
        .and_then(|meta| meta.get("trigger"))
        .and_then(Value::as_str)
        .or_else(|| record.get("trigger").and_then(Value::as_str));
    let pre_tokens = compact_metadata
        .and_then(|meta| meta.get("preTokens"))
        .and_then(Value::as_i64)
        .or_else(|| record.get("preTokens").and_then(Value::as_i64));
    let logical_parent_uuid = record
        .get("logicalParentUuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty());

    let mut text = String::from("Claude compaction boundary");
    if let Some(trigger) = trigger {
        text.push_str(&format!(" (trigger: {trigger})"));
    }
    if let Some(pre_tokens) = pre_tokens {
        text.push_str(&format!(", pre_tokens: {pre_tokens}"));
    }

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_compact_boundary".to_string()),
    );
    if let Some(trigger) = trigger {
        metadata.insert("trigger".to_string(), Value::String(trigger.to_string()));
    }
    if let Some(pre_tokens) = pre_tokens {
        metadata.insert("pre_tokens".to_string(), Value::from(pre_tokens));
    }
    if let Some(logical_parent_uuid) = logical_parent_uuid {
        metadata.insert(
            "logical_parent_uuid".to_string(),
            Value::String(logical_parent_uuid.to_string()),
        );
    }

    let message_id = marker_message_id(record, session_id, KIND_COMPACT_BOUNDARY, offset);
    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        // A compaction boundary is a genuine structural event LCM anchors on.
        role: "system".to_string(),
        timestamp: record_timestamp(record),
        ordinal: offset,
        text: preview_truncated(&text, MARKER_PREVIEW_BYTES),
        kind: Some(KIND_COMPACT_BOUNDARY.to_string()),
        model: None,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Build a `model_fallback` marker row from a `system` model-refusal-fallback
/// record (Claude routed a refused request to a fallback model).
fn model_fallback_row(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let subtype = record.get("subtype").and_then(Value::as_str);
    let original_model = record
        .get("originalModel")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty());
    let fallback_model = record
        .get("fallbackModel")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty());
    if subtype != Some("model_refusal_fallback")
        && original_model.is_none()
        && fallback_model.is_none()
    {
        return None;
    }

    let trigger = record.get("trigger").and_then(Value::as_str);
    let refusal_category = record
        .get("apiRefusalCategory")
        .and_then(Value::as_str)
        .filter(|category| !category.is_empty());

    let mut text = String::from("Claude model fallback");
    if let (Some(original), Some(fallback)) = (original_model, fallback_model) {
        text.push_str(&format!(": {original} -> {fallback}"));
    } else if let Some(fallback) = fallback_model {
        text.push_str(&format!(" -> {fallback}"));
    }
    if let Some(category) = refusal_category {
        text.push_str(&format!(" ({category})"));
    }

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_model_fallback".to_string()),
    );
    if let Some(original) = original_model {
        metadata.insert(
            "original_model".to_string(),
            Value::String(original.to_string()),
        );
    }
    if let Some(fallback) = fallback_model {
        metadata.insert(
            "fallback_model".to_string(),
            Value::String(fallback.to_string()),
        );
    }
    if let Some(trigger) = trigger {
        metadata.insert("trigger".to_string(), Value::String(trigger.to_string()));
    }
    if let Some(category) = refusal_category {
        metadata.insert(
            "api_refusal_category".to_string(),
            Value::String(category.to_string()),
        );
    }

    let message_id = marker_message_id(record, session_id, KIND_MODEL_FALLBACK, offset);
    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        role: "tool".to_string(),
        timestamp: record_timestamp(record),
        ordinal: offset,
        text: preview_truncated(&text, MARKER_PREVIEW_BYTES),
        kind: Some(KIND_MODEL_FALLBACK.to_string()),
        model: fallback_model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Stable, unique message id for a marker row: prefer the record `uuid`, else
/// synthesize one keyed by kind+offset so it stays stable across re-ingest and
/// never collides with a conversational row's `{session}:{offset}` id.
fn marker_message_id(record: &Value, session_id: &str, kind: &str, offset: i64) -> String {
    record
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty())
        .map_or_else(
            || format!("{session_id}:{kind}:{offset}"),
            |uuid| format!("{kind}:{uuid}"),
        )
}

/// Render a JSON scalar (number/string/bool) as plain text for a marker preview.
fn render_scalar(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn session_metadata(
    session_cwd: Option<&Path>,
    subagent: Option<&ClaudeSubagentInfo>,
    accumulator: &SessionAccumulator,
    location_cache: &ProjectRootMatcherCache,
) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    append_location_metadata_cached(
        &mut metadata,
        CLAUDE_SESSION_LOCATION_KEYS,
        TranscriptLocation::new(session_cwd, "transcript_session"),
        location_cache,
    );

    // Subagent spawn provenance (from the sibling agent-<id>.meta.json and the
    // on-disk layout). `parent_tool_use_id` rides the dedicated session column;
    // these richer facts have no column, so they land in metadata.
    if let Some(subagent) = subagent {
        if let Some(agent_type) = &subagent.agent_type {
            metadata.insert("agent_type".to_string(), Value::String(agent_type.clone()));
        }
        if let Some(description) = &subagent.description {
            metadata.insert(
                "agent_description".to_string(),
                Value::String(description.clone()),
            );
        }
        if let Some(spawn_depth) = subagent.spawn_depth {
            metadata.insert("spawn_depth".to_string(), Value::from(spawn_depth));
        }
        if let Some(workflow_run_id) = &subagent.workflow_run_id {
            metadata.insert(
                "workflow_run_id".to_string(),
                Value::String(workflow_run_id.clone()),
            );
        }
    }

    // Session-level rollups: only emitted when the session actually produced
    // them, so plain sessions keep byte-for-byte identical metadata.
    if !accumulator.pr_links.is_empty() {
        metadata.insert(
            "pr_links".to_string(),
            Value::Array(accumulator.pr_links.clone()),
        );
    }
    if !accumulator.edited_files.is_empty() {
        metadata.insert(
            "edited_files".to_string(),
            Value::Array(accumulator.edited_files.clone()),
        );
    }

    Value::Object(metadata)
}

fn message_metadata(
    kind: &str,
    record: &Value,
    message: &Value,
    content: &Value,
    session_cwd: Option<&Path>,
    accumulator: &mut SessionAccumulator,
    location_cache: &ProjectRootMatcherCache,
) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    metadata.insert("raw_type".to_string(), Value::String(kind.to_string()));
    let record_cwd = record_cwd(record);
    let (location_cwd, location_provenance) = if record_cwd.is_some() {
        (record_cwd.as_deref(), "transcript_record")
    } else {
        (session_cwd, "transcript_session")
    };
    append_location_metadata_cached(
        &mut metadata,
        CLAUDE_MESSAGE_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, location_provenance),
        location_cache,
    );
    if let Some(branch) = record
        .get("gitBranch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        metadata.insert("git_branch".to_string(), Value::String(branch.to_string()));
    }
    append_tool_calls_metadata(&mut metadata, message);
    append_tool_event_metadata(&mut metadata, content);
    // Anthropic-style per-message counters: `message.usage.{input_tokens,
    // output_tokens, cache_creation_input_tokens, cache_read_input_tokens}`.
    append_usage_metadata(&mut metadata, &[message]);
    // Per-turn adoption ground truth: which MCP server/tool/skill produced this
    // assistant turn. Top-level on the assistant record, copied verbatim.
    if kind == "assistant" {
        append_attribution_metadata(&mut metadata, record);
    }
    // Edit/Write tool results carry a top-level `toolUseResult` with the edited
    // file path + structured patch. Record the file + hunk stats (never the
    // patch bodies) and fold the file into the session summary.
    if kind == "user" {
        append_edited_file_metadata(&mut metadata, record, accumulator);
        append_git_operation_metadata(&mut metadata, record);
    }
    Value::Object(metadata)
}

/// Preserve Claude's structured git-operation event as direct commit evidence.
/// The abbreviated id is resolved against the repository before persistence;
/// raw stdout/stderr stays in the lossless transcript rather than metadata.
fn append_git_operation_metadata(metadata: &mut Map<String, Value>, record: &Value) {
    let Some(commit) = record
        .pointer("/toolUseResult/gitOperation/commit")
        .and_then(Value::as_object)
    else {
        return;
    };
    let Some(sha) = commit.get("sha").and_then(Value::as_str).filter(|sha| {
        (7..=64).contains(&sha.len()) && sha.chars().all(|ch| ch.is_ascii_hexdigit())
    }) else {
        return;
    };
    metadata.insert(
        "produced_commit_candidates".to_string(),
        Value::Array(vec![Value::String(sha.to_ascii_lowercase())]),
    );
    metadata.insert(
        "produced_commit_evidence".to_string(),
        Value::String("host_event".to_string()),
    );
    if let Some(kind) = commit
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
    {
        metadata.insert(
            "produced_commit_kind".to_string(),
            Value::String(kind.to_string()),
        );
    }
    if let Some(branch) = record
        .get("gitBranch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        metadata.insert("git_branch".to_string(), Value::String(branch.to_string()));
    }
}

/// Copy Claude's top-level attribution fields onto an assistant row's metadata.
fn append_attribution_metadata(metadata: &mut Map<String, Value>, record: &Value) {
    for (source_key, dest_key) in [
        ("attributionMcpServer", "attribution_mcp_server"),
        ("attributionMcpTool", "attribution_mcp_tool"),
        ("attributionSkill", "attribution_skill"),
        ("promptSource", "prompt_source"),
    ] {
        if let Some(value) = record
            .get(source_key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            metadata.insert(dest_key.to_string(), Value::String(value.to_string()));
        }
    }
    // `origin` only when it is a cheap scalar string; skip nested objects.
    if let Some(origin) = record
        .get("origin")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        metadata.insert("origin".to_string(), Value::String(origin.to_string()));
    }
}

/// Record edited-file facts from a user `tool_result` record's top-level
/// `toolUseResult` (Edit/Write payloads), and fold the file into the session
/// accumulator. Stores only the path, change type, and hunk count — never the
/// patch bodies.
fn append_edited_file_metadata(
    metadata: &mut Map<String, Value>,
    record: &Value,
    accumulator: &mut SessionAccumulator,
) {
    let Some(tool_use_result) = record
        .get("toolUseResult")
        .filter(|value| value.is_object())
    else {
        return;
    };
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return;
    };
    // Write results carry an explicit `type` ("create"/"update"); Edit results
    // do not, so an absent type means an in-place edit.
    let change_type = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("edit")
        .to_string();
    let hunks = tool_use_result
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    let mut edited = Map::new();
    edited.insert("path".to_string(), Value::String(file_path.to_string()));
    edited.insert(
        "change_type".to_string(),
        Value::String(change_type.clone()),
    );
    edited.insert("hunks".to_string(), Value::from(hunks as i64));
    metadata.insert("edited_file".to_string(), Value::Object(edited));

    accumulator.push_edited_file(file_path, &change_type, hunks);
}

fn record_cwd(record: &Value) -> Option<PathBuf> {
    record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use serde_json::json;

    static UNKNOWN_PATH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    fn retrying_identity(path: &Path) -> crate::worktree::GitRepoIdentityOutcome {
        let root = path
            .ancestors()
            .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "repo"))
            .unwrap_or(path);
        if UNKNOWN_PATH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 1 {
            return crate::worktree::GitRepoIdentityOutcome::Unknown;
        }
        crate::worktree::GitRepoIdentityOutcome::Resolved(crate::worktree::GitRepoIdentity {
            worktree_root: root.to_path_buf(),
            common_dir: root.join(".git"),
        })
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
        let message = message_from_line(
            &record,
            "sess",
            path,
            10,
            None,
            &mut accumulator,
            &ProjectRootMatcherCache::default(),
        )
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
    fn claude_message_metadata_reuses_worktree_for_repeated_cwd() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project_root)
            .status()
            .expect("git init");
        assert!(status.success());

        let record = assistant_record(&json!([{"type": "text", "text": "Repeated cwd metadata."}]));
        let path = temp.path().join("session.jsonl");
        let cache = ProjectRootMatcherCache::default();
        let mut accumulator = SessionAccumulator::default();
        let first = message_from_line(
            &record,
            "sess",
            &path,
            10,
            Some(&nested_cwd),
            &mut accumulator,
            &cache,
        )
        .expect("first message");
        let first_metadata: Value =
            serde_json::from_str(first.metadata_json.as_deref().unwrap()).unwrap();
        let first_worktree = first_metadata["claude_message_worktree"].clone();
        assert!(first_worktree.is_string());

        std::fs::rename(project_root.join(".git"), project_root.join(".git.hidden"))
            .expect("hide git metadata after first lookup");

        let second = message_from_line(
            &record,
            "sess",
            &path,
            20,
            Some(&nested_cwd),
            &mut accumulator,
            &cache,
        )
        .expect("second message");
        let second_metadata: Value =
            serde_json::from_str(second.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(second_metadata["claude_message_worktree"], first_worktree);
    }

    #[test]
    fn claude_unknown_membership_retries_without_advancing_cursor() {
        UNKNOWN_PATH_ATTEMPTS.store(0, Ordering::SeqCst);
        let temp = tempfile::TempDir::new().expect("temp dir");
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
        let transcript = temp.path().join("retry.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "sessionId": "retry",
                    "cwd": nested_cwd,
                    "message": {"role": "user", "content": "retry me"}
                })
            ),
        )
        .expect("write transcript");
        let mut source = ClaudeSource::with_home(temp.path());
        source.project_matchers =
            ProjectRootMatcherCache::with_identity_resolver(retrying_identity);

        let previous = StoredCursor::default();
        assert!(
            source
                .parse_new(&transcript, previous, &project_root, None)
                .is_none(),
            "unknown membership must abort before a new cursor can be persisted"
        );

        let retried = source
            .parse_new(&transcript, previous, &project_root, None)
            .expect("unknown membership must be resolved again on retry");
        assert_eq!(retried.messages.len(), 1);
        assert!(retried.new_cursor.position > previous.position);
        assert_eq!(UNKNOWN_PATH_ATTEMPTS.load(Ordering::SeqCst), 3);
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
