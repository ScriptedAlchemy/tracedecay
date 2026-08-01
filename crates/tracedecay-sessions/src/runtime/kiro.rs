//! AWS Kiro IDE transcript source.
//!
//! Kiro persists chat history under VS Code-style globalStorage at
//! `Kiro/User/globalStorage/kiro.kiroagent`. Two layouts are supported:
//!
//! * **Legacy** — `<workspace-hash>/<execution-id>.chat` JSON with a `chat`
//!   array (`human`/`bot` roles) and `metadata` (model, workflow id, times).
//! * **Modern** — extensionless execution JSON under workspace hash dirs or
//!   `workspace-sessions/<encoded-workspace-path>/<session-id>.json` with a
//!   top-level `messages`/`conversation`/`chat` array.
//!
//! Project scoping resolves each workspace hash via
//! `Kiro/User/workspaceStorage/<hash>/workspace.json` (`folder` field) or, for
//! `workspace-sessions`, by base64-decoding the directory name. The source uses
//! the shared **`ContentHash`** reader because Kiro writes full snapshot files.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::decode_kiro_workspace_path;
use tracedecay_capture::kiro::{
    KiroSnapshotMessage, snapshot_native_payload, stable_message_id as stable_kiro_message_id,
};

use crate::admission::HostAdmission;
#[cfg(test)]
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, TranscriptScopeMatcher,
    append_location_metadata, append_tool_calls_metadata, append_usage_metadata,
    content_storage_text_and_tools, title_from_messages,
};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_METADATA_BYTES, SnapshotAdmissionRecord,
    SnapshotCaptureOutcome, bounded_snapshot_input_len, capture_snapshot_observations,
    non_durable_snapshot_record, read_snapshot_text_bounded,
};
#[cfg(test)]
use crate::runtime::snapshot_observation::{
    canonical_snapshot_envelope, host_admission_error, snapshot_cursor_after,
};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext_bounded, read_changed_file,
};
use serde_json::{Map, Value};
#[cfg(test)]
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationSourceCursorV1, ObservationSourceRangeV1,
};
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1};
#[cfg(test)]
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;

const PROVIDER: &str = "kiro";
const KIRO_LOCATION_KEYS: TranscriptLocationMetadataKeys = TranscriptLocationMetadataKeys::new(
    "kiro_workspace_cwd",
    "kiro_workspace_worktree",
    "kiro_workspace_location_provenance",
);
/// Workspace hash dirs plus one level of session nesting.
const MAX_SCAN_DEPTH: u8 = 3;
/// Bound workspace hash enumeration on large installs.
const MAX_WORKSPACE_DIRS: usize = 256;
const MAX_TRANSCRIPTS_PER_PASS: usize = 512;
const MAX_TRANSCRIPTS_PER_WORKSPACE: usize = 128;
const MAX_MESSAGES_PER_SNAPSHOT: usize = 4_096;

