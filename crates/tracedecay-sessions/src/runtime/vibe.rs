//! Mistral Vibe transcript source.
//!
//! Vibe stores sessions under `$VIBE_HOME/logs/session/` or
//! `~/.vibe/logs/session/`. Each session directory contains:
//!
//! * `meta.json` - cumulative metadata, including session id, active model, and
//!   the working directory (`environment.working_directory` in current releases).
//! * `messages.jsonl` - append-only line-delimited LLM messages.
//!
//! This source uses the shared **`ByteOffset`** reader for `messages.jsonl` and
//! scopes sessions to a tracedecay project by matching the working directory in
//! `meta.json` to `project_root`.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;
use tracedecay_capture::vibe as vibe_capture;
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProviderId,
    RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use tracedecay_runtime_core::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::runtime::SessionMessageRecord;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionProgress, JsonlObservationAdmissionRequest,
    admit_jsonl_observations,
};
use crate::runtime::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    title_from_messages, TranscriptScopeMatcher,
};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_METADATA_BYTES, read_snapshot_text_bounded,
};
use crate::runtime::source::{
    FileDiscoveryLimit, FileDiscoveryReport, ParsedTranscript, SessionDraft,
    TranscriptDiscoveryBounds, TranscriptIngestError, TranscriptIngestResult, TranscriptSource,
    path_byte_len, stream_new_jsonl,
};

const PROVIDER: &str = "vibe";
const MAX_SCAN_DEPTH: u8 = 4;
/// Bound global history enumeration so one large Vibe profile cannot stall ingest.
const MAX_SESSION_FILES: usize = 512;
const VIBE_LOCATION_KEYS: TranscriptLocationMetadataKeys = TranscriptLocationMetadataKeys::new(
    "vibe_session_cwd",
    "vibe_session_worktree",
    "vibe_session_location_provenance",
);

/// Vibe session locator + parser.
pub struct VibeSource {
    session_root: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VibeCaptureOutcome {
    pub bytes_consumed: u64,
    pub deferred: bool,
}

impl VibeSource {
    /// Source rooted at the real Vibe home. Returns `None` when the home
    /// directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.vibe/logs/session` (used by tests). This does
    /// not read `VIBE_HOME`; tests can pass the desired base explicitly.
    pub fn with_home(home: &Path) -> Self {
        Self::with_vibe_home(&home.join(".vibe"))
    }

    /// Source rooted at `<vibe_home>/logs/session`.
    pub fn with_vibe_home(vibe_home: &Path) -> Self {
        Self {
            session_root: vibe_home.join("logs").join("session"),
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }

    fn scoped_meta(&self, path: &Path, project_root: &Path) -> Option<VibeMeta> {
        let meta = read_meta(&path.parent()?.join("meta.json"))?;
        if !TranscriptScopeMatcher::for_scope(
            project_root,
            self.user_registered_roots.as_deref(),
        )
        .accepts(Some(&meta.working_directory))
        {
            return None;
        }
        Some(meta)
    }

    /// Eligible `messages.jsonl` only, newest-first under `max_files`, with
    /// stable path tie-break and explicit positional continuation.
    fn discover_eligible_page(
        &self,
        bounds: TranscriptDiscoveryBounds,
        start_offset: usize,
    ) -> (FileDiscoveryReport, usize) {
        let effective_bounds = TranscriptDiscoveryBounds {
            max_files: bounds.max_files.min(MAX_SESSION_FILES),
            ..bounds
        };
        select_newest_eligible_page(
            collect_eligible_messages_jsonl(&self.session_root, MAX_SCAN_DEPTH, effective_bounds),
            effective_bounds.max_files,
            start_offset,
        )
    }
}

impl TranscriptSource for VibeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.discover_transcript_paths(
            project_root,
            TranscriptDiscoveryBounds::from_discovered_units(MAX_SESSION_FILES),
        )
        .paths
    }

    fn discover_transcript_paths(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        self.discover_eligible_page(bounds, 0).0
    }

