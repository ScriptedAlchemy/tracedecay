//! Cline/Roo Code/Kilo Code task-history transcript sources.
//!
//! These VS Code extension-family adapters persist each task in a directory with
//! JSON files such as:
//!
//! * `api_conversation_history.json` (or Roo's `api_messages.json`) - the
//!   Anthropic-compatible conversation sent to/received from the model.
//! * `ui_messages.json` - webview-oriented messages; `say`/`api_req_started`
//!   events carry token counters in the `text` JSON payload.
//! * `task_metadata.json` / `history_item.json` - task metadata.
//!
//! The API conversation file is a **full-rewrite** JSON array, so the source uses
//! the shared `ContentHash` reader and stable native-or-content-derived message
//! identities. To avoid mixing global VS Code extension history across projects, a task
//! is ingested only when its metadata contains a project/workspace/cwd path that
//! resolves to the current tracedecay project root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::admission::HostAdmission;
#[cfg(test)]
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    path_belongs_to_project, title_from_messages,
};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_METADATA_BYTES, SnapshotAdmissionRecord,
    SnapshotCaptureOutcome, StableMessageIdDomains, bounded_snapshot_input_len,
    capture_snapshot_observations, non_durable_snapshot_record, read_snapshot_text_bounded,
    snapshot_message_fields, stable_snapshot_message_id,
};
#[cfg(test)]
use crate::runtime::snapshot_observation::{
    canonical_snapshot_envelope, host_admission_error, snapshot_cursor_after,
};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, read_changed_with_companion,
};
use serde_json::{Map, Value};
#[cfg(test)]
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationSourceCursorV1, ObservationSourceRangeV1,
};
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1};
#[cfg(test)]
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;

/// Cap task-directory scans so a long VS Code globalStorage history cannot
/// block dashboard startup.
const MAX_TASK_DIRS_PER_ROOT: usize = 512;
const MAX_TASKS_PER_PASS: usize = 512;
const MAX_MESSAGES_PER_TASK: usize = 4_096;
const MAX_USAGE_EVENTS_PER_TASK: usize = 4_096;
const TASK_METADATA_FILES: [&str; 3] = ["task_metadata.json", "history_item.json", "history.json"];
const DELIMITED_NATIVE_MESSAGE_ID_DOMAIN: &[u8] =
    b"tracedecay.cline-like-delimited-native-message.v2";
const DERIVED_MESSAGE_ID_DOMAIN: &[u8] = b"tracedecay.cline-like-derived-message.v3";
const CLINE_LIKE_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "cline_like_task_cwd",
        "cline_like_task_worktree",
        "cline_like_task_location_provenance",
    );

/// One Cline-family provider configuration.
#[derive(Clone)]
pub struct ClineLikeSource {
    provider: &'static str,
    storage_roots: Vec<PathBuf>,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClineLikeSnapshotObservationRecord {
    provider: &'static str,
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

impl SnapshotAdmissionRecord for ClineLikeSnapshotObservationRecord {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    fn order(&self) -> u64 {
        self.order
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[cfg(test)]
impl ClineLikeSnapshotObservationRecord {
    fn cursor_after(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        snapshot_cursor_after(
            self.provider,
            &self.session_id,
            self.order,
            scope,
            generation,
        )
    }
}

impl ClineLikeSource {
    /// Cline VS Code extension storage:
    /// `Code/User/globalStorage/saoudrizwan.claude-dev/tasks`.
    pub fn cline() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::cline_with_home(&home))
    }

    /// Roo Code VS Code extension storage:
    /// `Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks`.
    pub fn roo_code() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::roo_code_with_home(&home))
    }

    /// Kilo Code storage. Current docs mention both the VS Code extension root
    /// and the CLI root (`~/.kilocode/cli/global/tasks`), so scan both.
    pub fn kilo() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::kilo_with_home(&home))
    }

    pub fn cline_with_home(home: &Path) -> Self {
        Self {
            provider: "cline",
            storage_roots: vec![
                crate::host_ports::vscode_data_dir(home)
                    .join("User/globalStorage/saoudrizwan.claude-dev/tasks"),
            ],
            user_registered_roots: None,
        }
    }

    pub fn roo_code_with_home(home: &Path) -> Self {
        Self {
            provider: "roo-code",
            storage_roots: vec![
                crate::host_ports::vscode_data_dir(home)
                    .join("User/globalStorage/rooveterinaryinc.roo-cline/tasks"),
            ],
            user_registered_roots: None,
        }
    }

    pub fn kilo_with_home(home: &Path) -> Self {
        Self {
            provider: "kilo",
            storage_roots: vec![
                crate::host_ports::vscode_data_dir(home)
                    .join("User/globalStorage/kilocode.kilo-code/tasks"),
                home.join(".kilocode/cli/global/tasks"),
            ],
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }
}

impl TranscriptSource for ClineLikeSource {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in &self.storage_roots {
            let remaining = MAX_TASKS_PER_PASS.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            out.extend(
                collect_task_api_paths(root)
                    .into_iter()
                    .filter(|path| self.snapshot_location(path, project_root).is_some())
                    .take(remaining),
            );
        }
        out
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        self.parse_snapshot(path, prev, project_root, max_new_bytes)
            .ok()
            .flatten()
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        self.parse_snapshot(path, prev, project_root, max_new_bytes)
    }
}