/// Kiro IDE transcript locator + parser.
pub struct KiroSource {
    agent_dir: PathBuf,
    workspace_storage_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KiroSnapshotObservationRecord {
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

impl SnapshotAdmissionRecord for KiroSnapshotObservationRecord {
    fn provider(&self) -> &'static str {
        PROVIDER
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
impl KiroSnapshotObservationRecord {
    fn cursor_after(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        snapshot_cursor_after(PROVIDER, &self.session_id, self.order, scope, generation)
    }
}

impl KiroSource {
    /// Source rooted at the real Kiro IDE storage. Returns `None` when home
    /// cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.config/Kiro` (or macOS equivalent).
    pub fn with_home(home: &Path) -> Self {
        let data_dir = crate::host_ports::kiro_data_dir(home);
        Self {
            agent_dir: data_dir.join("User/globalStorage/kiro.kiroagent"),
            workspace_storage_dir: data_dir.join("User/workspaceStorage"),
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }
}

impl TranscriptSource for KiroSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        if let Some(registered_roots) = &self.user_registered_roots {
            let mut out = collect_user_workspace_session_files(
                &self.agent_dir.join("workspace-sessions"),
                registered_roots,
            );
            out.extend(collect_user_agent_storage_files(
                &self.agent_dir,
                &self.workspace_storage_dir,
                registered_roots,
            ));
            out.sort();
            out.truncate(MAX_TRANSCRIPTS_PER_PASS);
            return out;
        }
        let mut out = Vec::new();
        out.extend(collect_workspace_session_files(
            &self.agent_dir.join("workspace-sessions"),
            project_root,
        ));
        out.extend(collect_agent_storage_files(
            &self.agent_dir,
            &self.workspace_storage_dir,
            project_root,
        ));
        out.sort();
        out.truncate(MAX_TRANSCRIPTS_PER_PASS);
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

impl KiroSource {
    fn parse_snapshot(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        let Some(location_cwd) = transcript_location_path(path, &self.workspace_storage_dir) else {
            return Ok(None);
        };
        if !TranscriptScopeMatcher::for_scope(project_root, self.user_registered_roots.as_deref())
            .accepts(Some(&location_cwd))
        {
            return Ok(None);
        }

        let byte_cap = max_new_bytes
            .unwrap_or(MAX_SNAPSHOT_FILE_BYTES)
            .min(MAX_SNAPSHOT_FILE_BYTES);
        ensure_bounded_snapshot(path, byte_cap)?;
        let Some(changed) = read_changed_file(path, prev, byte_cap) else {
            return Ok(None);
        };
        let value: Value = match serde_json::from_str(&changed.contents) {
            Ok(value) => value,
            Err(error) if error.is_eof() => return Ok(None),
            Err(_) => return Err(non_durable(path, "malformed snapshot JSON")),
        };
        if value.get("executions").and_then(Value::as_array).is_some() {
            return Err(non_durable(path, "unsupported execution index snapshot"));
        }

        let session_id = session_id_from_transcript(path, &value);
        let model = model_from_transcript(&value);
        let messages =
            messages_from_transcript(&value, &session_id, path, model.as_deref(), &location_cwd)?;
        if messages.is_empty() {
            return Err(non_durable(path, "snapshot contains no durable messages"));
        }

        let project = self.user_registered_roots.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(
                Some(&location_cwd),
                Some(&value),
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

fn collect_user_workspace_session_files(
    sessions_root: &Path,
    registered_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::profile(registered_roots);
    let mut workspace_dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let workspace =
                decode_kiro_workspace_path(entry.file_name().to_string_lossy().as_ref())?;
            if !scope_matcher.accepts(Some(&workspace)) {
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
    workspace_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir) in workspace_dirs {
        let Ok(entries) = std::fs::read_dir(workspace_dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_none_or(|ext| ext == "json"))
            .collect();
        paths.sort();
        paths.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        out.extend(paths);
    }
    out
}

/// Captures bounded Kiro snapshots through the daemon-owned observation authority.
///
/// This deliberately re-reads complete snapshots and derives a new source generation
/// from their content hash; it neither consults nor advances legacy parse offsets.
/// `max_new_bytes` is one logical source-byte budget for the complete sweep.
pub async fn capture_kiro_snapshot_observations(
    facade: &dyn HostAdmission,
    source: &KiroSource,
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
            TranscriptDiscoveryBounds::from_discovered_units(MAX_TRANSCRIPTS_PER_PASS),
        ),
        |path| source.snapshot_input_bytes(path),
        |path| {
            let Some(parsed) =
                source.parse_snapshot(path, StoredCursor::default(), project_root, None)?
            else {
                return Ok(None);
            };
            let generation = ObservationSourceGenerationV1::new(parsed.new_cursor.position.max(1))?;
            let records = normalize_kiro_snapshot_observations(&parsed.messages)?;
            Ok(Some((generation, records)))
        },
    )
    .await
}

fn ensure_bounded_snapshot(path: &Path, byte_cap: u64) -> TranscriptIngestResult<()> {
    bounded_snapshot_input_len(PROVIDER, path, byte_cap)
        .map(|_| ())
        .map_err(|_| non_durable(path, "snapshot exceeds provider byte bound"))
}

impl KiroSource {
    fn snapshot_input_bytes(&self, path: &Path) -> TranscriptIngestResult<u64> {
        let transcript_bytes = bounded_snapshot_input_len(PROVIDER, path, MAX_SNAPSHOT_FILE_BYTES)?;
        let metadata_bytes = workspace_hash_from_path(path).map_or(Ok(0), |hash| {
            bounded_snapshot_input_len(
                PROVIDER,
                &workspace_metadata_path(&self.workspace_storage_dir, &hash),
                MAX_SNAPSHOT_METADATA_BYTES,
            )
        })?;
        Ok(transcript_bytes.saturating_add(metadata_bytes))
    }
}

fn non_durable(path: &Path, reason: &'static str) -> TranscriptIngestError {
    non_durable_snapshot_record(PROVIDER, path, reason)
}

fn collect_workspace_session_files(sessions_root: &Path, project_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::project(project_root);
    let mut out = Vec::new();
    let mut matching_workspaces = 0usize;
    for entry in entries.flatten() {
        let encoded_dir = entry.path();
        if !encoded_dir.is_dir() {
            continue;
        }
        let Some(workspace) =
            decode_kiro_workspace_path(entry.file_name().to_string_lossy().as_ref())
        else {
            continue;
        };
        if !scope_matcher.accepts(Some(&workspace)) {
            continue;
        }
        if matching_workspaces >= MAX_WORKSPACE_DIRS {
            break;
        }
        matching_workspaces = matching_workspaces.saturating_add(1);
        let Ok(session_entries) = std::fs::read_dir(&encoded_dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = session_entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_none_or(|ext| ext == "json"))
            .collect();
        paths.sort();
        paths.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        out.extend(paths);
    }
    out
}

fn collect_agent_storage_files(
    agent_dir: &Path,
    workspace_storage_dir: &Path,
    project_root: &Path,
) -> Vec<PathBuf> {
    let mut workspace_dirs: Vec<(u64, PathBuf, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(agent_dir) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::project(project_root);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "workspace-sessions" || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() || name.len() != 32 {
            continue;
        }
        let Some(workspace) = workspace_path_from_hash(workspace_storage_dir, &name) else {
            continue;
        };
        if !scope_matcher.accepts(Some(&workspace)) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        workspace_dirs.push((mtime, path, workspace));
    }
    workspace_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir, _) in workspace_dirs {
        let mut workspace_files = collect_files_with_ext_bounded(
            &workspace_dir,
            "chat",
            MAX_SCAN_DEPTH,
            TranscriptDiscoveryBounds::from_discovered_units(MAX_TRANSCRIPTS_PER_WORKSPACE),
        )
        .paths
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
        workspace_files.sort();
        workspace_files.dedup();
        workspace_files.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        collect_extensionless_execution_files(&workspace_dir, MAX_SCAN_DEPTH, &mut workspace_files);
        workspace_files.sort();
        workspace_files.dedup();
        workspace_files.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        out.extend(workspace_files);
    }
    out
}

fn collect_user_agent_storage_files(
    agent_dir: &Path,
    workspace_storage_dir: &Path,
    registered_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(agent_dir) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::profile(registered_roots);
    let mut workspace_dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();
            if name == "workspace-sessions"
                || name.starts_with('.')
                || !path.is_dir()
                || name.len() != 32
            {
                return None;
            }
            let workspace = workspace_path_from_hash(workspace_storage_dir, &name)?;
            if !scope_matcher.accepts(Some(&workspace)) {
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
    workspace_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir) in workspace_dirs {
        let mut workspace_files = collect_files_with_ext_bounded(
            &workspace_dir,
            "chat",
            MAX_SCAN_DEPTH,
            TranscriptDiscoveryBounds::from_discovered_units(MAX_TRANSCRIPTS_PER_WORKSPACE),
        )
        .paths
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
        workspace_files.sort();
        workspace_files.dedup();
        workspace_files.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        collect_extensionless_execution_files(&workspace_dir, MAX_SCAN_DEPTH, &mut workspace_files);
        workspace_files.sort();
        workspace_files.dedup();
        workspace_files.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
        out.extend(workspace_files);
    }
    out
}

fn collect_extensionless_execution_files(dir: &Path, max_depth: u8, out: &mut Vec<PathBuf>) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_extensionless_execution_files(&path, max_depth - 1, out);
            continue;
        }
        if path.extension().is_some() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "sessions.json") {
            continue;
        }
        out.push(path);
        out.sort();
        out.dedup();
        out.truncate(MAX_TRANSCRIPTS_PER_WORKSPACE);
    }
}

