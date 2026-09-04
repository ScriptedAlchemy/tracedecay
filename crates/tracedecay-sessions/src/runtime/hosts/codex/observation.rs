//! Codex rollout observation admission and canonical-envelope normalization.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use tokio::sync::Notify;

pub(super) use tracedecay_capture::codex::codex_native_record_id;
#[cfg(test)]
pub use tracedecay_capture::codex::normalize_codex_observation;
use tracedecay_capture::codex::{
    CodexObservationLocation, codex_current_user_message, codex_message_visible_text,
    codex_observation_record_supported, codex_response_goal_context,
    normalize_codex_observation_with_location_and_pair,
};
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceIdentityV1, ProjectId, ProviderId, RetentionClass,
    SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use super::PROVIDER;
use super::context::CodexContextState;
use super::meta::{CodexMetaWithProvenance, session_meta_with_provenance};
use crate::admission::HostAdmission;
use crate::host_ports::unregistered_admission;
use crate::observation::ObservationCancellation;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, PersistedCursorUpdate,
    SharedJsonlFileIdentity, admit_jsonl_observations, reserve_shared_jsonl_page,
    shared_jsonl_background_cpu, shared_jsonl_file_identity, shared_jsonl_preparation_capacity,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, normalize_prepared_observation_record_v1,
};

const CODEX_OBSERVATION_RETENTION: &str = "retention.provider-observation";
pub const CODEX_HOOK_MAX_NEW_BYTES: u64 = crate::runtime::source::MAX_JSONL_RECORD_BYTES as u64;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodexMetaCacheKey {
    path: PathBuf,
    identity: SharedJsonlFileIdentity,
}

struct CachedCodexMeta {
    key: CodexMetaCacheKey,
    meta: Arc<CodexMetaWithProvenance>,
    _memory: Option<tracedecay_runtime_core::resident_memory::ProcessSharedMemoryReservationV1>,
}

#[derive(Default)]
struct CodexMetaCache {
    entries: VecDeque<CachedCodexMeta>,
    in_flight: HashMap<CodexMetaCacheKey, Arc<Notify>>,
}

static CODEX_META_CACHE: OnceLock<tokio::sync::Mutex<CodexMetaCache>> = OnceLock::new();

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexJsonlAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
    pub frames_decoded: u64,
    pub frames_accepted: u64,
    pub frames_skipped: u64,
    /// Of `frames_skipped`, the frames refused from their raw bytes before any
    /// decode. Every one is a rollout record this scope never owned.
    pub frames_rejected_before_decode: u64,
    pub frames_refused: u64,
    pub frames_persisted: u64,
}

/// Admit a Codex rollout for one exact project identity.
///
/// The scheduler supplies the already-resolved project id; each complete record
/// is routed by the rollout's current Codex cwd, including context reconstructed
/// before a resumed byte cursor.
pub async fn try_admit_codex_jsonl_observations_for_project(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let Some(admission) =
        unregistered_admission::create(unregistered_admission::Scope::Project(project_id.clone()))
    else {
        return Ok(CodexJsonlAdmissionProgress::default());
    };
    try_admit_codex_jsonl_observations_for_project_with_admission(
        path,
        project_root,
        project_id,
        admission.as_ref(),
        max_new_bytes,
    )
    .await
}

/// Project admission with authority prepared by the caller.
///
/// The project scheduler constructs this facade from its authoritative project
/// identity and may attach additional admission evidence before source routing.
pub async fn try_admit_codex_jsonl_observations_for_project_with_admission(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
        path,
        project_root,
        project_id,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Project {
            root: project_root,
            project_id,
        },
        admission,
        max_new_bytes,
        cancellation,
    )
    .await
}

/// Admit Codex records that are not attributable to any registered project.
///
/// A scheduler may constrain this pass to one session while it catches up a
/// profile-owned rollout.
pub async fn try_admit_codex_jsonl_observations_for_profile(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let Some(admission) = unregistered_admission::create(unregistered_admission::Scope::Profile)
    else {
        return Ok(CodexJsonlAdmissionProgress::default());
    };
    try_admit_codex_jsonl_observations_for_profile_with_admission(
        path,
        session_id,
        registered_roots,
        admission.as_ref(),
        max_new_bytes,
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
        path,
        session_id,
        registered_roots,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Profile {
            session_id,
            registered_roots,
        },
        admission,
        max_new_bytes,
        cancellation,
    )
    .await
}

