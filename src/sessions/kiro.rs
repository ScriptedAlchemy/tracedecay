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

use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;
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
    TranscriptSource, collect_files_with_ext, read_changed_file,
};

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
const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MESSAGES_PER_SNAPSHOT: usize = 4_096;

/// Kiro IDE transcript locator + parser.
pub struct KiroSource {
    agent_dir: PathBuf,
    workspace_storage_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KiroSnapshotObservationRecord {
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

#[allow(dead_code)]
impl KiroSnapshotObservationRecord {
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
        snapshot_capture_request(
            PROVIDER,
            self,
            scope,
            generation,
            expected_cursor,
            cancellation,
        )
    }

    pub(crate) fn cursor_after(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        Ok(ObservationSourceCursorV1::for_ordering(
            snapshot_source_identity(PROVIDER, &self.session_id)?,
            scope,
            generation,
            ObservationOrderingDomainV1::SnapshotOrder,
            self.order + 1,
        )?)
    }
}

impl KiroSource {
    /// Source rooted at the real Kiro IDE storage. Returns `None` when home
    /// cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.config/Kiro` (or macOS equivalent).
    pub fn with_home(home: &Path) -> Self {
        let data_dir = crate::agents::kiro_data_dir(home);
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
        if let Some(roots) = &self.user_registered_roots {
            if roots
                .iter()
                .any(|root| path_belongs_to_project(&location_cwd, root))
            {
                return Ok(None);
            }
        } else if !path_belongs_to_project(&location_cwd, project_root) {
            return Ok(None);
        }

        let byte_cap = max_new_bytes
            .unwrap_or(MAX_SNAPSHOT_BYTES)
            .min(MAX_SNAPSHOT_BYTES);
        ensure_bounded_snapshot(path, byte_cap)?;
        let Some(changed) = read_changed_file(path, prev) else {
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
    let mut workspace_dirs = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let workspace =
                decode_workspace_sessions_dir(entry.file_name().to_string_lossy().as_ref())?;
            if registered_roots
                .iter()
                .any(|root| path_belongs_to_project(&workspace, root))
            {
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
        .collect::<Vec<_>>();
    workspace_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir) in workspace_dirs {
        let Ok(entries) = std::fs::read_dir(workspace_dir) else {
            continue;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_none_or(|ext| ext == "json"))
            .collect::<Vec<_>>();
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
pub(crate) async fn capture_kiro_snapshot_observations(
    facade: &HostAdmissionFacade<'_>,
    source: &KiroSource,
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
        for record in normalize_kiro_snapshot_observations(&parsed.messages)? {
            let source_identity = snapshot_source_identity(PROVIDER, &record.session_id)?;
            let range = ObservationSourceRangeV1::new(record.order, record.order + 1)?;
            let expected_cursor = facade
                .get_source_cursor(&source_identity, &scope)
                .await
                .map_err(|outcome| host_admission_error(PROVIDER, outcome))?;
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
                .map_err(|outcome| host_admission_error(PROVIDER, outcome))?
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
    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    facade
        .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
        .await
        .map(|_| ())
        .map_err(|outcome| host_admission_error(PROVIDER, outcome))
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

fn ensure_bounded_snapshot(path: &Path, byte_cap: u64) -> TranscriptIngestResult<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() > byte_cap {
        return Err(non_durable(path, "snapshot exceeds provider byte bound"));
    }
    Ok(())
}

fn non_durable(path: &Path, reason: &'static str) -> TranscriptIngestError {
    let end_offset = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    TranscriptIngestError::NonDurableRecord {
        provider: PROVIDER,
        offset: 0,
        end_offset,
        reason,
    }
}

fn collect_workspace_session_files(sessions_root: &Path, project_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let encoded_dir = entry.path();
        if !encoded_dir.is_dir() {
            continue;
        }
        let Some(workspace) =
            decode_workspace_sessions_dir(entry.file_name().to_string_lossy().as_ref())
        else {
            continue;
        };
        if !path_belongs_to_project(&workspace, project_root) {
            continue;
        }
        let Ok(session_entries) = std::fs::read_dir(&encoded_dir) else {
            continue;
        };
        let mut paths = session_entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_none_or(|ext| ext == "json"))
            .collect::<Vec<_>>();
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
        if !path_belongs_to_project(&workspace, project_root) {
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
        let mut workspace_files = collect_files_with_ext(&workspace_dir, "chat", MAX_SCAN_DEPTH)
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
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
    let mut workspace_dirs = entries
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
            if registered_roots
                .iter()
                .any(|root| path_belongs_to_project(&workspace, root))
            {
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
        .collect::<Vec<_>>();
    workspace_dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir) in workspace_dirs {
        let mut workspace_files = collect_files_with_ext(&workspace_dir, "chat", MAX_SCAN_DEPTH)
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
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
    decode_workspace_sessions_dir(encoded)
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
    let workspace_json = workspace_storage_dir.join(hash).join("workspace.json");
    let contents = std::fs::read_to_string(workspace_json).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    folder_field_to_path(value.get("folder").and_then(Value::as_str)?)
}

fn folder_field_to_path(folder: &str) -> Option<PathBuf> {
    let stripped = folder
        .strip_prefix("file://")
        .or_else(|| folder.strip_prefix("file:"))
        .unwrap_or(folder);
    let decoded = percent_decode_path(stripped);
    if decoded.as_os_str().is_empty() {
        None
    } else {
        Some(decoded)
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
fn pathbuf_from_decoded_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

fn decode_workspace_sessions_dir(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim_end_matches('_');
    if trimmed.is_empty() {
        return None;
    }
    let mut padded = trimmed.replace('-', "+").replace('_', "/");
    let rem = padded.len() % 4;
    if rem > 0 {
        padded.push_str(&"=".repeat(4 - rem));
    }
    let decoded = base64_decode(&padded)?;
    let path = String::from_utf8(decoded).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0_u32;
    let mut bits = 0_u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let val = TABLE.iter().position(|&c| c == byte)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
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
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: stable_message_id(session_id, entry, index, &text),
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
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: stable_message_id(session_id, entry, index, &text),
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
        .and_then(crate::accounting::parser::parse_timestamp)
        .map(|secs| secs as i64)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[allow(dead_code)]
pub(crate) fn normalize_kiro_snapshot_observations(
    messages: &[SessionMessageRecord],
) -> TranscriptIngestResult<Vec<KiroSnapshotObservationRecord>> {
    messages
        .iter()
        .map(|message| {
            let order = u64::try_from(message.ordinal)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
            let metadata = message
                .metadata_json
                .as_deref()
                .and_then(|value| serde_json::from_str::<Value>(value).ok());
            let payload = serde_json::json!({
                "provider": PROVIDER,
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
            Ok(KiroSnapshotObservationRecord {
                session_id: message.session_id.clone(),
                native_record_id: message.message_id.clone(),
                order,
                payload,
            })
        })
        .collect()
}

fn snapshot_capture_request(
    provider: &'static str,
    record: &KiroSnapshotObservationRecord,
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
                provider,
                &record.session_id,
                &record.native_record_id,
                range,
            )
        },
    )
    .map_err(|_| TranscriptIngestError::NonDurableRecord {
        provider,
        offset: range.start(),
        end_offset: range.end(),
        reason: "normalized observation record is not durable",
    })?;
    let source = snapshot_source_identity(provider, &record.session_id)?;
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
        RetentionClass::new("transcript.kiro.v1")?,
        cancellation,
    )
    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })
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

fn stable_message_id(session_id: &str, entry: &Value, index: usize, text: &str) -> String {
    if let Some(native_id) = string_field(entry, &["id", "messageId", "message_id", "eventId"]) {
        return format!("{session_id}:{native_id}");
    }
    let native_order = entry
        .get("timestamp")
        .or_else(|| entry.get("createdAt"))
        .or_else(|| entry.get("startTime"))
        .and_then(parse_timestamp_secs)
        .map_or_else(
            || format!("ordinal-{index}"),
            |timestamp| timestamp.to_string(),
        );
    let digest = crate::sessions::source::content_hash64(text);
    format!("{session_id}:message:{native_order}:{digest:016x}")
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
    append_tool_calls_metadata(&mut metadata, entry);
    append_usage_metadata(&mut metadata, &[entry]);
    Value::Object(metadata)
}

#[cfg(test)]
mod observation_tests {
    use super::*;

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
            "tool_names": "read_file",
            "usage": {"input_tokens": 12, "output_tokens": 3},
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
        assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
        assert_eq!(canonical["evidence"]["range"]["start"], 4);
        assert_eq!(canonical["facts"].as_array().unwrap().len(), 6);
        let encoded = canonical.to_string();
        assert!(!encoded.contains("must-not-survive"));
        assert!(!encoded.contains("source_path"));
        assert!(!encoded.contains("metadata"));
    }
}