fn transcript_location_path(path: &Path, workspace_storage_dir: &Path) -> Option<PathBuf> {
    if let Some(workspace) = workspace_from_sessions_path(path) {
        return Some(workspace);
    }
    let hash = workspace_hash_from_path(path)?;
    workspace_path_from_hash(workspace_storage_dir, &hash)
}

fn workspace_from_sessions_path(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    let idx = components
        .iter()
        .position(|component| component.as_os_str() == "workspace-sessions")?;
    let encoded = components.get(idx + 1)?.as_os_str().to_str()?;
    decode_kiro_workspace_path(encoded)
}

fn workspace_hash_from_path(path: &Path) -> Option<String> {
    path.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.len() == 32 && name.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string)
    })
}

fn workspace_path_from_hash(workspace_storage_dir: &Path, hash: &str) -> Option<PathBuf> {
    let workspace_json = workspace_metadata_path(workspace_storage_dir, hash);
    let contents =
        read_snapshot_text_bounded(PROVIDER, &workspace_json, MAX_SNAPSHOT_METADATA_BYTES)
            .ok()
            .flatten()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    folder_field_to_path(value.get("folder").and_then(Value::as_str)?)
}

fn workspace_metadata_path(workspace_storage_dir: &Path, hash: &str) -> PathBuf {
    workspace_storage_dir.join(hash).join("workspace.json")
}

fn folder_field_to_path(folder: &str) -> Option<PathBuf> {
    let scheme_len = if folder
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
    {
        7
    } else if folder
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
    {
        5
    } else {
        0
    };
    let stripped = &folder[scheme_len..];
    let legacy_windows_drive = stripped
        .as_bytes()
        .get(..2)
        .is_some_and(|drive| drive[0].is_ascii_alphabetic() && drive[1] == b':');
    let path = if scheme_len > 0
        && !legacy_windows_drive
        && let Ok(uri) = url::Url::parse(folder)
        && let Ok(path) = uri.to_file_path()
    {
        path
    } else {
        percent_decode_path(stripped)
    };
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn percent_decode_path(value: &str) -> PathBuf {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            )
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    pathbuf_from_decoded_bytes(out)
}

#[cfg(unix)]
fn pathbuf_from_decoded_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
#[allow(
    clippy::needless_pass_by_value,
    reason = "all platform implementations share the owned decoded-byte contract"
)]
fn pathbuf_from_decoded_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

fn session_id_from_transcript(path: &Path, value: &Value) -> String {
    string_field(value, &["sessionId", "conversationId", "workflowId", "id"])
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|meta| string_field(meta, &["workflowId", "sessionId"]))
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
}

fn model_from_transcript(value: &Value) -> Option<String> {
    string_field(value, &["modelId", "modelID", "modelName", "model"]).or_else(|| {
        value
            .get("metadata")
            .and_then(|meta| string_field(meta, &["modelId", "modelID"]))
            .map(|model| model.replace('.', "-"))
    })
}