    fn discover_transcript_paths_page(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
        start_offset: usize,
    ) -> (FileDiscoveryReport, usize) {
        self.discover_eligible_page(bounds, start_offset)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let meta = self.scoped_meta(path, project_root)?;

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        for line in &new.lines {
            if let Some(message) = message_from_line(&line.value, &meta, path, line.offset) {
                messages.push(message);
            }
        }

        let project = self.user_registered_roots.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(&meta)).ok(),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

pub async fn capture_vibe_observations(
    facade: &dyn HostAdmission,
    source: &VibeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<VibeCaptureOutcome> {
    let discovery = source.discover_transcript_paths(
        project_root,
        TranscriptDiscoveryBounds::from_discovered_units(MAX_SESSION_FILES),
    );
    let mut outcome = VibeCaptureOutcome {
        deferred: discovery.is_truncated(),
        ..VibeCaptureOutcome::default()
    };
    let mut remaining = max_new_bytes.unwrap_or(u64::MAX);
    for path in discovery.paths {
        if cancellation.is_cancelled() {
            outcome.deferred = true;
            break;
        }
        if remaining == 0 {
            outcome.deferred = true;
            break;
        }
        let progress = capture_vibe_path(
            facade,
            source,
            &path,
            project_root,
            scope.clone(),
            max_new_bytes.map(|_| remaining),
            cancellation,
        )
        .await?;
        outcome.bytes_consumed = outcome
            .bytes_consumed
            .saturating_add(progress.bytes_consumed);
        outcome.deferred |= progress.source_deferred;
        remaining = remaining.saturating_sub(progress.bytes_consumed);
    }
    Ok(outcome)
}

async fn capture_vibe_path(
    facade: &dyn HostAdmission,
    source: &VibeSource,
    path: &Path,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<JsonlObservationAdmissionProgress> {
    let Some(meta) = source.scoped_meta(path, project_root) else {
        return Ok(JsonlObservationAdmissionProgress {
            bytes_consumed: 0,
            source_deferred: false,
        });
    };
    let provider = ProviderId::new(PROVIDER)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let session_id = SessionId::new(&meta.session_id)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let observation_source = ObservationSourceIdentityV1::for_provider(provider, session_id)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let retention = RetentionClass::new("transcript.vibe.v1")
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        facade,
        observation_source,
        scope,
        retention,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_cancellation(cancellation.clone());
    let session_id = meta.session_id;
    let model = meta.model;

    admit_jsonl_observations(
        request,
        |_| (),
        move |(), bytes, range, _| {
            let native_record_id = vibe_capture::native_record_id(&session_id, range)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
            match parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    vibe_capture::normalize_observation(
                        &native,
                        &session_id,
                        model.as_deref(),
                        native_record_id.clone(),
                        range,
                    )
                },
            ) {
                Ok(parsed) => Ok(JsonlFrameAdmission::durable(parsed, native_record_id)),
                Err(ObservationRecordParseErrorV1::Empty) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::BlankFrame,
                )),
                Err(
                    ObservationRecordParseErrorV1::TooLarge
                    | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge,
                ) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::OversizedFrame,
                )),
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::MalformedFrame,
                )),
            }
        },
    )
    .await
}

/// Fixed metadata charge per directory entry (mirrors discovery walk accounting).
const ENTRY_METADATA_CHARGE_BYTES: u64 = std::mem::size_of::<std::fs::Metadata>() as u64;

fn is_eligible_vibe_transcript(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
}

fn path_mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