impl ClineLikeSource {
    fn snapshot_location(&self, path: &Path, project_root: &Path) -> Option<PathBuf> {
        let metadata = read_task_metadata(self.provider, path.parent()?)?;
        self.snapshot_location_from_metadata(&metadata, project_root)
    }

    fn snapshot_location_from_metadata(
        &self,
        metadata: &Value,
        project_root: &Path,
    ) -> Option<PathBuf> {
        let paths = metadata_project_paths(metadata);
        if let Some(roots) = &self.user_registered_roots {
            if paths
                .iter()
                .any(|path| roots.iter().any(|root| path_belongs_to_project(path, root)))
            {
                return None;
            }
            paths.into_iter().next()
        } else {
            paths
                .into_iter()
                .find(|path| path_belongs_to_project(path, project_root))
        }
    }

    fn parse_snapshot(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        let Some(task_dir) = path.parent() else {
            return Ok(None);
        };
        let ui_path = task_dir.join("ui_messages.json");
        let byte_cap = max_new_bytes
            .unwrap_or(MAX_SNAPSHOT_FILE_BYTES)
            .min(MAX_SNAPSHOT_FILE_BYTES);
        ensure_bounded_file(self.provider, path, byte_cap)?;
        if ui_path.is_file() {
            ensure_bounded_file(self.provider, &ui_path, byte_cap)?;
        }
        let Some(changed) = read_changed_with_companion(path, &ui_path, prev, byte_cap) else {
            return Ok(None);
        };
        let Some(metadata) = read_task_metadata(self.provider, task_dir) else {
            return Ok(None);
        };
        let Some(location_cwd) = self.snapshot_location_from_metadata(&metadata, project_root)
        else {
            return Ok(None);
        };

        let document: Value = match serde_json::from_str(&changed.contents) {
            Ok(document) => document,
            Err(error) if error.is_eof() => return Ok(None),
            Err(_) => return Err(non_durable(self.provider, path, "malformed snapshot JSON")),
        };
        let Some(entries) = document.as_array() else {
            return Err(non_durable(
                self.provider,
                path,
                "unsupported snapshot root",
            ));
        };
        if entries.len() > MAX_MESSAGES_PER_TASK {
            return Err(non_durable(
                self.provider,
                path,
                "snapshot message count exceeds provider bound",
            ));
        }
        let task_id = task_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");

        let mut messages = Vec::with_capacity(entries.len());
        let mut fallback_occurrences = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            if let Some(message) = message_from_entry(
                self.provider,
                entry,
                task_id,
                path,
                index,
                &location_cwd,
                &mut fallback_occurrences,
            ) {
                messages.push(message);
            }
        }
        let Some(usage) = usage_records(
            self.provider,
            task_id,
            &ui_path,
            entries.len(),
            &location_cwd,
        )?
        else {
            return Ok(None);
        };
        messages.extend(usage);

        let project = self.user_registered_roots.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: task_id.to_string(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages)
                .or_else(|| metadata_task_title(&metadata).map(str::to_string)),
            metadata_json: serde_json::to_string(&session_metadata(
                self.provider,
                Some(&location_cwd),
            ))
            .ok(),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };

        Ok(Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: changed.new_cursor,
        }))
    }
}