fn messages_from_transcript(
    value: &Value,
    session_id: &str,
    path: &Path,
    model: Option<&str>,
    location_cwd: &Path,
) -> TranscriptIngestResult<Vec<SessionMessageRecord>> {
    if let Some(chat) = value.get("chat").and_then(Value::as_array) {
        if chat.len() > MAX_MESSAGES_PER_SNAPSHOT {
            return Err(non_durable(
                path,
                "snapshot message count exceeds provider bound",
            ));
        }
        return Ok(legacy_chat_messages(
            chat,
            session_id,
            path,
            model,
            value.get("metadata"),
            location_cwd,
        ));
    }
    for key in [
        "messages",
        "conversation",
        "transcript",
        "entries",
        "events",
    ] {
        if let Some(messages) = value.get(key).and_then(Value::as_array) {
            if messages.len() > MAX_MESSAGES_PER_SNAPSHOT {
                return Err(non_durable(
                    path,
                    "snapshot message count exceeds provider bound",
                ));
            }
            return Ok(modern_messages(
                messages,
                session_id,
                path,
                model,
                location_cwd,
            ));
        }
    }
    Err(non_durable(path, "unsupported snapshot message layout"))
}

fn legacy_chat_messages(
    chat: &[Value],
    session_id: &str,
    path: &Path,
    model: Option<&str>,
    metadata: Option<&Value>,
    location_cwd: &Path,
) -> Vec<SessionMessageRecord> {
    let base_ts = metadata
        .and_then(|meta| meta.get("startTime"))
        .and_then(parse_timestamp_secs);
    let mut out = Vec::new();
    let mut fallback_occurrences = BTreeMap::new();
    for (index, entry) in chat.iter().enumerate() {
        let role = match entry.get("role").and_then(Value::as_str) {
            Some("human" | "user") => "user",
            Some("bot" | "assistant" | "model") => "assistant",
            _ => continue,
        };
        let content = entry.get("content").unwrap_or(entry);
        let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
        if text.trim().is_empty() {
            continue;
        }
        let occurrence =
            if string_field(entry, &["id", "messageId", "message_id", "eventId"]).is_none() {
                let next = fallback_occurrences
                    .entry((role.to_string(), None::<i64>, text.clone()))
                    .or_insert(0);
                let occurrence = *next;
                *next += 1;
                occurrence
            } else {
                0
            };
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: stable_message_id(session_id, entry, role, None, occurrence, &text),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: base_ts.map(|ts| ts + index as i64),
            ordinal: index as i64,
            text,
            kind: Some("message".to_string()),
            model: model.map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(path.to_string_lossy().to_string()),
            source_offset: Some(index as i64),
            metadata_json: serde_json::to_string(&message_metadata(entry, Some(location_cwd))).ok(),
        });
    }
    out
}

fn modern_messages(
    messages: &[Value],
    session_id: &str,
    path: &Path,
    model: Option<&str>,
    location_cwd: &Path,
) -> Vec<SessionMessageRecord> {
    let mut out = Vec::new();
    let mut fallback_occurrences = BTreeMap::new();
    for (index, entry) in messages.iter().enumerate() {
        let Some(role) = normalized_role(entry) else {
            continue;
        };
        let content = entry
            .get("content")
            .or_else(|| entry.get("text"))
            .or_else(|| entry.get("message"))
            .unwrap_or(entry);
        let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
        if text.trim().is_empty() {
            continue;
        }
        let timestamp = entry
            .get("timestamp")
            .or_else(|| entry.get("createdAt"))
            .or_else(|| entry.get("startTime"))
            .and_then(parse_timestamp_secs);
        let occurrence =
            if string_field(entry, &["id", "messageId", "message_id", "eventId"]).is_none() {
                let next = fallback_occurrences
                    .entry((role.to_string(), timestamp, text.clone()))
                    .or_insert(0);
                let occurrence = *next;
                *next += 1;
                occurrence
            } else {
                0
            };
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: stable_message_id(session_id, entry, role, timestamp, occurrence, &text),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp,
            ordinal: index as i64,
            text,
            kind: Some("message".to_string()),
            model: model.map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(path.to_string_lossy().to_string()),
            source_offset: Some(index as i64),
            metadata_json: serde_json::to_string(&message_metadata(entry, Some(location_cwd))).ok(),
        });
    }
    out
}

fn normalized_role(entry: &Value) -> Option<&'static str> {
    let role = entry
        .get("role")
        .or_else(|| entry.get("type"))
        .or_else(|| entry.get("author"))
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    match role.as_str() {
        "human" | "user" => Some("user"),
        "bot" | "assistant" | "model" | "ai" => Some("assistant"),
        _ => None,
    }
}

