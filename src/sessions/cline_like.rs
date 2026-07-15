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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalWorkflowEvidenceKindV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::application::host_admission::{
    HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionStatus,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::SessionMessageRecord;
use crate::sessions::shared::{
    StoredCursor, TranscriptIngestStats, TranscriptLocation, TranscriptLocationMetadataKeys,
    append_location_metadata, append_tool_calls_metadata, append_usage_metadata,
    content_storage_text_and_tools, path_belongs_to_project, title_from_messages,
};
use crate::sessions::source::{
    ParsedTranscript, SessionDraft, TranscriptIngestError, TranscriptIngestResult,
    TranscriptSource, read_changed_with_companion,
};

/// Cap task-directory scans so a long VS Code globalStorage history cannot
/// block dashboard startup.
const MAX_TASK_DIRS_PER_ROOT: usize = 512;
const MAX_TASKS_PER_PASS: usize = 512;
const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 256 * 1024;
const MAX_MESSAGES_PER_TASK: usize = 4_096;
const MAX_USAGE_EVENTS_PER_TASK: usize = 4_096;
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
pub(crate) struct ClineLikeSnapshotObservationRecord {
    provider: &'static str,
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

#[allow(dead_code)]
impl ClineLikeSnapshotObservationRecord {
    pub(crate) fn provider(&self) -> &'static str {
        self.provider
    }

    pub(crate) fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    pub(crate) fn order(&self) -> u64 {
        self.order
    }

    pub(crate) fn capture_request(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        cancellation: ObservationCancellation,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        snapshot_capture_request(self, scope, generation, expected_cursor, cancellation)
    }

    pub(crate) fn cursor_after(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        Ok(ObservationSourceCursorV1::for_ordering(
            snapshot_source_identity(self.provider, &self.session_id)?,
            scope,
            generation,
            ObservationOrderingDomainV1::SnapshotOrder,
            self.order + 1,
        )?)
    }
}

impl ClineLikeSource {
    /// Cline VS Code extension storage:
    /// `Code/User/globalStorage/saoudrizwan.claude-dev/tasks`.
    pub fn cline() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::cline_with_home(&home))
    }

    /// Roo Code VS Code extension storage:
    /// `Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks`.
    pub fn roo_code() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::roo_code_with_home(&home))
    }

    /// Kilo Code storage. Current docs mention both the VS Code extension root
    /// and the CLI root (`~/.kilocode/cli/global/tasks`), so scan both.
    pub fn kilo() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::kilo_with_home(&home))
    }

    pub fn cline_with_home(home: &Path) -> Self {
        Self {
            provider: "cline",
            storage_roots: vec![
                crate::agents::vscode_data_dir(home)
                    .join("User/globalStorage/saoudrizwan.claude-dev/tasks"),
            ],
            user_registered_roots: None,
        }
    }

    pub fn roo_code_with_home(home: &Path) -> Self {
        Self {
            provider: "roo-code",
            storage_roots: vec![
                crate::agents::vscode_data_dir(home)
                    .join("User/globalStorage/rooveterinaryinc.roo-cline/tasks"),
            ],
            user_registered_roots: None,
        }
    }

    pub fn kilo_with_home(home: &Path) -> Self {
        Self {
            provider: "kilo",
            storage_roots: vec![
                crate::agents::vscode_data_dir(home)
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

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in &self.storage_roots {
            let remaining = MAX_TASKS_PER_PASS.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            out.extend(collect_task_api_paths(root).into_iter().take(remaining));
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
            .unwrap_or(MAX_SNAPSHOT_BYTES)
            .min(MAX_SNAPSHOT_BYTES);
        ensure_bounded_file(self.provider, path, byte_cap)?;
        if ui_path.is_file() {
            ensure_bounded_file(self.provider, &ui_path, byte_cap)?;
        }
        let Some(changed) = read_changed_with_companion(path, &ui_path, prev) else {
            return Ok(None);
        };
        let Some(metadata) = read_task_metadata(task_dir) else {
            return Ok(None);
        };
        let location_cwd = if let Some(roots) = &self.user_registered_roots {
            let paths = metadata_project_paths(&metadata);
            if paths
                .iter()
                .any(|path| roots.iter().any(|root| path_belongs_to_project(path, root)))
            {
                return Ok(None);
            }
            let Some(path) = paths.into_iter().next() else {
                return Ok(None);
            };
            path
        } else {
            let Some(path) = metadata_project_location(&metadata, project_root) else {
                return Ok(None);
            };
            path
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
        for (index, entry) in entries.iter().enumerate() {
            if let Some(message) =
                message_from_entry(self.provider, entry, task_id, path, index, &location_cwd)
            {
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
pub(crate) async fn capture_cline_like_snapshot_observations(
    facade: &HostAdmissionFacade<'_>,
    source: &ClineLikeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<TranscriptIngestStats> {
    let mut stats = TranscriptIngestStats::default();
    let mut sessions = BTreeSet::new();
    for path in source.transcript_paths(project_root) {
        let Some(parsed) =
            source.parse_snapshot(&path, StoredCursor::default(), project_root, max_new_bytes)?
        else {
            continue;
        };
        let generation = ObservationSourceGenerationV1::new(parsed.new_cursor.position.max(1))?;
        for record in normalize_cline_like_snapshot_observations(source.provider, &parsed.messages)?
        {
            let source_identity = snapshot_source_identity(record.provider, &record.session_id)?;
            let range = ObservationSourceRangeV1::new(record.order, record.order + 1)?;
            let expected_cursor = facade
                .get_source_cursor(&source_identity, &scope)
                .await
                .map_err(|outcome| host_admission_error(source.provider, outcome))?;
            if expected_cursor.as_ref().is_some_and(|cursor| {
                cursor.generation() == generation && cursor.position() >= range.end()
            }) {
                continue;
            }
            let request = record.capture_request(
                scope.clone(),
                generation,
                expected_cursor.clone(),
                ObservationCancellation::default(),
            )?;
            match facade
                .capture_observation(request)
                .await
                .map_err(|outcome| host_admission_error(source.provider, outcome))?
            {
                CaptureObservationOutcome::Persisted { outcome, .. } => {
                    if matches!(outcome, ObservationPersistOutcome::Committed(_)) {
                        stats.messages_upserted = stats.messages_upserted.saturating_add(1);
                    }
                    sessions.insert(record.session_id);
                }
                CaptureObservationOutcome::Rejected { receipt, .. } => {
                    advance_snapshot_coverage(
                        facade,
                        source_identity,
                        range,
                        expected_cursor,
                        scope.clone(),
                        generation,
                        ObservationCoverageReason::SanitizerRejected,
                        receipt,
                    )
                    .await?;
                }
                CaptureObservationOutcome::Quarantined { receipt, .. } => {
                    advance_snapshot_coverage(
                        facade,
                        source_identity,
                        range,
                        expected_cursor,
                        scope.clone(),
                        generation,
                        ObservationCoverageReason::SanitizerQuarantined,
                        receipt,
                    )
                    .await?;
                }
            }
        }
    }
    stats.sessions_upserted = sessions.len() as u64;
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
async fn advance_snapshot_coverage(
    facade: &HostAdmissionFacade<'_>,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    reason: ObservationCoverageReason,
    receipt: tracedecay_domain::SanitizationReceiptV1,
) -> TranscriptIngestResult<()> {
    let provider = match source.provider().as_str() {
        "cline" => "cline",
        "roo-code" => "roo-code",
        "kilo" => "kilo",
        _ => return Err(TranscriptIngestError::InvalidFrameState { provider: "cline" }),
    };
    let advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
        source,
        scope,
        generation,
        ObservationOrderingDomainV1::SnapshotOrder,
        expected_cursor,
        range,
        reason,
        receipt,
    )
    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
    facade
        .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
        .await
        .map(|_| ())
        .map_err(|outcome| host_admission_error(provider, outcome))
}

fn host_admission_error(
    provider: &'static str,
    outcome: HostAdmissionOutcome,
) -> TranscriptIngestError {
    let reason = outcome.reason_code.unwrap_or(match outcome.status {
        HostAdmissionStatus::Backpressured => "observation_admission_backpressured",
        HostAdmissionStatus::Unavailable => "observation_authority_unavailable",
        HostAdmissionStatus::Unknown => "observation_provider_unsupported",
        HostAdmissionStatus::Degraded => "observation_admission_degraded",
        HostAdmissionStatus::Supported
        | HostAdmissionStatus::AcceptedForReplay
        | HostAdmissionStatus::Committed
        | HostAdmissionStatus::ExactDuplicate => "observation_admission_incomplete",
    });
    TranscriptIngestError::NonDurableRecord {
        provider,
        offset: 0,
        end_offset: 0,
        reason,
    }
}

fn ensure_bounded_file(
    provider: &'static str,
    path: &Path,
    byte_cap: u64,
) -> TranscriptIngestResult<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() > byte_cap {
        return Err(non_durable(
            provider,
            path,
            "snapshot exceeds provider byte bound",
        ));
    }
    Ok(())
}

fn non_durable(provider: &'static str, path: &Path, reason: &'static str) -> TranscriptIngestError {
    let end_offset = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    TranscriptIngestError::NonDurableRecord {
        provider,
        offset: 0,
        end_offset,
        reason,
    }
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

fn read_task_metadata(task_dir: &Path) -> Option<Value> {
    for name in ["task_metadata.json", "history_item.json", "history.json"] {
        let path = task_dir.join(name);
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_METADATA_BYTES {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<Value>(&contents)
        {
            return Some(value);
        }
    }
    None
}

fn metadata_project_location(metadata: &Value, project_root: &Path) -> Option<PathBuf> {
    metadata_project_paths(metadata)
        .into_iter()
        .find(|path| path_belongs_to_project(path, project_root))
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
    let Ok(contents) = std::fs::read_to_string(ui_path) else {
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
        let message_id = stable_message_id(
            task_id,
            "ui-usage",
            native_id,
            timestamp,
            index,
            &usage.to_string(),
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
            text: usage.to_string(),
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
    let message_id = stable_message_id(
        task_id,
        "api-message",
        native_record_id(entry),
        timestamp,
        index,
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

#[allow(dead_code)]
pub(crate) fn normalize_cline_like_snapshot_observations(
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
            let payload = serde_json::json!({
                "provider": provider,
                "session_id": message.session_id,
                "message_id": message.message_id,
                "role": message.role,
                "timestamp": message.timestamp,
                "ordinal": message.ordinal,
                "kind": message.kind,
                "model": message.model,
                "text": message.text,
                "tool_names": message.tool_names,
                "usage": metadata.as_ref().and_then(|value| value.get("usage")),
                "reasoning": metadata.as_ref().and_then(|value| value.get("reasoning")),
                "git": metadata.as_ref().and_then(|value| value.get("git")),
                "workflow": metadata.as_ref().and_then(|value| value.get("workflow")),
            })
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

fn snapshot_capture_request(
    record: &ClineLikeSnapshotObservationRecord,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: ObservationCancellation,
) -> TranscriptIngestResult<CaptureObservationRequest> {
    let range = ObservationSourceRangeV1::new(record.order, record.order + 1)?;
    let parsed = parse_normalized_observation_record_v1(
        &record.payload,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                record.provider,
                &record.session_id,
                &record.native_record_id,
                range,
            )
        },
    )
    .map_err(|_| TranscriptIngestError::NonDurableRecord {
        provider: record.provider,
        offset: range.start(),
        end_offset: range.end(),
        reason: "normalized observation record is not durable",
    })?;
    let source = snapshot_source_identity(record.provider, &record.session_id)?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        ObservationId::new(record.native_record_id.clone())?,
    )?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(format!("transcript.{}.v1", record.provider))?,
        cancellation,
    )
    .map_err(|_| TranscriptIngestError::InvalidFrameState {
        provider: record.provider,
    })
}

fn canonical_snapshot_envelope(
    native: &Value,
    provider: &str,
    session_id: &str,
    message_id: &str,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let invalid = || ObservationRecordParseErrorV1::NormalizationFailed;
    let role = match native.get("role").and_then(Value::as_str) {
        Some("user") => CanonicalMessageRoleV1::User,
        Some("assistant") => CanonicalMessageRoleV1::Assistant,
        Some("system") => CanonicalMessageRoleV1::System,
        Some("tool") => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    };
    let timestamp = native.get("timestamp").and_then(Value::as_i64);
    let mut facts = Vec::new();
    if let Some(text) = native.get("text").cloned() {
        facts.push(CanonicalObservationFactV1::Message {
            role,
            content: text,
            model: native
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            timestamp,
        });
    }
    for (index, name) in native
        .get("tool_names")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(|names| names.split(',').filter(|name| !name.is_empty()))
        .enumerate()
    {
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: ObservationId::new(format!("{message_id}:tool:{index}"))
                .map_err(|_| invalid())?,
            name: name.to_string(),
            arguments: Value::Null,
        });
    }
    if let Some(usage) = native.get("usage").filter(|value| value.is_object()) {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
            cache_write_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64),
            reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_u64),
        });
    }
    if let Some(reasoning) = native
        .get("reasoning")
        .filter(|value| !value.is_null())
        .cloned()
    {
        facts.push(CanonicalObservationFactV1::Reasoning {
            visibility: tracedecay_domain::CanonicalReasoningVisibilityV1::Visible,
            content: Some(reasoning),
        });
    }
    if let Some(git) = native.get("git").filter(|value| !value.is_null()).cloned() {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Unknown,
            reference: None,
            content: Some(git),
        });
    }
    if let Some(workflow) = native
        .get("workflow")
        .filter(|value| !value.is_null())
        .cloned()
    {
        facts.push(CanonicalObservationFactV1::Workflow {
            evidence_kind: CanonicalWorkflowEvidenceKindV1::Unknown,
            reference: None,
            content: Some(workflow),
        });
    }
    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range);
    if let Some(sequence) = native.get("ordinal").and_then(Value::as_u64) {
        evidence = evidence.with_native_sequence(sequence);
    }
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(provider).map_err(|_| invalid())?,
        native
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("message"),
        ObservationId::new(message_id).map_err(|_| invalid())?,
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?)
            .with_message_id(ObservationId::new(message_id).map_err(|_| invalid())?),
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

fn snapshot_source_identity(
    provider: &'static str,
    session_id: &str,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    Ok(ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider)?,
        SessionId::new(session_id.to_string())?,
    )?)
}

fn stable_message_id(
    task_id: &str,
    kind: &str,
    native_id: Option<&str>,
    timestamp: Option<i64>,
    ordinal: usize,
    content: &str,
) -> String {
    if let Some(native_id) = native_id {
        return format!("{task_id}:{native_id}");
    }
    let native_order = timestamp.map_or_else(|| format!("ordinal-{ordinal}"), |ts| ts.to_string());
    let digest = crate::sessions::source::content_hash64(content);
    format!("{task_id}:{kind}:{native_order}:{digest:016x}")
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
    let mut metadata = serde_json::Map::new();
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
        assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
        assert_eq!(canonical["evidence"]["range"]["start"], 7);
        assert_eq!(canonical["facts"].as_array().unwrap().len(), 6);
        let encoded = canonical.to_string();
        assert!(!encoded.contains("must-not-survive"));
        assert!(!encoded.contains("source_path"));
        assert!(!encoded.contains("metadata"));
    }
}