pub(super) enum CodexObservationAdmission<'a> {
    Project {
        root: &'a Path,
        project_id: ProjectId,
    },
    Profile {
        session_id: Option<&'a str>,
        registered_roots: &'a [PathBuf],
    },
}

impl CodexObservationAdmission<'_> {
    pub(super) fn scope(&self) -> ObservationScopeV1 {
        match self {
            Self::Project { project_id, .. } => ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            Self::Profile { .. } => ObservationScopeV1::Profile,
        }
    }

    /// Resolve this rollout's scope boundary once, so the per-record admission
    /// test does not re-resolve the git identity of an unchanging root.
    pub(super) fn scope_matcher(&self) -> TranscriptScopeMatcher {
        match self {
            Self::Project { root, .. } => TranscriptScopeMatcher::project(root),
            Self::Profile {
                registered_roots, ..
            } => TranscriptScopeMatcher::profile(registered_roots),
        }
    }

    pub(super) fn accepts_session(&self, session_id: &str) -> bool {
        !matches!(self, Self::Profile { session_id: Some(expected), .. } if *expected != session_id)
    }

    fn projection_project_path<'b>(&'b self, cwd: Option<&'b Path>) -> Option<&'b Path> {
        match self {
            Self::Project { root, .. } => Some(root),
            Self::Profile { .. } => cwd,
        }
    }
}

#[derive(Clone)]
struct CodexAdmissionState {
    context: CodexContextState,
    scope_verdict: Option<bool>,
    paired_response_goal_ids: Arc<HashSet<String>>,
    paired_current_goal_parents: Arc<HashMap<String, tracedecay_domain::ObservationId>>,
}

fn goal_context_identity(text: &str) -> Option<[u8; 32]> {
    tracedecay_store::codex_goal_context_from_text(text)?.correlation_identity()
}

struct CodexGoalPagePairs {
    response_ids: HashSet<String>,
    current_parents: HashMap<String, tracedecay_domain::ObservationId>,
}

fn goal_page_pairs(
    scan: &crate::runtime::jsonl_observation_admission::JsonlObservationScan<'_>,
    mut context: CodexContextState,
    path: &Path,
    meta: &super::meta::CodexMeta,
    scope_matcher: &TranscriptScopeMatcher,
) -> CodexGoalPagePairs {
    let mut scope_verdict = None;
    let mut pending_responses = Vec::<([u8; 32], String, tracedecay_domain::ObservationId)>::new();
    let mut pairs = CodexGoalPagePairs {
        response_ids: HashSet::new(),
        current_parents: HashMap::new(),
    };
    for bytes in scan.frame_bytes() {
        // Decode only records that can change scope or carry the paired
        // current item. Any escape stays eligible because JSON may encode
        // these discriminators with Unicode escapes.
        const CANDIDATES: [&[u8]; 4] = [
            b"session_meta",
            b"turn_context",
            b"response_item",
            b"UserMessage",
        ];
        if !bytes.contains(&b'\\')
            && !CANDIDATES.iter().any(|candidate| {
                bytes
                    .windows(candidate.len())
                    .any(|window| window == *candidate)
            })
        {
            continue;
        }
        let Ok(native) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            continue;
        };
        if context.observe_context_record(&native, path, meta) {
            scope_verdict = None;
            continue;
        }
        if !*scope_verdict.get_or_insert_with(|| scope_matcher.accepts(context.cwd.as_deref())) {
            continue;
        }
        let payload = native.get("payload").unwrap_or(&native);
        match native.get("type").and_then(serde_json::Value::as_str) {
            Some("response_item") => {
                let Some(goal) = codex_response_goal_context(payload) else {
                    continue;
                };
                let Some(identity) = goal_context_identity(&goal.visible_text) else {
                    continue;
                };
                let Ok(record_id) = codex_native_record_id(&meta.session_id, &native) else {
                    continue;
                };
                let response_message_id = goal
                    .item_id
                    .and_then(|item_id| tracedecay_domain::ObservationId::new(item_id).ok())
                    .unwrap_or_else(|| record_id.clone());
                pending_responses.push((
                    identity,
                    record_id.as_str().to_owned(),
                    response_message_id,
                ));
            }
            Some("event_msg") => {
                let Some(message) = codex_current_user_message(payload) else {
                    continue;
                };
                let visible_text = codex_message_visible_text(message.content);
                let Some(identity) = goal_context_identity(&visible_text) else {
                    continue;
                };
                let Some(index) = pending_responses
                    .iter()
                    .rposition(|(pending, _, _)| *pending == identity)
                else {
                    continue;
                };
                let (_, response_record_id, response_message_id) = pending_responses.remove(index);
                let Ok(current_id) = codex_native_record_id(&meta.session_id, &native) else {
                    continue;
                };
                pairs.response_ids.insert(response_record_id);
                pairs
                    .current_parents
                    .insert(current_id.as_str().to_owned(), response_message_id);
            }
            _ => {}
        }
    }
    pairs
}

