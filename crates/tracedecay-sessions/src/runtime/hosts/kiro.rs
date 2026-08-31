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
use tracedecay_capture::normalize_timestamp_secs;

use crate::admission::HostAdmission;
#[cfg(test)]
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptLocation,
    TranscriptLocationMetadataKeys, TranscriptScopeMatcher, append_location_metadata,
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    title_from_messages,
};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_METADATA_BYTES, SnapshotCaptureOutcome,
    bounded_snapshot_input_len, capture_snapshot_observations, non_durable_snapshot_record,
    read_snapshot_text_bounded,
};
#[cfg(test)]
use crate::runtime::snapshot_observation::{canonical_snapshot_envelope, host_admission_error};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext_bounded, read_changed_file,
};
use serde_json::{Map, Value};
#[cfg(test)]
use tracedecay_domain::{ObservationOrderingDomainV1, ObservationSourceRangeV1};
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1};
#[cfg(test)]
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;

mod observation;
pub use observation::KiroSnapshotObservationRecord;

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

pub struct KiroSource {
    agent_dir: PathBuf,
    workspace_storage_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
    /// Source-lifetime cache so one scan pass resolves git identity once per
    /// root/workspace instead of once per discovered directory.
    project_matchers: ProjectRootMatcherCache,
}

#[hotpath::measure_all]
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
            project_matchers: ProjectRootMatcherCache::default(),
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

    #[hotpath::measure(label = "sessions.hosts.kiro.transcript_paths")]
    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        if let Some(registered_roots) = &self.user_registered_roots {
            let mut out = collect_user_workspace_session_files(
                &self.agent_dir.join("workspace-sessions"),
                registered_roots,
                &self.project_matchers,
            );
            out.extend(collect_user_agent_storage_files(
                &self.agent_dir,
                &self.workspace_storage_dir,
                registered_roots,
                &self.project_matchers,
            ));
            out.sort();
            out.truncate(MAX_TRANSCRIPTS_PER_PASS);
            return out;
        }
        let mut out = Vec::new();
        out.extend(collect_workspace_session_files(
            &self.agent_dir.join("workspace-sessions"),
            project_root,
            &self.project_matchers,
        ));
        out.extend(collect_agent_storage_files(
            &self.agent_dir,
            &self.workspace_storage_dir,
            project_root,
            &self.project_matchers,
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
    #[hotpath::measure(label = "sessions.hosts.kiro.parse_snapshot")]
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
        // `Unknown` (bounded git timeout) is excluded exactly like `NoMatch`:
        // `Ok(None)` never persists a cursor, so the next scan pass re-resolves
        // the membership instead of misfiling the snapshot.
        if TranscriptScopeMatcher::for_scope_cached(
            project_root,
            self.user_registered_roots.as_deref(),
            &self.project_matchers,
        )
        .membership(Some(&location_cwd))
            != ProjectMembership::Match
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

#[hotpath::measure(label = "sessions.hosts.kiro.collect_user_workspace_sessions")]
fn collect_user_workspace_session_files(
    sessions_root: &Path,
    registered_roots: &[PathBuf],
    project_matchers: &ProjectRootMatcherCache,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::profile_cached(registered_roots, project_matchers);
    let mut workspace_dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let workspace =
                decode_kiro_workspace_path(entry.file_name().to_string_lossy().as_ref())?;
            if scope_matcher.membership(Some(&workspace)) != ProjectMembership::Match {
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
#[hotpath::measure(label = "sessions.hosts.kiro.capture", future = true)]
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
        PROVIDER,
        scope,
        cancellation,
        max_new_bytes,
        || {
            source.discover_transcript_paths(
                project_root,
                TranscriptDiscoveryBounds::from_discovered_units(MAX_TRANSCRIPTS_PER_PASS),
            )
        },
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
    #[hotpath::measure(label = "sessions.hosts.kiro.snapshot_input_bytes")]
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

#[hotpath::measure(label = "sessions.hosts.kiro.collect_workspace_sessions")]
fn collect_workspace_session_files(
    sessions_root: &Path,
    project_root: &Path,
    project_matchers: &ProjectRootMatcherCache,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::project_cached(project_root, project_matchers);
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
        if scope_matcher.membership(Some(&workspace)) != ProjectMembership::Match {
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

#[hotpath::measure(label = "sessions.hosts.kiro.collect_agent_storage")]
fn collect_agent_storage_files(
    agent_dir: &Path,
    workspace_storage_dir: &Path,
    project_root: &Path,
    project_matchers: &ProjectRootMatcherCache,
) -> Vec<PathBuf> {
    let mut workspace_dirs: Vec<(u64, PathBuf, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(agent_dir) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::project_cached(project_root, project_matchers);
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
        if scope_matcher.membership(Some(&workspace)) != ProjectMembership::Match {
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

#[hotpath::measure(label = "sessions.hosts.kiro.collect_user_agent_storage")]
fn collect_user_agent_storage_files(
    agent_dir: &Path,
    workspace_storage_dir: &Path,
    registered_roots: &[PathBuf],
    project_matchers: &ProjectRootMatcherCache,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(agent_dir) else {
        return Vec::new();
    };
    let scope_matcher = TranscriptScopeMatcher::profile_cached(registered_roots, project_matchers);
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
            if scope_matcher.membership(Some(&workspace)) != ProjectMembership::Match {
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

#[hotpath::measure(label = "sessions.hosts.kiro.workspace_path_from_hash")]
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

#[hotpath::measure(label = "sessions.hosts.kiro.messages_from_transcript")]
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
        return Some(normalize_timestamp_secs(ts));
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

#[hotpath::measure(label = "sessions.hosts.kiro.normalize")]
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
mod observation_tests;