/// Captures bounded Cline-family snapshots through the daemon-owned observation authority.
///
/// This deliberately re-reads complete snapshots and derives a new source generation
/// from their content hash; it neither consults nor advances legacy parse offsets.
/// `max_new_bytes` is one logical source-byte budget for the complete sweep.
pub async fn capture_cline_like_snapshot_observations(
    facade: &dyn HostAdmission,
    source: &ClineLikeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<SnapshotCaptureOutcome> {
    capture_snapshot_observations(
        facade,
        scope,
        cancellation,
        max_new_bytes,
        source.discover_transcript_paths(
            project_root,
            TranscriptDiscoveryBounds::from_discovered_units(MAX_TASKS_PER_PASS),
        ),
        |path| snapshot_input_bytes(source.provider, path),
        |path| {
            let Some(parsed) =
                source.parse_snapshot(path, StoredCursor::default(), project_root, None)?
            else {
                return Ok(None);
            };
            let generation = ObservationSourceGenerationV1::new(parsed.new_cursor.position.max(1))?;
            let records =
                normalize_cline_like_snapshot_observations(source.provider, &parsed.messages)?;
            Ok(Some((generation, records)))
        },
    )
    .await
}

fn ensure_bounded_file(
    provider: &'static str,
    path: &Path,
    byte_cap: u64,
) -> TranscriptIngestResult<()> {
    bounded_snapshot_input_len(provider, path, byte_cap)
        .map(|_| ())
        .map_err(|_| non_durable(provider, path, "snapshot exceeds provider byte bound"))
}

fn snapshot_input_bytes(provider: &'static str, path: &Path) -> TranscriptIngestResult<u64> {
    let Some(task_dir) = path.parent() else {
        return Ok(0);
    };
    let primary = bounded_snapshot_input_len(provider, path, MAX_SNAPSHOT_FILE_BYTES)?;
    let ui = bounded_snapshot_input_len(
        provider,
        &task_dir.join("ui_messages.json"),
        MAX_SNAPSHOT_FILE_BYTES,
    )?;
    let metadata = TASK_METADATA_FILES.iter().fold(0_u64, |total, name| {
        let bytes = std::fs::metadata(task_dir.join(name))
            .ok()
            .map(|metadata| metadata.len())
            .filter(|bytes| *bytes <= MAX_SNAPSHOT_METADATA_BYTES)
            .unwrap_or(0);
        total.saturating_add(bytes)
    });
    Ok(primary.saturating_add(ui).saturating_add(metadata))
}

fn non_durable(provider: &'static str, path: &Path, reason: &'static str) -> TranscriptIngestError {
    non_durable_snapshot_record(provider, path, reason)
}

fn collect_task_api_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut task_dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .take(MAX_TASK_DIRS_PER_ROOT)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            Some((mtime, path))
        })
        .collect();
    task_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    task_dirs.truncate(MAX_TASK_DIRS_PER_ROOT);

    let mut out = Vec::new();
    for (_, task_dir) in task_dirs {
        for name in ["api_conversation_history.json", "api_messages.json"] {
            let path = task_dir.join(name);
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

fn read_task_metadata(provider: &'static str, task_dir: &Path) -> Option<Value> {
    for name in TASK_METADATA_FILES {
        let path = task_dir.join(name);
        if let Ok(Some(contents)) =
            read_snapshot_text_bounded(provider, &path, MAX_SNAPSHOT_METADATA_BYTES)
            && let Ok(value) = serde_json::from_str::<Value>(&contents)
        {
            return Some(value);
        }
    }
    None
}

fn metadata_project_paths(value: &Value) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_metadata_project_paths(value, None, &mut out);
    out
}

fn collect_metadata_project_paths(value: &Value, key: Option<&str>, out: &mut Vec<PathBuf>) {
    match value {
        Value::Object(map) => {
            for (child_key, child_value) in map {
                collect_metadata_project_paths(child_value, Some(child_key), out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_metadata_project_paths(item, key, out);
            }
        }
        Value::String(s) => {
            let key = key.unwrap_or_default().to_ascii_lowercase();
            let looks_like_project_path = key.contains("workspace")
                || key.contains("project")
                || key.contains("cwd")
                || key.contains("workdir")
                || key.contains("directory")
                || key == "root";
            if looks_like_project_path && !s.is_empty() {
                out.push(PathBuf::from(s));
            }
        }
        _ => {}
    }
}

fn metadata_task_title(metadata: &Value) -> Option<&str> {
    metadata
        .get("task")
        .or_else(|| metadata.get("title"))
        .or_else(|| metadata.get("summary"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn usage_records(
    provider: &'static str,
    task_id: &str,
    ui_path: &Path,
    ordinal_base: usize,
    location_cwd: &Path,
) -> TranscriptIngestResult<Option<Vec<SessionMessageRecord>>> {
    if !ui_path.is_file() {
        return Ok(Some(Vec::new()));
    }
    let Ok(Some(contents)) = read_snapshot_text_bounded(provider, ui_path, MAX_SNAPSHOT_FILE_BYTES)
    else {
        return Ok(None);
    };
    let document: Value = match serde_json::from_str(&contents) {
        Ok(document) => document,
        Err(error) if error.is_eof() => return Ok(None),
        Err(_) => {
            return Err(non_durable(
                provider,
                ui_path,
                "malformed usage snapshot JSON",
            ));
        }
    };
    let Some(events) = document.as_array() else {
        return Err(non_durable(
            provider,
            ui_path,
            "unsupported usage snapshot root",
        ));
    };
    if events.len() > MAX_USAGE_EVENTS_PER_TASK {
        return Err(non_durable(
            provider,
            ui_path,
            "usage event count exceeds provider bound",
        ));
    }

    let mut records = Vec::new();
    let mut fallback_occurrences = BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        if event.get("type").and_then(Value::as_str) != Some("say")
            || event.get("say").and_then(Value::as_str) != Some("api_req_started")
        {
            continue;
        }
        let Some(text) = event.get("text").and_then(Value::as_str) else {
            continue;
        };
        let Some(usage) = usage_from_api_req_started(text) else {
            continue;
        };
        let timestamp = entry_timestamp(event);
        let native_id = native_record_id(event);
        let content = usage.to_string();
        let occurrence = if native_id.is_none() {
            let next = fallback_occurrences
                .entry((timestamp, content.clone()))
                .or_insert(0);
            let occurrence = *next;
            *next += 1;
            occurrence
        } else {
            0
        };
        let message_id = stable_message_id(
            task_id,
            "ui-usage",
            native_id,
            timestamp,
            "assistant",
            occurrence,
            &content,
        );
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "source".to_string(),
            Value::String(format!("{provider}_ui_messages")),
        );
        metadata.insert("usage".to_string(), usage.clone());
        metadata.insert(
            "correlation".to_string(),
            native_id.map_or_else(
                || Value::String("unavailable".to_string()),
                |id| serde_json::json!({"native_request_id": id}),
            ),
        );
        append_location_metadata(
            &mut metadata,
            CLINE_LIKE_LOCATION_KEYS,
            TranscriptLocation::new(Some(location_cwd), "task_metadata"),
        );
        records.push(SessionMessageRecord {
            provider: provider.to_string(),
            message_id,
            session_id: task_id.to_string(),
            role: "assistant".to_string(),
            timestamp,
            ordinal: (ordinal_base + index) as i64,
            text: content,
            kind: Some("usage".to_string()),
            model: None,
            tool_names: None,
            source_path: Some(ui_path.to_string_lossy().to_string()),
            source_offset: Some(index as i64),
            metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
        });
    }
    Ok(Some(records))
}

fn usage_from_api_req_started(text: &str) -> Option<Value> {
    let payload: Value = serde_json::from_str(text).ok()?;
    let mut counters = Map::new();
    map_counter(
        &mut counters,
        "input_tokens",
        &payload,
        &["tokensIn", "tokens_in"],
    );
    map_counter(
        &mut counters,
        "output_tokens",
        &payload,
        &["tokensOut", "tokens_out"],
    );
    map_counter(
        &mut counters,
        "cache_read_input_tokens",
        &payload,
        &["cacheReads", "cache_reads"],
    );
    map_counter(
        &mut counters,
        "cache_creation_input_tokens",
        &payload,
        &["cacheWrites", "cache_writes"],
    );
    if let Some(total) = payload
        .get("totalTokens")
        .or_else(|| payload.get("total_tokens"))
        .and_then(Value::as_i64)
    {
        counters.insert("total_tokens".to_string(), Value::from(total));
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

fn map_counter(
    counters: &mut Map<String, Value>,
    target_key: &str,
    payload: &Value,
    source_keys: &[&str],
) {
    for key in source_keys {
        if let Some(count) = payload.get(*key).and_then(Value::as_i64) {
            counters.insert(target_key.to_string(), Value::from(count));
            return;
        }
    }
}

fn message_from_entry(
    provider: &str,
    entry: &Value,
    task_id: &str,
    path: &Path,
    index: usize,
    location_cwd: &Path,
    fallback_occurrences: &mut BTreeMap<(String, Option<i64>, String), usize>,
) -> Option<SessionMessageRecord> {
    let role = match entry.get("role").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" | "model" => "assistant",
        _ => return None,
    };
    let content = entry.get("content").unwrap_or(entry);
    let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
    if text.trim().is_empty() {
        return None;
    }
    let timestamp = entry_timestamp(entry);
    let model = entry
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let native_id = native_record_id(entry);
    let occurrence = if native_id.is_none() {
        let next = fallback_occurrences
            .entry((role.to_string(), timestamp, text.clone()))
            .or_insert(0);
        let occurrence = *next;
        *next += 1;
        occurrence
    } else {
        0
    };
    let message_id = stable_message_id(
        task_id,
        "api-message",
        native_id,
        timestamp,
        role,
        occurrence,
        &text,
    );

    Some(SessionMessageRecord {
        provider: provider.to_string(),
        message_id,
        session_id: task_id.to_string(),
        role: role.to_string(),
        timestamp,
        ordinal: index as i64,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(index as i64),
        metadata_json: serde_json::to_string(&message_metadata(provider, entry, location_cwd)).ok(),
    })
}

fn entry_timestamp(entry: &Value) -> Option<i64> {
    entry
        .get("ts")
        .or_else(|| entry.get("timestamp"))
        .or_else(|| entry.get("createdAt"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        })
}

fn native_record_id(entry: &Value) -> Option<&str> {
    ["id", "messageId", "message_id", "requestId", "apiRequestId"]
        .iter()
        .find_map(|key| entry.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
}

pub fn normalize_cline_like_snapshot_observations(
    provider: &'static str,
    messages: &[SessionMessageRecord],
) -> TranscriptIngestResult<Vec<ClineLikeSnapshotObservationRecord>> {
    messages
        .iter()
        .map(|message| {
            let order = u64::try_from(message.ordinal)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
            let metadata = message
                .metadata_json
                .as_deref()
                .and_then(|value| serde_json::from_str::<Value>(value).ok());
            let payload = snapshot_native_payload(provider, message, metadata.as_ref())
                .to_string()
                .into_bytes();
            Ok(ClineLikeSnapshotObservationRecord {
                provider,
                session_id: message.session_id.clone(),
                native_record_id: message.message_id.clone(),
                order,
                payload,
            })
        })
        .collect()
}

/// Shape only Cline-family fields evidenced by repository transcript fixtures.
/// The shared fixture contract exposes `content[].type = "tool_use"` names but
/// no native lineage, reasoning, structured tool IDs/arguments/results, Git,
/// or workflow evidence.
fn snapshot_native_payload(
    provider: &str,
    message: &SessionMessageRecord,
    metadata: Option<&Value>,
) -> Value {
    let mut payload = snapshot_message_fields(provider, message);
    let is_usage = message.kind.as_deref() == Some("usage");
    if is_usage {
        payload.remove("role");
        payload.remove("text");
        payload.remove("model");
    } else if let Some(tool_names) = &message.tool_names {
        payload.insert("tool_names".to_string(), Value::String(tool_names.clone()));
    }
    if let Some(usage) = metadata
        .and_then(|value| value.get("usage"))
        .filter(|value| value.is_object())
    {
        payload.insert("usage".to_string(), usage.clone());
    }
    Value::Object(payload)
}

fn stable_message_id(
    task_id: &str,
    kind: &str,
    native_id: Option<&str>,
    timestamp: Option<i64>,
    role: &str,
    occurrence: usize,
    content: &str,
) -> String {
    let timestamp_bytes = timestamp.map(i64::to_be_bytes);
    let timestamp_bytes = timestamp_bytes
        .as_ref()
        .map_or(&[][..], |bytes| bytes.as_slice());
    let occurrence_bytes = u64::try_from(occurrence).unwrap_or(u64::MAX).to_be_bytes();
    stable_snapshot_message_id(
        StableMessageIdDomains {
            delimited_domain: DELIMITED_NATIVE_MESSAGE_ID_DOMAIN,
            delimited_prefix: "cline-like.message-id.v2.",
            derived_domain: DERIVED_MESSAGE_ID_DOMAIN,
            derived_prefix: "cline-like.derived-message.v3.",
        },
        task_id,
        native_id,
        &[
            task_id.as_bytes(),
            kind.as_bytes(),
            role.as_bytes(),
            timestamp_bytes,
            content.as_bytes(),
            &occurrence_bytes,
        ],
    )
}

fn session_metadata(provider: &str, location_cwd: Option<&Path>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String(format!("{provider}_task_history")),
    );
    append_location_metadata(
        &mut metadata,
        CLINE_LIKE_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, "task_metadata"),
    );
    Value::Object(metadata)
}

fn message_metadata(provider: &str, entry: &Value, location_cwd: &Path) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String(format!("{provider}_task_history")),
    );
    append_location_metadata(
        &mut metadata,
        CLINE_LIKE_LOCATION_KEYS,
        TranscriptLocation::new(Some(location_cwd), "task_metadata"),
    );
    append_tool_calls_metadata(&mut metadata, entry);
    append_usage_metadata(&mut metadata, &[entry]);
    Value::Object(metadata)
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn snapshot_budget_counts_all_task_input_files_once() {
        let temp = tempfile::TempDir::new().expect("temp Cline task");
        let task_dir = temp.path().join("task-1");
        std::fs::create_dir_all(&task_dir).unwrap();
        let transcript = task_dir.join("api_conversation_history.json");
        std::fs::write(&transcript, b"12345").unwrap();
        std::fs::write(task_dir.join("ui_messages.json"), b"1234").unwrap();
        std::fs::write(task_dir.join("task_metadata.json"), b"123").unwrap();
        std::fs::write(task_dir.join("history_item.json"), b"12").unwrap();
        std::fs::write(task_dir.join("history.json"), b"1").unwrap();

        assert_eq!(snapshot_input_bytes("cline", &transcript).unwrap(), 15);
    }

    #[test]
    fn snapshot_discovery_filters_scope_before_spending_byte_budget() {
        let temp = tempfile::TempDir::new().expect("temp Cline storage");
        let tasks = temp.path().join("tasks");
        let project = temp.path().join("project");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        for (task, cwd) in [("relevant", &project), ("unrelated", &other)] {
            let task_dir = tasks.join(task);
            std::fs::create_dir_all(&task_dir).unwrap();
            std::fs::write(task_dir.join("api_messages.json"), b"[]").unwrap();
            std::fs::write(
                task_dir.join("task_metadata.json"),
                serde_json::json!({"cwd": cwd}).to_string(),
            )
            .unwrap();
        }
        let source = ClineLikeSource {
            provider: "cline",
            storage_roots: vec![tasks],
            user_registered_roots: None,
        };

        let paths = source.transcript_paths(&project);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("relevant/api_messages.json"));
    }

    #[tokio::test]
    async fn pre_cancelled_snapshot_capture_does_not_advance_cline_source() {
        use crate::admission::test_support::PanicHostAdmission;

        let temp = tempfile::TempDir::new().expect("temp Cline storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let task_dir = temp.path().join("tasks").join("cancelled");
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::write(
            task_dir.join("api_messages.json"),
            serde_json::json!([{"role": "assistant", "content": "retry me"}]).to_string(),
        )
        .unwrap();
        std::fs::write(
            task_dir.join("task_metadata.json"),
            serde_json::json!({"cwd": project}).to_string(),
        )
        .unwrap();
        let source = ClineLikeSource {
            provider: "cline",
            storage_roots: vec![temp.path().join("tasks")],
            user_registered_roots: None,
        };
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();

        let error = capture_cline_like_snapshot_observations(
            &PanicHostAdmission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .expect_err("pre-cancelled Cline capture must stop before persistence");
        assert!(matches!(
            error,
            TranscriptIngestError::NonDurableRecord {
                reason: "admission_cancelled",
                ..
            }
        ));
    }

    fn message(provider: &str, ordinal: i64) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: provider.to_string(),
            message_id: "task-1:native-message-1".to_string(),
            session_id: "task-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_800_000_000),
            ordinal,
            text: "Redacted response".to_string(),
            kind: Some("message".to_string()),
            model: Some("redacted-model".to_string()),
            tool_names: Some("read_file".to_string()),
            source_path: None,
            source_offset: Some(ordinal),
            metadata_json: Some(serde_json::json!({"task": "redacted"}).to_string()),
        }
    }

    #[test]
    fn provider_identity_and_snapshot_order_feed_canonical_requests() {
        for provider in ["cline", "roo-code", "kilo"] {
            let first =
                normalize_cline_like_snapshot_observations(provider, &[message(provider, 0)])
                    .unwrap();
            let prior =
                normalize_cline_like_snapshot_observations(provider, &[message(provider, 2)])
                    .unwrap();
            let moved =
                normalize_cline_like_snapshot_observations(provider, &[message(provider, 3)])
                    .unwrap();
            assert_eq!(first[0].provider(), provider);
            assert_eq!(first[0].native_record_id(), moved[0].native_record_id());
            assert_eq!(first[0].order(), 0);
            assert_eq!(moved[0].order(), 3);

            let scope = ObservationScopeV1::Profile;
            let generation = ObservationSourceGenerationV1::new(11).unwrap();
            first[0]
                .capture_request(
                    scope.clone(),
                    generation,
                    None,
                    ObservationCancellation::default(),
                )
                .expect("first Cline-like SnapshotOrder request");

            let expected_cursor = prior[0]
                .cursor_after(scope.clone(), generation)
                .expect("typed post-record cursor");
            moved[0]
                .capture_request(
                    scope,
                    generation,
                    Some(expected_cursor),
                    ObservationCancellation::default(),
                )
                .expect("continued Cline-like SnapshotOrder request");
        }
    }

    #[test]
    fn usage_snapshot_emits_only_the_usage_fact() {
        let mut usage = message("cline", 1);
        usage.kind = Some("usage".to_string());
        usage.text = serde_json::json!({"input_tokens": 777}).to_string();
        usage.model = None;
        usage.tool_names = None;
        usage.metadata_json = Some(serde_json::json!({"usage": {"input_tokens": 777}}).to_string());

        let records = normalize_cline_like_snapshot_observations("cline", &[usage]).unwrap();
        let range = ObservationSourceRangeV1::new(1, 2).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &records[0].payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    "cline",
                    "task-1",
                    records[0].native_record_id(),
                    range,
                )
            },
        )
        .expect("usage snapshot envelope");

        let facts = parsed.value()["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["kind"], "usage");
        assert_eq!(facts[0]["input_tokens"], 777);
    }

    #[test]
    fn host_admission_failures_preserve_provider_with_bounded_reason_codes() {
        for provider in ["cline", "roo-code", "kilo"] {
            let error = host_admission_error(
                provider,
                HostAdmissionOutcome {
                    status: HostAdmissionStatus::Unavailable,
                    retryable: true,
                    reason_code: Some("authority_unavailable"),
                },
            );
            assert!(matches!(
                error,
                TranscriptIngestError::NonDurableRecord {
                    provider: error_provider,
                    offset: 0,
                    end_offset: 0,
                    reason: "authority_unavailable",
                } if error_provider == provider
            ));
        }
    }

    #[test]
    fn snapshot_normalization_preserves_roo_code_without_generic_metadata() {
        let native = serde_json::json!({
            "provider": "cline",
            "session_id": "forged-task",
            "message_id": "forged-message",
            "role": "assistant",
            "timestamp": 1_800_000_000_i64,
            "ordinal": 7,
            "kind": "message",
            "model": "redacted-model",
            "text": "Redacted response",
            "tool_names": "read_file",
            "usage": {"input_tokens": 12, "output_tokens": 3},
            // Untyped bags / content-without-visibility must not invent facts.
            "reasoning": "Redacted reasoning",
            "git": {"commit": "redacted"},
            "workflow": {"task": "redacted"},
            "source_path": "/must-not-survive",
            "cwd": "/must-not-survive",
            "metadata": {"must-not-survive": true},
        });
        let range = ObservationSourceRangeV1::new(7, 8).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &serde_json::to_vec(&native).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    "roo-code",
                    "redacted-task",
                    "redacted-task:message",
                    range,
                )
            },
        )
        .expect("redacted Roo Code canonical envelope");
        let canonical = parsed.value();
        assert_eq!(canonical["provider"], "roo-code");
        assert_eq!(canonical["stable_record_id"], "redacted-task:message");
        assert_eq!(canonical["relations"]["session_id"], "redacted-task");
        assert_eq!(
            canonical["relations"]["message_id"],
            "redacted-task:message"
        );
        assert!(canonical["relations"].get("thread_id").is_none());
        assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
        assert_eq!(canonical["evidence"]["range"]["start"], 7);
        // message + tool_names fallback + usage; no invented reasoning/git/workflow
        assert_eq!(canonical["facts"].as_array().unwrap().len(), 3);
        let encoded = canonical.to_string();
        assert!(!encoded.contains("must-not-survive"));
        assert!(!encoded.contains("source_path"));
        assert!(!encoded.contains("metadata"));
        assert!(!encoded.contains("Redacted reasoning"));
    }

    const GOLDEN_API_HISTORY: &str = include_str!(
        "../../../../tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json"
    );
    const GOLDEN_API_MESSAGES: &str = include_str!(
        "../../../../tests/fixtures/transcript_golden/cline_like/input/api_messages.json"
    );
    const GOLDEN_EXPECTED_ASSISTANT: &str = include_str!(
        "../../../../tests/fixtures/transcript_golden/cline_like/expected/assistant_tool_use.canonical.json"
    );
    const GOLDEN_PARSER_PROVENANCE: &str = include_str!(
        "../../../../tests/fixtures/transcript_golden/cline_like/expected/parser_provenance.json"
    );

    #[test]
    fn fixture_backed_tool_use_name_reaches_canonical_facts() {
        // Checked-in golden input (same shape as write_task). Roo's api_messages.json
        // twin must stay byte-equivalent to the shared Cline/Kilo history fixture.
        let history: Value =
            serde_json::from_str(GOLDEN_API_HISTORY).expect("golden api history JSON");
        let roo_twin: Value =
            serde_json::from_str(GOLDEN_API_MESSAGES).expect("golden Roo api_messages JSON");
        assert_eq!(
            history, roo_twin,
            "Roo api_messages.json must mirror the shared Cline-family history shape"
        );
        let expected: Value =
            serde_json::from_str(GOLDEN_EXPECTED_ASSISTANT).expect("golden expected envelope");
        let provenance: Value =
            serde_json::from_str(GOLDEN_PARSER_PROVENANCE).expect("golden parser provenance");
        assert_eq!(
            provenance["ordering_domain"], "snapshot_order",
            "parser provenance must declare SnapshotOrder"
        );
        assert_eq!(
            provenance["unknown_version"]["emitted"], false,
            "Cline-family protocol is unversioned — do not invent UnknownVersion"
        );

        let entries = history.as_array().expect("history array");
        let entry = &entries[1];
        assert_eq!(
            entry["content"][1]["type"], "tool_use",
            "golden must evidence content[].type=tool_use parser path"
        );

        for provider in ["cline", "roo-code", "kilo"] {
            let api_name = expected["per_provider"][provider]["api_history_filename"]
                .as_str()
                .expect("per-provider api filename");
            let message = message_from_entry(
                provider,
                entry,
                "task-1",
                Path::new(api_name),
                1,
                Path::new("/tmp/project"),
                &mut BTreeMap::new(),
            )
            .expect("fixture-backed assistant message");
            assert_eq!(
                message.provider, provider,
                "{provider}: parser must tag provider"
            );
            assert_eq!(message.tool_names.as_deref(), Some("read_file"));

            let records = normalize_cline_like_snapshot_observations(provider, &[message]).unwrap();
            let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
            assert_eq!(native["provider"], provider);
            assert_eq!(
                native["tool_names"],
                expected["parser_derived_native_payload"]["tool_names"]
            );
            for absent in expected["parser_derived_native_payload"]["absent"]
                .as_array()
                .expect("absent native keys")
            {
                let key = absent.as_str().expect("absent key");
                assert!(
                    native.get(key).is_none(),
                    "{provider}: parser must not invent {key}"
                );
            }

            let range = ObservationSourceRangeV1::new(1, 2).unwrap();
            let parsed = parse_normalized_observation_record_v1(
                &records[0].payload,
                range,
                ObservationOrderingDomainV1::SnapshotOrder,
                |native| {
                    canonical_snapshot_envelope(
                        &native,
                        provider,
                        "task-1",
                        records[0].native_record_id(),
                        range,
                    )
                },
            )
            .expect("fixture-backed tool-name envelope");
            let canonical = parsed.value();
            assert_eq!(canonical["provider"], provider);
            assert_eq!(canonical["version"], 1);
            assert_eq!(
                canonical["evidence"]["ordering_domain"],
                expected["assistant_envelope"]["evidence"]["ordering_domain"]
            );
            assert_eq!(
                canonical["evidence"]["native_timestamp"],
                expected["assistant_envelope"]["evidence"]["native_timestamp"]
            );
            for absent in expected["assistant_envelope"]["relations"]["absent"]
                .as_array()
                .expect("absent relations")
            {
                let key = absent.as_str().expect("relation key");
                assert!(
                    canonical["relations"].get(key).is_none(),
                    "{provider}: {key} must stay absent"
                );
            }
            let encoded = canonical.to_string();
            for needle in expected["assistant_envelope"]["encoded_must_contain"]
                .as_array()
                .expect("must_contain")
            {
                let needle = needle.as_str().expect("needle");
                assert!(
                    encoded.contains(needle),
                    "{provider}: envelope missing parser evidence {needle}"
                );
            }
            for needle in expected["assistant_envelope"]["encoded_must_not_contain"]
                .as_array()
                .expect("must_not_contain")
            {
                let needle = needle.as_str().expect("needle");
                assert!(
                    !encoded.contains(needle),
                    "{provider}: envelope must not contain {needle}"
                );
            }
            assert!(
                !encoded.contains("\"kind\":\"workflow_lifecycle\""),
                "{provider}: checked-in tool_use fixture must not emit WorkflowLifecycle"
            );
        }
    }

    #[test]
    fn hostile_lookalike_fields_remain_absent_for_all_variants() {
        let entry = serde_json::json!({
            "role": "assistant",
            "content": "protocol echo",
            "requestId": "req-1",
            "threadId": "hostile-thread",
            "turnId": "hostile-turn",
            "agentId": "hostile-agent",
            "parentAgentId": "hostile-parent-agent",
            "parentMessageId": "hostile-parent-message",
            "reasoning": "hostile reasoning",
            "reasoning_visibility": "visible",
            "git": {"evidence_kind": "commit", "reference": "hostile-commit"},
            "workflow": {"evidence_kind": "task", "reference": "hostile-task"},
            "tool_calls": [{
                "id": "hostile-call",
                "name": "hostile_tool",
                "arguments": {"secret": "hostile-arguments"}
            }],
            "tool_result": {
                "invocation_id": "hostile-call",
                "content": "hostile-result",
                "success": true
            }
        });
        for provider in ["cline", "roo-code", "kilo"] {
            let metadata = message_metadata(provider, &entry, Path::new("/tmp/p"));
            let message = SessionMessageRecord {
                provider: provider.to_string(),
                message_id: format!("{provider}-message"),
                session_id: "task-9".to_string(),
                role: "assistant".to_string(),
                timestamp: Some(1_800_000_000),
                ordinal: 0,
                text: "protocol echo".to_string(),
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: Some(0),
                metadata_json: Some(metadata.to_string()),
            };
            let records = normalize_cline_like_snapshot_observations(provider, &[message]).unwrap();
            let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
            for key in [
                "thread_id",
                "turn_id",
                "agent_id",
                "parent_agent_id",
                "parent_message_id",
                "reasoning_visibility",
                "reasoning",
                "git",
                "workflow",
                "tool_calls",
                "tool_result",
            ] {
                assert!(
                    native.get(key).is_none(),
                    "{provider}: {key} must be absent"
                );
            }

            let range = ObservationSourceRangeV1::new(0, 1).unwrap();
            let parsed = parse_normalized_observation_record_v1(
                &records[0].payload,
                range,
                ObservationOrderingDomainV1::SnapshotOrder,
                |native| {
                    canonical_snapshot_envelope(
                        &native,
                        provider,
                        "task-9",
                        records[0].native_record_id(),
                        range,
                    )
                },
            )
            .expect("hostile lookalikes must normalize without invented facts");
            let canonical = parsed.value();
            let relations = canonical["relations"].as_object().unwrap();
            for key in [
                "thread_id",
                "turn_id",
                "agent_id",
                "parent_agent_id",
                "parent_message_id",
            ] {
                assert!(
                    relations.get(key).is_none(),
                    "{provider}: {key} must be absent"
                );
            }
            let encoded = canonical.to_string();
            for rejected in [
                "hostile-thread",
                "hostile-turn",
                "hostile-agent",
                "hostile reasoning",
                "hostile-commit",
                "hostile-task",
                "hostile-call",
                "hostile_tool",
                "hostile-arguments",
                "hostile-result",
            ] {
                assert!(
                    !encoded.contains(rejected),
                    "{provider}: {rejected} must not survive"
                );
            }
            assert!(
                !encoded.contains("\"kind\":\"workflow_lifecycle\""),
                "{provider}: workflow lookalike must not emit WorkflowLifecycle"
            );
        }
    }

    #[test]
    fn native_message_ids_distinguish_delimiter_ambiguous_structural_tuples() {
        assert_eq!(format!("{}:{}", "a:b", "c"), format!("{}:{}", "a", "b:c"));
        let left = stable_message_id(
            "a:b",
            "api-message",
            Some("c"),
            Some(1),
            "assistant",
            0,
            "ignored",
        );
        let right = stable_message_id(
            "a",
            "api-message",
            Some("b:c"),
            Some(1),
            "assistant",
            0,
            "ignored",
        );
        assert_ne!(left, right);
        assert!(
            left.starts_with("cline-like.message-id.v2.")
                && right.starts_with("cline-like.message-id.v2.")
        );
        assert_eq!(
            left,
            stable_message_id(
                "a:b",
                "api-message",
                Some("c"),
                Some(1),
                "assistant",
                0,
                "ignored",
            ),
            "framed IDs must be deterministic for replay"
        );
    }

    #[test]
    fn native_message_ids_remain_unhashed_provider_identity() {
        let id = stable_message_id(
            "task-1",
            "api-message",
            Some("native-xyz"),
            Some(1_800_000_000),
            "assistant",
            0,
            "ignored-for-native",
        );
        assert_eq!(id, "task-1:native-xyz");
    }

    #[test]
    fn derived_message_ids_encode_timestamp_and_semantic_occurrence() {
        let first = stable_message_id(
            "task-1",
            "api-message",
            None,
            Some(1_800_000_000),
            "assistant",
            0,
            "stable body",
        );
        let reordered = stable_message_id(
            "task-1",
            "api-message",
            None,
            Some(1_800_000_000),
            "assistant",
            0,
            "stable body",
        );
        assert_eq!(first, reordered);
        assert!(first.starts_with("cline-like.derived-message.v3."));
        assert_ne!(
            first,
            stable_message_id(
                "task-1",
                "api-message",
                None,
                Some(1_800_000_000),
                "assistant",
                1,
                "stable body",
            )
        );
    }
}
