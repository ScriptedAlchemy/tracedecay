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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use crate::admission::HostAdmission;
#[cfg(test)]
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptLocation,
    TranscriptLocationMetadataKeys, append_location_metadata, append_tool_calls_metadata,
    append_usage_metadata, content_storage_text_and_tools, title_from_messages,
};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_METADATA_BYTES, SnapshotCaptureOutcome,
    StableMessageIdDomains, bounded_snapshot_input_len, capture_snapshot_observations,
    non_durable_snapshot_record, read_snapshot_text_bounded, snapshot_message_fields,
    stable_snapshot_message_id,
};
#[cfg(test)]
use crate::runtime::snapshot_observation::{canonical_snapshot_envelope, host_admission_error};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, read_changed_with_companion,
};
use serde_json::{Map, Value};
#[cfg(test)]
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ObservationOrderingDomainV1, ObservationSourceRangeV1,
};
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1};
#[cfg(test)]
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;

mod observation;
pub use observation::ClineLikeSnapshotObservationRecord;

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

#[derive(Clone, Default)]
struct TaskMetadataCache {
    entries: Arc<Mutex<HashMap<PathBuf, Value>>>,
}

impl TaskMetadataCache {
    fn get(&self, provider: &'static str, task_dir: &Path) -> Option<Value> {
        if let Some(cached) = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(task_dir)
        {
            return Some(cached.clone());
        }
        let metadata = read_task_metadata(provider, task_dir)?;
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(task_dir.to_path_buf(), metadata.clone());
        Some(metadata)
    }
}

#[derive(Clone)]
pub struct ClineLikeSource {
    provider: &'static str,
    storage_roots: Vec<PathBuf>,
    user_registered_roots: Option<Vec<PathBuf>>,
    /// Source-lifetime cache so one scan pass resolves git identity once per
    /// root/task path instead of once per task directory.
    project_matchers: ProjectRootMatcherCache,
    /// Task metadata is read during discovery and again at parse; keep the
    /// parsed document for the life of the source.
    task_metadata: TaskMetadataCache,
}

#[hotpath::measure_all]
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
            project_matchers: ProjectRootMatcherCache::default(),
            task_metadata: TaskMetadataCache::default(),
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
            project_matchers: ProjectRootMatcherCache::default(),
            task_metadata: TaskMetadataCache::default(),
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
            project_matchers: ProjectRootMatcherCache::default(),
            task_metadata: TaskMetadataCache::default(),
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

    #[hotpath::measure(label = "sessions.hosts.cline_like.transcript_paths")]
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

#[hotpath::measure_all]
impl ClineLikeSource {
    fn snapshot_location(&self, path: &Path, project_root: &Path) -> Option<PathBuf> {
        let metadata = self.task_metadata.get(self.provider, path.parent()?)?;
        self.snapshot_location_from_metadata(&metadata, project_root)
    }

    #[hotpath::measure(label = "sessions.hosts.cline_like.snapshot_location")]
    fn snapshot_location_from_metadata(
        &self,
        metadata: &Value,
        project_root: &Path,
    ) -> Option<PathBuf> {
        let paths = metadata_project_paths(metadata);
        if let Some(roots) = &self.user_registered_roots {
            // `Unknown` (bounded git timeout) excludes the task exactly like a
            // registered-project match: a `None` here never persists a cursor,
            // so the next scan pass re-resolves the membership instead of
            // misfiling the task into the user store.
            if paths.iter().any(|path| {
                self.project_matchers.membership_against_roots(path, roots)
                    != ProjectMembership::NoMatch
            }) {
                return None;
            }
            paths.into_iter().next()
        } else {
            let mut matched = None;
            for path in paths {
                match self.project_matchers.membership(&path, project_root) {
                    ProjectMembership::Match => {
                        if matched.is_none() {
                            matched = Some(path);
                        }
                    }
                    // A definitive match on any metadata path decides the
                    // location; an `Unknown` auxiliary path must not veto it.
                    // With no match the task stays excluded without persisting
                    // a cursor, so a later pass re-resolves the membership.
                    ProjectMembership::NoMatch | ProjectMembership::Unknown => {}
                }
            }
            matched
        }
    }

    #[hotpath::measure(label = "sessions.hosts.cline_like.parse_snapshot")]
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
        let Some(metadata) = self.task_metadata.get(self.provider, task_dir) else {
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
            changed.companion_contents.as_deref(),
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
#[hotpath::measure(label = "sessions.hosts.cline_like.capture", future = true)]
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
        source.provider,
        scope,
        cancellation,
        max_new_bytes,
        || {
            source.discover_transcript_paths(
                project_root,
                TranscriptDiscoveryBounds::from_discovered_units(MAX_TASKS_PER_PASS),
            )
        },
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

#[hotpath::measure(label = "sessions.hosts.cline_like.snapshot_input_bytes")]
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

#[hotpath::measure(label = "sessions.hosts.cline_like.collect_task_api_paths")]
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

#[hotpath::measure(label = "sessions.hosts.cline_like.read_task_metadata")]
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

#[hotpath::measure(label = "sessions.hosts.cline_like.usage_records")]
fn usage_records(
    provider: &'static str,
    task_id: &str,
    ui_path: &Path,
    companion_contents: Option<&str>,
    ordinal_base: usize,
    location_cwd: &Path,
) -> TranscriptIngestResult<Option<Vec<SessionMessageRecord>>> {
    if !ui_path.is_file() {
        return Ok(Some(Vec::new()));
    }
    let owned;
    let contents = if let Some(contents) = companion_contents {
        contents
    } else {
        match read_snapshot_text_bounded(provider, ui_path, MAX_SNAPSHOT_FILE_BYTES) {
            Ok(Some(contents)) => {
                owned = contents;
                owned.as_str()
            }
            _ => return Ok(None),
        }
    };
    let document: Value = match serde_json::from_str(contents) {
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

#[hotpath::measure(label = "sessions.hosts.cline_like.normalize")]
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
mod location_tests;

#[cfg(test)]
mod observation_tests;
