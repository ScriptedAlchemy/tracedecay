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
use crate::runtime::hosts::codex::{CodexDiscoveryHub, PendingTranscript};
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionProgress, JsonlObservationAdmissionRequest,
    admit_jsonl_observations,
};
use crate::runtime::shared::{ProjectMembership, ProjectRootMatcherCache, TranscriptScopeMatcher};
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_METADATA_BYTES, read_snapshot_text_bounded,
};
use crate::runtime::source::{
    FileDiscoveryLimit, FileDiscoveryReport, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, path_byte_len, run_blocking_transcript_section,
};
use tracedecay_privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
    protect_sensitive_structural_id,
};

const PROVIDER: &str = "vibe";
const MAX_SCAN_DEPTH: u8 = 4;
/// Bound global history enumeration so one large Vibe profile cannot stall ingest.
const MAX_SESSION_FILES: usize = 512;

pub struct VibeSource {
    session_root: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
    /// Source-lifetime cache so one scan pass resolves git identity once per
    /// root/cwd instead of once per session directory.
    project_matchers: ProjectRootMatcherCache,
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
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }

    fn scoped_meta(&self, path: &Path, project_root: &Path) -> ScopedMeta {
        let Some(meta) = path
            .parent()
            .and_then(|session| read_meta(&session.join("meta.json")))
        else {
            return ScopedMeta::Undecided;
        };
        // `Unknown` (bounded git timeout) stays undecided: nothing is
        // recorded, so the next scan pass re-resolves the membership instead
        // of misfiling the session.
        match TranscriptScopeMatcher::for_scope_cached(
            project_root,
            self.user_registered_roots.as_deref(),
            &self.project_matchers,
        )
        .membership(Some(&meta.working_directory))
        {
            ProjectMembership::Match => ScopedMeta::InScope(meta),
            ProjectMembership::NoMatch => ScopedMeta::OutsideScope,
            ProjectMembership::Unknown => ScopedMeta::Undecided,
        }
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
}

enum ScopedMeta {
    InScope(VibeMeta),
    OutsideScope,
    Undecided,
}

/// The session this pass must admit, or `None` when its transcript already
/// converged, lies outside the scope, or cannot be scoped yet.
fn pending_session<'a>(
    source: &VibeSource,
    path: &Path,
    project_root: &Path,
    convergence: Option<(&'a CodexDiscoveryHub, &'a str)>,
) -> TranscriptIngestResult<Option<(PendingTranscript<'a>, VibeMeta)>> {
    let Some(pending) = PendingTranscript::observe_blocking(convergence, path)? else {
        return Ok(None);
    };
    match source.scoped_meta(path, project_root) {
        ScopedMeta::InScope(meta) => Ok(Some((pending, meta))),
        ScopedMeta::OutsideScope => pending.finished(path).map(|()| None),
        ScopedMeta::Undecided => Ok(None),
    }
}

#[tracing::instrument(name = "sessions.hosts.vibe.capture", level = "trace", skip_all)]
pub async fn capture_vibe_observations(
    facade: &dyn HostAdmission,
    source: &VibeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
    convergence: Option<(&CodexDiscoveryHub, &str)>,
) -> TranscriptIngestResult<VibeCaptureOutcome> {
    let discovery = {
        let _span = tracing::trace_span!("sessions.hosts.vibe.discover_blocking").entered();
        run_blocking_transcript_section(|| {
            source.discover_transcript_paths(
                project_root,
                TranscriptDiscoveryBounds::from_discovered_units(MAX_SESSION_FILES),
            )
        })
    };
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
        let Some((pending, meta)) = {
            let _span = tracing::trace_span!("sessions.hosts.vibe.meta_blocking").entered();
            run_blocking_transcript_section(|| {
                pending_session(source, &path, project_root, convergence)
            })
        }?
        else {
            continue;
        };
        let progress = capture_vibe_path(
            facade,
            &path,
            meta,
            scope.clone(),
            max_new_bytes.map(|_| remaining),
            cancellation,
        )
        .await?;
        pending.admitted(&path, progress.source_deferred, progress.covered_through)?;
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
    path: &Path,
    meta: VibeMeta,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<JsonlObservationAdmissionProgress> {
    let provider = ProviderId::new(PROVIDER)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let canonical_session_id = protect_sensitive_structural_id(&meta.session_id)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    let session_id = SessionId::new(&canonical_session_id)
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
    let native_session_id = meta.session_id;
    let model = meta.model;
    let location = meta.working_directory.to_string_lossy().into_owned();

    admit_jsonl_observations(
        request,
        |_| (),
        move |(), bytes, range, _, _prepared, _hints| {
            let native_record_id = vibe_capture::native_record_id(&native_session_id, range)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
            match parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    vibe_capture::normalize_observation(
                        &native,
                        &canonical_session_id,
                        model.as_deref(),
                        Some(location.as_str()),
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
    let files_considered = u64::try_from(paths.len())
        .unwrap_or(u64::MAX)
        .saturating_add(skipped_oversized_entries);

    crate::runtime::pipeline_metrics::record_discovery_files(
        files_considered,
        u64::try_from(paths.len()).unwrap_or(u64::MAX),
        bytes_charged,
    );
    crate::runtime::pipeline_metrics::record_sweep_outcome(truncated.is_none());
    FileDiscoveryReport {
        paths,
        truncated,
        skipped_oversized_entries,
        bytes_charged,
        files_considered,
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
        let name_bytes = crate::runtime::source::os_str_byte_len(&file_name);
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

#[cfg(test)]
mod tests;