fn parse_timestamp_secs(value: &Value) -> Option<i64> {
    if let Some(ts) = value.as_i64() {
        return Some(if ts >= 1_000_000_000_000 {
            ts / 1000
        } else {
            ts
        });
    }
    value
        .as_str()
        .and_then(crate::host_ports::parse_timestamp)
        .map(|secs| secs as i64)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub fn normalize_kiro_snapshot_observations(
    messages: &[SessionMessageRecord],
) -> TranscriptIngestResult<Vec<KiroSnapshotObservationRecord>> {
    messages
        .iter()
        .map(|message| {
            let order = u64::try_from(message.ordinal)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
            let payload = snapshot_native_payload(KiroSnapshotMessage {
                session_id: &message.session_id,
                message_id: &message.message_id,
                role: &message.role,
                timestamp: message.timestamp,
                ordinal: message.ordinal,
                text: &message.text,
                kind: message.kind.as_deref(),
                model: message.model.as_deref(),
            })
            .to_string()
            .into_bytes();
            Ok(KiroSnapshotObservationRecord {
                session_id: message.session_id.clone(),
                native_record_id: message.message_id.clone(),
                order,
                payload,
            })
        })
        .collect()
}

fn stable_message_id(
    session_id: &str,
    entry: &Value,
    role: &str,
    timestamp: Option<i64>,
    occurrence: usize,
    text: &str,
) -> String {
    let native_id = string_field(entry, &["id", "messageId", "message_id", "eventId"]);
    stable_kiro_message_id(
        session_id,
        native_id.as_deref(),
        role,
        timestamp,
        occurrence,
        text,
    )
}

fn session_metadata(location_cwd: Option<&Path>, transcript: Option<&Value>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("kiro_transcript".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        KIRO_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, "workspace_mapping"),
    );
    if let Some(transcript) = transcript {
        for key in ["workflowId", "profileId", "projectId"] {
            if let Some(value) = transcript
                .get(key)
                .or_else(|| {
                    transcript
                        .get("metadata")
                        .and_then(|metadata| metadata.get(key))
                })
                .filter(|value| value.is_string() || value.is_number() || value.is_boolean())
            {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    Value::Object(metadata)
}

fn message_metadata(entry: &Value, location_cwd: Option<&Path>) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("kiro_transcript".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        KIRO_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, "workspace_mapping"),
    );
    append_tool_calls_metadata(&mut metadata, entry);
    append_usage_metadata(&mut metadata, &[entry]);
    Value::Object(metadata)
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn workspace_folder_file_uri_round_trips_native_paths() {
        let temp = tempfile::TempDir::new().expect("temporary Kiro workspace");
        let path = temp.path().join("workspace with spaces");
        let uri = url::Url::from_file_path(&path).expect("native path has a file URI");

        assert_eq!(folder_field_to_path(uri.as_str()), Some(path));
    }

    #[cfg(windows)]
    #[test]
    fn workspace_folder_file_uri_removes_windows_drive_separator() {
        assert_eq!(
            folder_field_to_path("file:///D:/Kiro%20Workspace"),
            Some(PathBuf::from(r"D:\Kiro Workspace"))
        );
        assert_eq!(
            folder_field_to_path(r"file://D:\Kiro%20Workspace"),
            Some(PathBuf::from(r"D:\Kiro Workspace"))
        );
    }

    #[tokio::test]
    async fn byte_budget_charges_once_and_defers_second_before_parse() {
        use crate::admission::test_support::MemoryHostAdmission;

        let temp = tempfile::TempDir::new().expect("temp Kiro storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let hash = "0123456789abcdef0123456789abcdef";
        let agent_dir = temp.path().join("agent");
        let workspace_dir = agent_dir.join(hash);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let workspace_storage_dir = temp.path().join("workspaces");
        let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
        std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
        std::fs::write(
            &workspace_metadata,
            serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
        )
        .unwrap();
        let first_path = workspace_dir.join("a-first.chat");
        std::fs::write(
            &first_path,
            serde_json::json!({
                "executionId": "first",
                "chat": [{"role": "human", "content": "first"}]
            })
            .to_string(),
        )
        .unwrap();
        let hostile = format!("{{\"executionId\":\"hostile\",\"chat\":{}", "x".repeat(256));
        let second_path = workspace_dir.join("z-hostile.chat");
        std::fs::write(&second_path, &hostile).unwrap();
        let source = KiroSource {
            agent_dir,
            workspace_storage_dir,
            user_registered_roots: None,
        };
        let paths = source.transcript_paths(&project);
        assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
        let first_bytes = source.snapshot_input_bytes(&first_path).unwrap();
        let second_bytes = source.snapshot_input_bytes(&second_path).unwrap();

        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        let deferred = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first_bytes),
            &cancellation,
        )
        .await
        .expect("second unit must defer without parsing malformed JSON");
        assert!(deferred.deferred_by_byte_cap);
        assert_eq!(deferred.stats.messages_upserted, 1);
        assert_eq!(deferred.bytes_consumed, first_bytes);

        std::fs::remove_file(first_path).unwrap();
        let err = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(second_bytes),
            &cancellation,
        )
        .await
        .expect_err("deferred malformed snapshot must remain retryable");
        assert!(matches!(
            err,
            TranscriptIngestError::NonDurableRecord {
                reason: "malformed snapshot JSON",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn pre_cancelled_snapshot_capture_does_not_advance_kiro_source() {
        use crate::admission::test_support::PanicHostAdmission;

        let temp = tempfile::TempDir::new().expect("temp Kiro storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let hash = "0123456789abcdef0123456789abcdef";
        let agent_dir = temp.path().join("agent");
        let workspace_dir = agent_dir.join(hash);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let workspace_storage_dir = temp.path().join("workspaces");
        let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
        std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
        std::fs::write(
            workspace_metadata,
            serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
        )
        .unwrap();
        std::fs::write(
            workspace_dir.join("cancelled.chat"),
            serde_json::json!({
                "executionId": "cancelled",
                "chat": [{"role": "human", "content": "retry me"}]
            })
            .to_string(),
        )
        .unwrap();
        let source = KiroSource {
            agent_dir,
            workspace_storage_dir,
            user_registered_roots: None,
        };
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();

        let error = capture_kiro_snapshot_observations(
            &PanicHostAdmission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .expect_err("pre-cancelled Kiro capture must stop before persistence");
        assert!(matches!(
            error,
            TranscriptIngestError::NonDurableRecord {
                reason: "admission_cancelled",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn aggregate_budget_replay_charges_committed_prefix_and_retries_suffix() {
        use crate::admission::test_support::MemoryHostAdmission;

        let temp = tempfile::TempDir::new().expect("temp Kiro storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let hash = "0123456789abcdef0123456789abcdef";
        let agent_dir = temp.path().join("agent");
        let workspace_dir = agent_dir.join(hash);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let workspace_storage_dir = temp.path().join("workspaces");
        let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
        std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
        std::fs::write(
            &workspace_metadata,
            serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
        )
        .unwrap();
        for id in ["a", "b"] {
            std::fs::write(
                workspace_dir.join(format!("{id}.chat")),
                serde_json::json!({
                    "executionId": id,
                    "chat": [{"role": "human", "content": format!("message-{id}")}]
                })
                .to_string(),
            )
            .unwrap();
        }
        let source = KiroSource {
            agent_dir,
            workspace_storage_dir,
            user_registered_roots: None,
        };
        let paths = source.transcript_paths(&project);
        assert_eq!(paths.len(), 2);
        let first_bytes = source.snapshot_input_bytes(&paths[0]).unwrap();
        let second_bytes = source.snapshot_input_bytes(&paths[1]).unwrap();
        let full_cap = first_bytes.saturating_add(second_bytes);
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();

        let first = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first_bytes),
            &cancellation,
        )
        .await
        .expect("first bounded sweep");
        assert_eq!(first.stats.messages_upserted, 1);
        assert_eq!(first.bytes_consumed, first_bytes);
        assert!(first.deferred_by_byte_cap);

        let second = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first_bytes),
            &cancellation,
        )
        .await
        .expect("committed prefix replay");
        assert_eq!(second.stats.messages_upserted, 0);
        assert_eq!(second.bytes_consumed, first_bytes);
        assert!(second.deferred_by_byte_cap);

        let resumed = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(full_cap),
            &cancellation,
        )
        .await
        .expect("deferred suffix replay");
        assert_eq!(resumed.stats.messages_upserted, 1);
        assert_eq!(resumed.bytes_consumed, full_cap);
        assert!(!resumed.deferred_by_byte_cap);

        let complete = capture_kiro_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(full_cap),
            &cancellation,
        )
        .await
        .expect("complete replay");
        assert_eq!(complete.stats.messages_upserted, 0);
        assert_eq!(complete.bytes_consumed, full_cap);
        assert!(!complete.deferred_by_byte_cap);
    }

    #[test]
    fn snapshot_budget_counts_transcript_and_workspace_metadata() {
        let temp = tempfile::TempDir::new().expect("temp Kiro storage");
        let hash = "0123456789abcdef0123456789abcdef";
        let transcript = temp.path().join("agent").join(hash).join("session.json");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"1234").unwrap();

        let workspace_storage_dir = temp.path().join("workspaces");
        let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
        std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
        std::fs::write(&workspace_metadata, b"123").unwrap();
        let source = KiroSource {
            agent_dir: temp.path().join("agent"),
            workspace_storage_dir,
            user_registered_roots: None,
        };

        assert_eq!(source.snapshot_input_bytes(&transcript).unwrap(), 7);
    }

    fn message(ordinal: i64) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: "native-message-1".to_string(),
            session_id: "kiro-session-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_800_000_000),
            ordinal,
            text: "Redacted response".to_string(),
            kind: Some("message".to_string()),
            model: Some("redacted-model".to_string()),
            tool_names: Some("read_file".to_string()),
            source_path: None,
            source_offset: Some(ordinal),
            metadata_json: Some(serde_json::json!({"projectId": "project-1"}).to_string()),
        }
    }

    #[test]
    fn snapshot_records_build_canonical_capture_requests() {
        let first = normalize_kiro_snapshot_observations(&[message(0)]).unwrap();
        let prior = normalize_kiro_snapshot_observations(&[message(3)]).unwrap();
        let moved = normalize_kiro_snapshot_observations(&[message(4)]).unwrap();
        assert_eq!(first[0].native_record_id(), moved[0].native_record_id());
        assert_eq!(first[0].order(), 0);
        assert_eq!(moved[0].order(), 4);

        let scope = ObservationScopeV1::Profile;
        let generation = ObservationSourceGenerationV1::new(7).unwrap();
        first[0]
            .capture_request(
                scope.clone(),
                generation,
                None,
                ObservationCancellation::default(),
            )
            .expect("first Kiro SnapshotOrder request");

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
            .expect("continued Kiro SnapshotOrder request");
    }

    #[test]
    fn host_admission_failures_use_bounded_ingest_reason_codes() {
        let error = host_admission_error(
            PROVIDER,
            HostAdmissionOutcome {
                status: HostAdmissionStatus::Unavailable,
                retryable: true,
                reason_code: Some("authority_unavailable"),
            },
        );
        assert!(matches!(
            error,
            TranscriptIngestError::NonDurableRecord {
                provider: PROVIDER,
                offset: 0,
                end_offset: 0,
                reason: "authority_unavailable",
            }
        ));
    }

    #[test]
    fn snapshot_normalization_emits_only_redacted_canonical_evidence() {
        let native = serde_json::json!({
            "provider": "kiro",
            "session_id": "redacted-session",
            "message_id": "redacted-message",
            "role": "assistant",
            "timestamp": 1_800_000_000_i64,
            "ordinal": 4,
            "kind": "message",
            "model": "redacted-model",
            "text": "Redacted response",
            // Untyped bags / content-without-visibility must not invent facts.
            "reasoning": "Redacted reasoning",
            "git": {"commit": "redacted"},
            "workflow": {"task": "redacted"},
            "source_path": "/must-not-survive",
            "cwd": "/must-not-survive",
            "metadata": {"must-not-survive": true},
        });
        let range = ObservationSourceRangeV1::new(4, 5).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &serde_json::to_vec(&native).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    "kiro",
                    "redacted-session",
                    "redacted-message",
                    range,
                )
            },
        )
        .expect("redacted Kiro canonical envelope");
        let canonical = parsed.value();
        assert_eq!(canonical["provider"], "kiro");
        assert_eq!(canonical["stable_record_id"], "redacted-message");
        assert_eq!(canonical["relations"]["session_id"], "redacted-session");
        assert_eq!(canonical["relations"]["message_id"], "redacted-message");
        assert!(canonical["relations"].get("thread_id").is_none());
        assert!(canonical["relations"].get("turn_id").is_none());
        assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
        assert_eq!(canonical["evidence"]["range"]["start"], 4);
        assert_eq!(canonical["facts"].as_array().unwrap().len(), 1);
        let encoded = canonical.to_string();
        assert!(!encoded.contains("must-not-survive"));
        assert!(!encoded.contains("source_path"));
        assert!(!encoded.contains("metadata"));
        assert!(!encoded.contains("Redacted reasoning"));
    }

    #[test]
    fn hostile_lookalike_fields_remain_absent() {
        // The Kiro fixtures in tests/transcript_ingest_suite/kiro.rs contain
        // role/content plus transcript-level identity/model/time only. These
        // lookalike keys have no fixture-backed Kiro semantics.
        let entry = serde_json::json!({
            "role": "assistant",
            "content": "echoed protocol noise",
            "threadId": "thread-native-1",
            "turnId": "turn-native-1",
            "agentId": "agent-native-1",
            "parentAgentId": "parent-agent-1",
            "parentMessageId": "parent-msg-1",
            "tool_calls": [{
                "id": "call-read-1",
                "function": {
                    "name": "read_file",
                    "arguments": "{\"path\":\"src/billing.rs\"}"
                }
            }],
            "reasoning": "check the invoice join",
            "reasoning_visibility": "visible",
            "git": {
                "evidence_kind": "commit",
                "reference": "abc123"
            },
            "workflow": {
                "evidence_kind": "task",
                "reference": "kiro-workflow-1"
            },
            "tool_result": {
                "invocation_id": "call-read-1",
                "content": "arbitrary result",
                "success": true
            },
            "usage": {"input_tokens": 999_999}
        });
        let metadata = message_metadata(&entry, None);
        // V1 metadata may retain a sibling tool_calls bag, but snapshot shaping
        // must not promote that uncontracted object into canonical evidence.
        let message = SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: "kiro-session:msg-native-1".to_string(),
            session_id: "kiro-session".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_800_000_000),
            ordinal: 2,
            text: "echoed protocol noise".to_string(),
            kind: Some("message".to_string()),
            model: Some("redacted-model".to_string()),
            tool_names: None,
            source_path: None,
            source_offset: Some(2),
            metadata_json: Some(metadata.to_string()),
        };
        let records = normalize_kiro_snapshot_observations(&[message]).unwrap();
        let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
        for key in [
            "thread_id",
            "turn_id",
            "agent_id",
            "parent_agent_id",
            "parent_message_id",
            "reasoning_visibility",
            "reasoning",
            "tool_calls",
            "tool_result",
            "usage",
            "git",
            "workflow",
        ] {
            assert!(native.get(key).is_none(), "{key} must remain absent");
        }

        let range = ObservationSourceRangeV1::new(2, 3).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &records[0].payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    PROVIDER,
                    "kiro-session",
                    "kiro-session:msg-native-1",
                    range,
                )
            },
        )
        .expect("typed Kiro envelope");
        let canonical = parsed.value();
        let relations = canonical["relations"].as_object().unwrap();
        for key in [
            "thread_id",
            "turn_id",
            "agent_id",
            "parent_agent_id",
            "parent_message_id",
        ] {
            assert!(relations.get(key).is_none(), "{key} must remain absent");
        }
        let encoded = canonical.to_string();
        for rejected in [
            "thread-native-1",
            "turn-native-1",
            "agent-native-1",
            "check the invoice join",
            "call-read-1",
            "abc123",
            "kiro-workflow-1",
            "arbitrary result",
            "999999",
        ] {
            assert!(!encoded.contains(rejected), "{rejected} must not survive");
        }
        assert!(
            !encoded.contains("\"kind\":\"workflow_lifecycle\""),
            "Kiro hostile workflow lookalike must not emit WorkflowLifecycle"
        );
    }

    #[test]
    fn fixture_backed_workspace_session_message_reaches_canonical_envelope() {
        // Exact modern workspace-session message shape from
        // tests/transcript_ingest_suite/kiro.rs::write_workspace_session_json.
        // Provider-parser path: modern_messages → normalize_kiro_snapshot_observations
        // → canonical_snapshot_envelope (not a hand-built canonical record).
        let input: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/kiro/workspace_session.input.json"
        ))
        .expect("Kiro golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/kiro/workspace_session.expected_envelope.json"
        ))
        .expect("Kiro golden expected envelope");
        let session_id = input["sessionId"].as_str().unwrap();
        let model = input["modelId"].as_str();
        let messages = modern_messages(
            input["messages"].as_array().unwrap(),
            session_id,
            Path::new("workspace-session.json"),
            model,
            Path::new("/tmp/project"),
        );
        let message = messages
            .into_iter()
            .find(|message| message.role == "assistant")
            .expect("Kiro golden assistant");
        let text = message.text.clone();
        let message_id = message.message_id.clone();
        assert!(
            message_id.starts_with("kiro.derived-message.v3."),
            "fallback identity must use the framed v3 domain, got {message_id}"
        );

        let records = normalize_kiro_snapshot_observations(&[message]).unwrap();
        let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
        assert_eq!(native["provider"], PROVIDER);
        assert_eq!(native["role"], "assistant");
        assert_eq!(native["text"], text);
        assert_eq!(native["model"], "claude-sonnet-4.6");
        assert!(native.get("tool_calls").is_none());
        assert!(native.get("reasoning").is_none());
        assert!(native.get("metadata").is_none());

        let range = ObservationSourceRangeV1::new(1, 2).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &records[0].payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| canonical_snapshot_envelope(&native, PROVIDER, session_id, &message_id, range),
        )
        .expect("fixture-backed Kiro canonical envelope");
        let canonical = parsed.value();
        assert_eq!(canonical["version"], expected["version"]);
        assert_eq!(canonical["provider"], expected["provider"]);
        assert_eq!(
            canonical["native_record_kind"],
            expected["native_record_kind"]
        );
        assert_eq!(canonical["stable_record_id"], message_id);
        assert_eq!(
            canonical["relations"]["session_id"],
            expected["relations"]["session_id"]
        );
        assert_eq!(canonical["relations"]["message_id"], message_id);
        for absent in expected["relations"]["absent"].as_array().unwrap() {
            assert!(
                canonical["relations"]
                    .get(absent.as_str().unwrap())
                    .is_none()
            );
        }
        assert_eq!(canonical["evidence"], expected["evidence"]);
        assert_eq!(canonical["facts"], expected["facts"]);
        assert_eq!(text, expected["facts"][0]["content"].as_str().unwrap());
        assert!(
            canonical["facts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|fact| fact["kind"] != "workflow_lifecycle"),
            "Kiro workspace fixture has no native lifecycle evidence for WorkflowLifecycle"
        );
    }

    #[test]
    fn native_message_ids_distinguish_delimiter_ambiguous_structural_tuples() {
        assert_eq!(format!("{}:{}", "a:b", "c"), format!("{}:{}", "a", "b:c"));
        let left = stable_message_id(
            "a:b",
            &serde_json::json!({"messageId": "c"}),
            "assistant",
            None,
            0,
            "ignored",
        );
        let right = stable_message_id(
            "a",
            &serde_json::json!({"messageId": "b:c"}),
            "assistant",
            None,
            0,
            "ignored",
        );
        assert_ne!(left, right);
        assert!(
            left.starts_with("kiro.message-id.v2.") && right.starts_with("kiro.message-id.v2.")
        );
        assert_eq!(
            left,
            stable_message_id(
                "a:b",
                &serde_json::json!({"messageId": "c"}),
                "assistant",
                None,
                0,
                "ignored",
            ),
            "framed IDs must be deterministic for replay"
        );
    }

    #[test]
    fn native_message_ids_remain_unhashed_provider_identity() {
        let id = stable_message_id(
            "sess-1",
            &serde_json::json!({"messageId": "native-xyz", "content": "ignored"}),
            "assistant",
            None,
            0,
            "ignored-for-native",
        );
        assert_eq!(id, "sess-1:native-xyz");
    }

    #[test]
    fn derived_message_ids_encode_role_timestamp_and_semantic_occurrence() {
        let entry = serde_json::json!({"role": "assistant", "content": "stable body"});
        let first = stable_message_id(
            "sess-1",
            &entry,
            "assistant",
            Some(1_800_000_000),
            0,
            "stable body",
        );
        let reordered = stable_message_id(
            "sess-1",
            &entry,
            "assistant",
            Some(1_800_000_000),
            0,
            "stable body",
        );
        assert_eq!(first, reordered);
        assert!(first.starts_with("kiro.derived-message.v3."));
        assert_ne!(
            first,
            stable_message_id(
                "sess-1",
                &entry,
                "user",
                Some(1_800_000_000),
                0,
                "stable body",
            )
        );
        assert_ne!(
            first,
            stable_message_id(
                "sess-1",
                &entry,
                "assistant",
                Some(1_800_000_000),
                1,
                "stable body",
            )
        );
    }
}