/// Walk Vibe session trees retaining only eligible `messages.jsonl` paths.
///
/// Ineligible `.jsonl` siblings are examined for metadata budget but never
/// retained, so they cannot crowd the file-count cap before eligibility.
fn collect_eligible_messages_jsonl(
    dir: &Path,
    max_depth: u8,
    bounds: TranscriptDiscoveryBounds,
) -> FileDiscoveryReport {
    let mut paths = Vec::new();
    let mut truncated = None;
    let mut skipped_oversized_entries = 0u64;
    let mut bytes_charged = 0u64;
    collect_eligible_messages_jsonl_walk(
        dir,
        0,
        max_depth,
        bounds,
        &mut paths,
        &mut truncated,
        &mut skipped_oversized_entries,
        &mut bytes_charged,
    );
    FileDiscoveryReport {
        paths,
        truncated,
        skipped_oversized_entries,
        bytes_charged,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_eligible_messages_jsonl_walk(
    dir: &Path,
    depth: u8,
    max_depth: u8,
    bounds: TranscriptDiscoveryBounds,
    paths: &mut Vec<PathBuf>,
    truncated: &mut Option<FileDiscoveryLimit>,
    skipped_oversized_entries: &mut u64,
    bytes_charged: &mut u64,
) {
    if truncated.is_some() || depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if truncated.is_some() {
            return;
        }
        let file_name = entry.file_name();
        let name_bytes = super::source::os_str_byte_len(&file_name);
        if name_bytes > bounds.max_path_bytes {
            *skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            continue;
        }
        let meta_charge = ENTRY_METADATA_CHARGE_BYTES;
        if meta_charge > bounds.max_metadata_bytes {
            *skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            *truncated = Some(FileDiscoveryLimit::MetadataBytes);
            return;
        }
        if bytes_charged.saturating_add(meta_charge) > bounds.max_discovery_bytes {
            *truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
            return;
        }
        *bytes_charged = bytes_charged.saturating_add(meta_charge);

        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = dir.join(&file_name);
        if file_type.is_symlink() {
            if is_eligible_vibe_transcript(&path) {
                try_retain_eligible(
                    path,
                    bounds,
                    paths,
                    truncated,
                    skipped_oversized_entries,
                    bytes_charged,
                );
            }
            continue;
        }
        if file_type.is_dir() {
            collect_eligible_messages_jsonl_walk(
                &path,
                depth.saturating_add(1),
                max_depth,
                bounds,
                paths,
                truncated,
                skipped_oversized_entries,
                bytes_charged,
            );
            continue;
        }
        if file_type.is_file() && is_eligible_vibe_transcript(&path) {
            try_retain_eligible(
                path,
                bounds,
                paths,
                truncated,
                skipped_oversized_entries,
                bytes_charged,
            );
        }
    }
}

fn try_retain_eligible(
    path: PathBuf,
    bounds: TranscriptDiscoveryBounds,
    paths: &mut Vec<PathBuf>,
    truncated: &mut Option<FileDiscoveryLimit>,
    skipped_oversized_entries: &mut u64,
    bytes_charged: &mut u64,
) {
    if truncated.is_some() {
        return;
    }
    let path_bytes = path_byte_len(&path);
    if path_bytes > bounds.max_path_bytes {
        *skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
        return;
    }
    let path_charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
    if bytes_charged.saturating_add(path_charge) > bounds.max_discovery_bytes {
        *truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
        return;
    }
    *bytes_charged = bytes_charged.saturating_add(path_charge);
    paths.push(path);
}

/// Newest-first selection under `max_files` with path ascending tie-break.
///
/// `start_offset` pages through the current newest-first eligible ranking.
fn select_newest_eligible_page(
    mut collected: FileDiscoveryReport,
    max_files: usize,
    start_offset: usize,
) -> (FileDiscoveryReport, usize) {
    let walk_truncated = collected.truncated;
    let mut ranked = collected
        .paths
        .into_iter()
        .map(|path| (path_mtime_secs(&path), path))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let total_eligible = ranked.len();
    let page = ranked
        .into_iter()
        .skip(start_offset)
        .take(max_files)
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    let omitted_before = start_offset.min(total_eligible);
    let omitted_after = total_eligible.saturating_sub(omitted_before.saturating_add(page.len()));
    // File-count truncation means more eligible sessions remain beyond this page
    // (continuation). Otherwise preserve walk budget truncation so callers know
    // the eligible set may be incomplete.
    collected.truncated = if omitted_after > 0 {
        Some(FileDiscoveryLimit::FileCount)
    } else {
        walk_truncated
    };
    collected.paths = page;
    let omitted_paths = omitted_before
        .saturating_add(omitted_after)
        .saturating_add(usize::from(walk_truncated.is_some()));
    (collected, omitted_paths)
}

struct VibeMeta {
    session_id: String,
    working_directory: PathBuf,
    model: Option<String>,
}

fn read_meta(path: &Path) -> Option<VibeMeta> {
    let text = read_snapshot_text_bounded(PROVIDER, path, MAX_SNAPSHOT_METADATA_BYTES)
        .ok()
        .flatten()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let session_id = value
        .get("session_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            ToString::to_string,
        );
    let working_directory = value
        .pointer("/environment/working_directory")
        .or_else(|| value.pointer("/environment/workdir"))
        .or_else(|| value.pointer("/config/working_directory"))
        .or_else(|| value.pointer("/config/workdir"))
        .or_else(|| value.get("working_directory"))
        .or_else(|| value.get("cwd"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)?;
    let model = value
        .pointer("/config/active_model")
        .or_else(|| value.get("active_model"))
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(VibeMeta {
        session_id,
        working_directory,
        model,
    })
}

fn message_from_line(
    record: &Value,
    meta: &VibeMeta,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .or_else(|| record.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|role| matches!(*role, "user" | "assistant" | "model"))?;
    let normalized_role = if role == "model" { "assistant" } else { role };
    let content = record
        .get("content")
        .or_else(|| record.pointer("/message/content"))
        .unwrap_or(record);
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        record
            .get("tool_calls")
            .or_else(|| record.pointer("/message/tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }
    let timestamp = record
        .get("timestamp")
        .or_else(|| record.get("created_at"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
        });

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: normalized_role.to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model: meta.model.clone(),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(record, meta)).ok(),
    })
}

fn session_metadata(meta: &VibeMeta) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("vibe_messages".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        VIBE_LOCATION_KEYS,
        TranscriptLocation::new(Some(&meta.working_directory), "session_meta"),
    );
    Value::Object(metadata)
}