async fn shared_session_meta_with_provenance(
    path: &Path,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<Arc<CodexMetaWithProvenance>> {
    let identity_path = path.to_path_buf();
    let key = tokio::task::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(&identity_path).map_err(|source| {
            TranscriptIngestError::ScanIo {
                operation: "resolve Codex session metadata identity",
                path: identity_path.clone(),
                source,
            }
        })?;
        let identity = shared_jsonl_file_identity(&canonical)?;
        Ok::<_, TranscriptIngestError>(CodexMetaCacheKey {
            path: canonical,
            identity,
        })
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    let cache_lock = CODEX_META_CACHE.get_or_init(tokio::sync::Mutex::default);
    loop {
        let mut cache = cache_lock.lock().await;
        if let Some(index) = cache.entries.iter().position(|entry| entry.key == key) {
            let entry = cache
                .entries
                .remove(index)
                .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
            let meta = Arc::clone(&entry.meta);
            cache.entries.push_back(entry);
            hotpath::gauge!("codex_shared_meta_hits").inc(1.0);
            return Ok(meta);
        }
        if let Some(notify) = cache.in_flight.get(&key) {
            let notified = Arc::clone(notify).notified_owned();
            drop(cache);
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    if cancellation.is_cancelled() {
                        return Err(TranscriptIngestError::Cancelled { provider: PROVIDER });
                    }
                }
            }
            continue;
        }
        cache.in_flight.insert(key.clone(), Arc::new(Notify::new()));
        hotpath::gauge!("codex_shared_meta_misses").inc(1.0);
        break;
    }

    let parse_path = key.path.clone();
    let error_path = path.to_path_buf();
    let build_cancellation = cancellation.clone();
    let build = async move {
        if build_cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider: PROVIDER });
        }
        let mut memory = reserve_shared_jsonl_page()?;
        let background_cpu = shared_jsonl_background_cpu()?;
        let parsed = tokio::task::spawn_blocking(move || {
            background_cpu.with_permit(|| session_meta_with_provenance(&parse_path))
        })
        .await
        .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })?
        .ok_or(TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: error_path,
        })?;
        let parsed = Arc::new(parsed);
        if let Some(reservation) = &mut memory {
            reservation
                .shrink_to(parsed.retained_bytes())
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
        }
        Ok::<_, TranscriptIngestError>((parsed, memory))
    }
    .await;
    let mut cache = cache_lock.lock().await;
    let notify = cache.in_flight.remove(&key);
    let (parsed, memory) = match build {
        Ok(built) => built,
        Err(error) => {
            drop(cache);
            if let Some(notify) = notify {
                notify.notify_waiters();
            }
            return Err(error);
        }
    };
    while cache.entries.len() >= shared_jsonl_preparation_capacity() {
        cache.entries.pop_front();
    }
    cache.entries.push_back(CachedCodexMeta {
        key,
        meta: Arc::clone(&parsed),
        _memory: memory,
    });
    drop(cache);
    if let Some(notify) = notify {
        notify.notify_waiters();
    }
    Ok(parsed)
}

