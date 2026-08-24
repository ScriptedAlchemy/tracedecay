use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::host_ports::parse_timestamp;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectRootMatcherCache, TranscriptLocation, TranscriptLocationMetadataKeys,
    append_location_metadata, append_location_metadata_cached, append_tool_calls_metadata,
    append_tool_event_metadata, append_usage_metadata, preview_truncated,
};

use super::source_records::record_cwd;
use super::{
    CLAUDE_MESSAGE_LOCATION_KEYS, CLAUDE_SESSION_LOCATION_KEYS, ClaudeSubagentInfo,
    KIND_COMPACT_BOUNDARY, KIND_MODEL_FALLBACK, KIND_PR_LINK, MARKER_PREVIEW_BYTES, PROVIDER,
};

#[derive(Default)]
pub(super) struct SessionAccumulator {
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

pub(super) fn accumulate_session_facts(record: &Value, accumulator: &mut SessionAccumulator) {
    append_edited_file_metadata(&mut Map::new(), record, accumulator);
    if let Ok(envelope) =
        serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(record.clone())
    {
        for fact in envelope.facts() {
            match fact {
                tracedecay_domain::CanonicalObservationFactV1::Git {
                    evidence_kind: tracedecay_domain::CanonicalGitEvidenceKindV1::FileEdit,
                    content: Some(native),
                    ..
                } => append_edited_file_metadata(&mut Map::new(), native, accumulator),
                tracedecay_domain::CanonicalObservationFactV1::Git {
                    evidence_kind: tracedecay_domain::CanonicalGitEvidenceKindV1::PullRequest,
                    content: Some(native),
                    ..
                } => {
                    let mut link = Map::new();
                    if let Some(number) = native.get("prNumber").filter(|value| !value.is_null()) {
                        link.insert("pr_number".to_string(), number.clone());
                    }
                    if let Some(url) = native
                        .get("prUrl")
                        .and_then(Value::as_str)
                        .filter(|url| !url.is_empty())
                    {
                        link.insert("pr_url".to_string(), Value::String(url.to_string()));
                    }
                    if let Some(repo) = native
                        .get("prRepository")
                        .and_then(Value::as_str)
                        .filter(|repo| !repo.is_empty())
                    {
                        link.insert("pr_repository".to_string(), Value::String(repo.to_string()));
                    }
                    if !link.is_empty() {
                        accumulator.push_pr_link(Value::Object(link));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Read a record's optional wall-clock timestamp.
pub(super) fn record_timestamp(record: &Value) -> Option<i64> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64)
}

/// Build a marker row for a `type=="pr-link"` record and fold the PR into the
/// session accumulator. Emits both so the git-correlation join has a per-turn
/// anchor (`message_search`) *and* a session-level `pr_links[]` summary.
pub(super) fn pr_link_row(
    record: &Value,
    session_id: &str,
    file_generation: u64,
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

    let message_id = marker_message_id(record, session_id, file_generation, KIND_PR_LINK, offset);
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
pub(super) fn compact_boundary_row(
    record: &Value,
    session_id: &str,
    file_generation: u64,
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
        let _ = write!(text, " (trigger: {trigger})");
    }
    if let Some(pre_tokens) = pre_tokens {
        let _ = write!(text, ", pre_tokens: {pre_tokens}");
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

    let message_id = marker_message_id(
        record,
        session_id,
        file_generation,
        KIND_COMPACT_BOUNDARY,
        offset,
    );
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
pub(super) fn model_fallback_row(
    record: &Value,
    session_id: &str,
    file_generation: u64,
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
        let _ = write!(text, ": {original} -> {fallback}");
    } else if let Some(fallback) = fallback_model {
        let _ = write!(text, " -> {fallback}");
    }
    if let Some(category) = refusal_category {
        let _ = write!(text, " ({category})");
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

    let message_id = marker_message_id(
        record,
        session_id,
        file_generation,
        KIND_MODEL_FALLBACK,
        offset,
    );
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

/// Stable marker identity: prefer a usable record `uuid`, otherwise use the
/// source generation and offset shared by every sanitized Claude row.
fn marker_message_id(
    record: &Value,
    session_id: &str,
    file_generation: u64,
    kind: &str,
    offset: i64,
) -> String {
    record
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty() && !is_redaction_marker(uuid))
        .map_or_else(
            || source_position_message_id(session_id, file_generation, offset),
            |uuid| format!("{kind}:{uuid}"),
        )
}

pub(super) fn source_position_message_id(
    session_id: &str,
    file_generation: u64,
    offset: i64,
) -> String {
    format!("{session_id}:{file_generation}:{offset}")
}

pub(super) fn is_redaction_marker(value: &str) -> bool {
    value.starts_with("[TraceDecay redacted:")
}

/// Render a JSON scalar (number/string/bool) as plain text for a marker preview.
fn render_scalar(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

/// Dispatch location metadata through the source-lifetime worktree cache when
/// the caller has one (batch transcript parses), falling back to the uncached
/// per-call resolution for single-record paths (observation projection).
pub(super) fn append_claude_location_metadata(
    map: &mut Map<String, Value>,
    keys: TranscriptLocationMetadataKeys,
    location: TranscriptLocation<'_>,
    worktree_cache: Option<&ProjectRootMatcherCache>,
) {
    match worktree_cache {
        Some(cache) => append_location_metadata_cached(map, keys, location, cache),
        None => append_location_metadata(map, keys, location),
    }
}

pub(super) fn session_metadata(
    sanitized_session_cwd: Option<&Path>,
    subagent: Option<&ClaudeSubagentInfo>,
    accumulator: &SessionAccumulator,
    worktree_cache: Option<&ProjectRootMatcherCache>,
) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    append_claude_location_metadata(
        &mut metadata,
        CLAUDE_SESSION_LOCATION_KEYS,
        TranscriptLocation::new(sanitized_session_cwd, "transcript_session"),
        worktree_cache,
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

pub(super) fn message_metadata(
    kind: &str,
    record: &Value,
    message: &Value,
    content: &Value,
    sanitized_session_cwd: Option<&Path>,
    accumulator: &mut SessionAccumulator,
    worktree_cache: Option<&ProjectRootMatcherCache>,
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
        (sanitized_session_cwd, "transcript_session")
    };
    append_claude_location_metadata(
        &mut metadata,
        CLAUDE_MESSAGE_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, location_provenance),
        worktree_cache,
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
    // assistant turn. The caller supplies the sanitizer-issued record, so these
    // top-level fields have already crossed the mandatory privacy boundary.
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
pub(super) fn append_git_operation_metadata(metadata: &mut Map<String, Value>, record: &Value) {
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