fn message_metadata(record: &Value, meta: &VibeMeta) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("vibe_messages".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        VIBE_LOCATION_KEYS,
        TranscriptLocation::new(Some(&meta.working_directory), "session_meta"),
    );
    append_tool_calls_metadata(&mut metadata, record);
    if let Some(message) = record.get("message") {
        append_tool_calls_metadata(&mut metadata, message);
        append_usage_metadata(&mut metadata, &[record, message]);
    } else {
        append_usage_metadata(&mut metadata, &[record]);
    }
    Value::Object(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    use serde_json::Value;
    use tempfile::TempDir;

    fn write_session_messages(root: &Path, name: &str, mtime_secs: u64) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.jsonl");
        std::fs::write(&path, "").unwrap();
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();
        path
    }

    fn write_ineligible_jsonl(root: &Path, name: &str, file_name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file_name), "").unwrap();
    }

    #[test]
    fn ineligible_jsonl_does_not_crowd_out_eligible_newest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".vibe/logs/session");
        for index in 0..20 {
            write_ineligible_jsonl(&root, &format!("noise-{index:02}"), "other.jsonl");
        }
        let older = write_session_messages(&root, "session-older", 100);
        let newer = write_session_messages(&root, "session-newer", 200);
        let source = VibeSource::with_home(tmp.path());
        let bounds = TranscriptDiscoveryBounds {
            max_files: 2,
            ..TranscriptDiscoveryBounds::from_discovered_units(2)
        };
        let report = source.discover_transcript_paths(tmp.path(), bounds);
        assert_eq!(report.paths, vec![newer, older]);
        assert!(!report.is_truncated());
    }

    #[test]
    fn newest_selection_uses_stable_path_tie_break() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".vibe/logs/session");
        let shared_mtime = 1_700_000_000;
        let alpha = write_session_messages(&root, "session-a", shared_mtime);
        let bravo = write_session_messages(&root, "session-b", shared_mtime);
        let charlie = write_session_messages(&root, "session-c", shared_mtime);
        let source = VibeSource::with_home(tmp.path());
        let bounds = TranscriptDiscoveryBounds {
            max_files: 3,
            ..TranscriptDiscoveryBounds::from_discovered_units(3)
        };
        let report = source.discover_transcript_paths(tmp.path(), bounds);
        assert_eq!(report.paths, vec![alpha, bravo, charlie]);
    }

    #[test]
    fn pagination_completes_older_work_then_surfaces_finite_new_arrivals() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".vibe/logs/session");
        let oldest = write_session_messages(&root, "session-old", 10);
        let mid = write_session_messages(&root, "session-mid", 20);
        let newest = write_session_messages(&root, "session-new", 30);
        let source = VibeSource::with_home(tmp.path());
        let bounds = TranscriptDiscoveryBounds {
            max_files: 2,
            ..TranscriptDiscoveryBounds::from_discovered_units(2)
        };

        let (page0, omitted0) = source.discover_transcript_paths_page(tmp.path(), bounds, 0);
        assert_eq!(page0.paths, vec![newest.clone(), mid.clone()]);
        assert_eq!(page0.truncated, Some(FileDiscoveryLimit::FileCount));
        assert_eq!(omitted0, 1);

        // A new session shifts the positional ranking, but the next page still
        // reaches the previously omitted oldest session. Once the finite
        // ranking is exhausted, the scheduler's existing reset-to-zero path
        // makes the arrival part of the next newest-first cycle.
        let arrived = write_session_messages(&root, "session-arrived", 40);
        let (page1, omitted1) = source.discover_transcript_paths_page(tmp.path(), bounds, 2);
        assert_eq!(page1.paths, vec![mid, oldest.clone()]);
        assert!(!page1.is_truncated());
        assert_eq!(omitted1, 2);

        let (exhausted, omitted_exhausted) =
            source.discover_transcript_paths_page(tmp.path(), bounds, 4);
        assert!(exhausted.paths.is_empty());
        assert_eq!(omitted_exhausted, 4);

        let (page0_after, _) = source.discover_transcript_paths_page(tmp.path(), bounds, 0);
        assert_eq!(page0_after.paths[0], arrived);
        assert!(page0_after.paths.contains(&newest));
        assert!(!page0_after.paths.contains(&oldest));
    }

    #[test]
    fn vibe_workflow_lookalike_stays_ordinary_message_without_goal_kind() {
        // Vibe has no DurableObservation / WorkflowLifecycle normalizer yet.
        // Prove lookalike lifecycle bags are not promoted into session_messages
        // kind=goal (the legacy goals surface) or metadata keys.
        let input: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/vibe/workflow_lookalike.input.json"
        ))
        .expect("Vibe workflow lookalike input");
        let meta = VibeMeta {
            session_id: "vibe-lookalike".to_string(),
            working_directory: PathBuf::from("/tmp/vibe-project"),
            model: Some("vibe-model".to_string()),
        };
        let message = message_from_line(
            &input,
            &meta,
            Path::new("/tmp/vibe-project/.vibe/logs/session/vibe-lookalike/messages.jsonl"),
            0,
        )
        .expect("lookalike must still parse as a transcript message");
        assert_eq!(message.kind.as_deref(), Some("message"));
        assert_eq!(
            message.text,
            "Vibe workflow lookalike remains an ordinary message"
        );
        let metadata: Value =
            serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
        for key in ["workflow", "todos", "thread_goal_updated", "status", "kind"] {
            assert!(
                metadata.get(key).is_none(),
                "Vibe must not promote lookalike key {key} into message metadata"
            );
        }
        let encoded = message.metadata_json.unwrap_or_default();
        for rejected in [
            "vibe-hostile-task",
            "todo-hostile-1",
            "invented todo",
            "invented goal",
        ] {
            assert!(
                !encoded.contains(rejected),
                "{rejected} must not survive Vibe message metadata shaping"
            );
        }
    }
}