async fn try_admit_codex_jsonl_observations(
    path: &Path,
    admission_scope: CodexObservationAdmission<'_>,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    if cancellation.is_cancelled() {
        return Ok(CodexJsonlAdmissionProgress {
            bytes_consumed: 0,
            source_deferred: true,
            ..CodexJsonlAdmissionProgress::default()
        });
    }
    let parsed_meta = shared_session_meta_with_provenance(path, cancellation).await?;
    let native_thread_id = parsed_meta.native_thread_id.clone();
    let meta = parsed_meta.meta.clone();
    if !admission_scope.accepts_session(&meta.session_id) {
        return Ok(CodexJsonlAdmissionProgress::default());
    }
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)?,
        SessionId::new(meta.session_id.clone())?,
    )?;
    let scope = admission_scope.scope();
    let scope_matcher = admission_scope.scope_matcher();
    let source_location_path = admission_scope.projection_project_path(Some(meta.cwd.as_path()));
    let source_location = CodexObservationLocation {
        project_path: source_location_path,
        location_path: source_location_path,
    };
    let request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        admission,
        source,
        scope,
        RetentionClass::new(CODEX_OBSERVATION_RETENTION)?,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_persisted_cursor_update(PersistedCursorUpdate::Replace)
    .with_lazy_shared_frame_preparation()
    .with_cancellation(cancellation.clone());
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            let context = if scan.resumed {
                CodexContextState::scan_prior(path, scan.start_offset, &meta)
            } else {
                CodexContextState::from_meta(&meta)
            };
            let pairs = goal_page_pairs(&scan, context.clone(), path, &meta, &scope_matcher);
            CodexAdmissionState {
                paired_response_goal_ids: Arc::new(pairs.response_ids),
                paired_current_goal_parents: Arc::new(pairs.current_parents),
                context,
                scope_verdict: None,
            }
        },
        |state, _bytes, range, _, prepared, hints| {
            let mut stable_record_id = None;
            let mut non_durable_reason = None;
            // Scope is consulted before the record is decoded, not after. A
            // rollout that belongs to another project answers the same verdict
            // for every one of its frames, and the old order paid a full JSON
            // decode per frame to reach it.
            let in_scope = *state
                .scope_verdict
                .get_or_insert_with(|| scope_matcher.accepts(state.context.cwd.as_deref()));
            if !in_scope && !hints.may_change_codex_context {
                return Ok(JsonlFrameAdmission::non_durable_before_decode(
                    ObservationCoverageReason::OutOfScope,
                ));
            }
            let Some(prepared) = prepared else {
                return Ok(JsonlFrameAdmission::needs_preparation());
            };
            let parsed = normalize_prepared_observation_record_v1(prepared, |native| {
                if state.context.observe_context_record(native, path, &meta) {
                    // Only a context record can move the rollout's cwd,
                    // so the memoized verdict is dropped exactly when it
                    // can no longer be trusted.
                    state.scope_verdict = None;
                }
                if !*state
                    .scope_verdict
                    .get_or_insert_with(|| scope_matcher.accepts(state.context.cwd.as_deref()))
                {
                    non_durable_reason = Some(ObservationCoverageReason::OutOfScope);
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                if !codex_observation_record_supported(native) {
                    non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                let record_id = codex_native_record_id(&meta.session_id, native)
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                if native.get("type").and_then(serde_json::Value::as_str) == Some("response_item")
                    && state.paired_response_goal_ids.contains(record_id.as_str())
                {
                    non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                let paired_response_id = state
                    .paired_current_goal_parents
                    .get(record_id.as_str())
                    .cloned();
                let envelope = normalize_codex_observation_with_location_and_pair(
                    native,
                    &meta.session_id,
                    native_thread_id.as_deref(),
                    record_id.clone(),
                    range,
                    source_location,
                    paired_response_id,
                )?;
                stable_record_id = Some(record_id);
                Ok(envelope)
            });
            match parsed {
                Ok(parsed) => Ok(JsonlFrameAdmission::durable(
                    parsed,
                    stable_record_id
                        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?,
                )),
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    non_durable_reason.unwrap_or(ObservationCoverageReason::MalformedFrame),
                )),
            }
        },
    )
    .await?;
    Ok(CodexJsonlAdmissionProgress {
        bytes_consumed: progress.bytes_consumed,
        source_deferred: progress.source_deferred,
        frames_decoded: progress.frames_decoded,
        frames_accepted: progress.frames_accepted,
        frames_skipped: progress.frames_skipped,
        frames_rejected_before_decode: progress.frames_rejected_before_decode,
        frames_refused: progress.frames_refused,
        frames_persisted: progress.frames_persisted,
    })
}
