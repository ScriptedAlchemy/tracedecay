//! Codex rollout observation admission and canonical-envelope normalization.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use tokio::sync::Notify;

pub(super) use tracedecay_capture::codex::codex_native_record_id;
#[cfg(test)]
pub use tracedecay_capture::codex::normalize_codex_observation;
use tracedecay_capture::codex::{
    CodexObservationLocation, codex_current_user_message, codex_observation_record_supported,
    normalize_codex_observation_with_location,
};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
    ObservationScopeV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, RetentionClass, SessionId,
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
use crate::runtime::shared::{StoredCursor, TranscriptScopeMatcher};
use crate::runtime::source::{
    JsonlResumeState, MAX_JSONL_RECORD_BYTES, TranscriptIngestError, TranscriptIngestResult,
    try_stream_new_jsonl_raw_strict_with_resume,
};
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
    replay_through: Option<u64>,
}

#[derive(Clone)]
enum CodexAdmissionMode {
    Ordinary,
    CurrentUserMessageReplay {
        expected_start_cursor: Option<tracedecay_domain::ObservationSourceCursorV1>,
        generation: u64,
        through: u64,
    },
}

impl CodexAdmissionMode {
    fn replay_through(&self, scan_generation: u64) -> Option<u64> {
        match *self {
            Self::CurrentUserMessageReplay {
                generation,
                through,
                ..
            } if generation == scan_generation => Some(through),
            Self::Ordinary | Self::CurrentUserMessageReplay { .. } => None,
        }
    }

    fn replay_window(&self) -> Option<(Option<tracedecay_domain::ObservationSourceCursorV1>, u64)> {
        match self {
            Self::CurrentUserMessageReplay {
                expected_start_cursor,
                through,
                ..
            } => Some((expected_start_cursor.clone(), *through)),
            Self::Ordinary => None,
        }
    }
}

#[derive(Clone, Copy)]
struct CodexAdmissionContext<'a> {
    path: &'a Path,
    scope: &'a CodexObservationAdmission<'a>,
    admission: &'a dyn HostAdmission,
    meta: &'a super::meta::CodexMeta,
    native_thread_id: Option<&'a str>,
    cancellation: &'a ObservationCancellation,
}

fn replay_identity(session_id: &str, domain: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    format!("sha256:{}", encode_lowercase_hex(&hasher.finalize()))
}

pub(crate) fn codex_observation_source_v2(
    session_id: &str,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    Ok(ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new(PROVIDER)?,
        SessionId::new(session_id.to_string())?,
        SessionId::new(replay_identity(
            session_id,
            b"tracedecay.codex.observation-source.v2",
        ))?,
    )?)
}

async fn replay_advanced_current_user_messages(
    context: CodexAdmissionContext<'_>,
    ordinary_source: &ObservationSourceIdentityV1,
    canonical_source: &ObservationSourceIdentityV1,
    target: &tracedecay_domain::ObservationSourceCursorV1,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<Option<CodexJsonlAdmissionProgress>> {
    let CodexAdmissionContext {
        scope: admission_scope,
        admission,
        ..
    } = context;
    let scope = admission_scope.scope();
    let replay_cursor = admission
        .get_source_cursor(canonical_source, &scope)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
        })?;
    let replay_position = match replay_cursor.as_ref() {
        Some(cursor) if cursor.generation() == target.generation() => cursor.position(),
        Some(_) => return Ok(None),
        None => 0,
    };
    let remaining = target.position().saturating_sub(replay_position);
    if remaining == 0 {
        return Ok(None);
    }
    let replay_limit = max_new_bytes
        .unwrap_or(CODEX_HOOK_MAX_NEW_BYTES)
        .min(CODEX_HOOK_MAX_NEW_BYTES)
        .min(remaining);
    let mut progress = admit_codex_jsonl_page(
        context,
        canonical_source.clone(),
        Some((ordinary_source, target.generation())),
        Some(replay_limit),
        CodexAdmissionMode::CurrentUserMessageReplay {
            expected_start_cursor: replay_cursor,
            generation: target.generation().generation_id(),
            through: target.position(),
        },
    )
    .await?;
    // This invocation was reserved for historical catch-up. Ordinary
    // admission resumes on the next bounded scheduler pass.
    progress.source_deferred = true;
    Ok(Some(progress))
}

async fn legacy_cursor_matches_current_file(
    path: &Path,
    target: &tracedecay_domain::ObservationSourceCursorV1,
) -> TranscriptIngestResult<bool> {
    if target.position() == 0 {
        return Ok(true);
    }
    let (Some(file_identity), Some(fingerprint)) =
        (target.file_identity(), target.resume_fingerprint())
    else {
        return Ok(false);
    };
    let generation = target.generation().generation_id();
    let position = target.position();
    let path = path.to_path_buf();
    let scan = tokio::task::spawn_blocking(move || {
        try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor {
                position,
                mtime: 0,
                file_id: generation,
            },
            Some(0),
            MAX_JSONL_RECORD_BYTES,
            Some(JsonlResumeState {
                generation,
                file_identity,
                fingerprint,
            }),
        )
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    Ok(scan.start_offset == position
        && scan.new_cursor.file_id == generation
        && !scan.replacement_generation)
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
    let ordinary_source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)?,
        SessionId::new(meta.session_id.clone())?,
    )?;
    let canonical_source = codex_observation_source_v2(&meta.session_id)?;
    let context = CodexAdmissionContext {
        path,
        scope: &admission_scope,
        admission,
        meta: &meta,
        native_thread_id: native_thread_id.as_deref(),
        cancellation,
    };
    let scope = admission_scope.scope();
    if let Some(target) = admission
        .get_source_cursor(&ordinary_source, &scope)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
        })?
    {
        let canonical_cursor = admission
            .get_source_cursor(&canonical_source, &scope)
            .await
            .map_err(|outcome| {
                crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
            })?;
        let replay_is_pending = canonical_cursor.as_ref().is_none_or(|cursor| {
            cursor.generation() == target.generation() && cursor.position() < target.position()
        });
        if replay_is_pending
            && legacy_cursor_matches_current_file(path, &target).await?
            && let Some(progress) = replay_advanced_current_user_messages(
                context,
                &ordinary_source,
                &canonical_source,
                &target,
                max_new_bytes,
            )
            .await?
        {
            return Ok(progress);
        }
    }
    admit_codex_jsonl_page(
        context,
        canonical_source,
        None,
        max_new_bytes,
        CodexAdmissionMode::Ordinary,
    )
    .await
}

async fn admit_codex_jsonl_page(
    context: CodexAdmissionContext<'_>,
    source: ObservationSourceIdentityV1,
    ordinary_identity: Option<(&ObservationSourceIdentityV1, ObservationSourceGenerationV1)>,
    max_new_bytes: Option<u64>,
    mode: CodexAdmissionMode,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let CodexAdmissionContext {
        path,
        scope: admission_scope,
        admission,
        meta,
        native_thread_id,
        cancellation,
    } = context;
    let scope = admission_scope.scope();
    let scope_matcher = admission_scope.scope_matcher();
    let source_location_path = admission_scope.projection_project_path(Some(meta.cwd.as_path()));
    let source_location = CodexObservationLocation {
        project_path: source_location_path,
        location_path: source_location_path,
    };
    let mut request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        admission,
        source,
        scope.clone(),
        RetentionClass::new(CODEX_OBSERVATION_RETENTION)?,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_persisted_cursor_update(PersistedCursorUpdate::Replace)
    .with_lazy_shared_frame_preparation()
    .with_cancellation(cancellation.clone());
    if let Some((expected_start_cursor, through)) = mode.replay_window() {
        request = request
            .with_required_start_cursor(expected_start_cursor)
            .with_max_end_offset(through);
    }
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            debug_assert!(
                scan.frame_bytes().all(|frame| !frame.is_empty()),
                "shared JSONL admission never exposes empty native frames"
            );
            let context = if scan.resumed {
                CodexContextState::scan_prior(path, scan.start_offset, meta)
            } else {
                CodexContextState::from_meta(meta)
            };
            CodexAdmissionState {
                context,
                scope_verdict: None,
                replay_through: mode.replay_through(scan.generation),
            }
        },
        |state, _bytes, range, _, prepared, hints| {
            let mut stable_record_id = None;
            let mut ordinary_observation_id = None;
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
                if state.context.observe_context_record(native, path, meta) {
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
                let replaying_frame = state
                    .replay_through
                    .is_some_and(|through| range.end() <= through);
                if replaying_frame {
                    let payload = native.get("payload").unwrap_or(native);
                    if codex_current_user_message(payload).is_none() {
                        non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                }
                let record_id = codex_native_record_id(&meta.session_id, native)
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                if replaying_frame {
                    let (ordinary_source, ordinary_generation) = ordinary_identity
                        .ok_or(ObservationRecordParseErrorV1::NormalizationFailed)?;
                    let identity = ObservationIdentityMaterialV1::for_native_record(
                        ordinary_source.clone(),
                        scope.clone(),
                        ordinary_generation,
                        range,
                        ObservationOrderingDomainV1::FileBytes,
                        record_id.clone(),
                    )
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                    ordinary_observation_id = Some(
                        CanonicalObservationIdV1::derive(&identity)
                            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
                    );
                }
                let envelope = normalize_codex_observation_with_location(
                    native,
                    &meta.session_id,
                    native_thread_id,
                    record_id.clone(),
                    range,
                    source_location,
                )?;
                stable_record_id = Some(record_id);
                Ok(envelope)
            });
            match parsed {
                Ok(parsed) => {
                    let record_id = stable_record_id
                        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
                    if let Some(observation_id) = ordinary_observation_id {
                        Ok(JsonlFrameAdmission::durable_unless_observation_exists(
                            parsed,
                            record_id,
                            observation_id,
                        ))
                    } else {
                        Ok(JsonlFrameAdmission::durable(parsed, record_id))
                    }
                }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod replay_boundary_tests {
    use std::io::Write as _;

    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_domain::{CanonicalObservationEnvelopeV1, CanonicalObservationFactV1};

    use super::*;
    use crate::admission::test_support::MemoryHostAdmission;

    #[tokio::test]
    async fn stale_replay_stops_when_peer_and_legacy_writer_advance() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let path = tmp.path().join("rollout.jsonl");
        let session_id = "peer-winner-session";
        let lines = [
            json!({
                "timestamp": "2026-09-04T12:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": session_id, "cwd": project}
            }),
            json!({
                "timestamp": "2026-09-04T12:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "item": {
                        "type": "UserMessage",
                        "id": "peer-winner-item",
                        "content": [{"type": "text", "text": "Admit before peer race."}]
                    }
                }
            }),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let admission = MemoryHostAdmission::default();
        let project_id = ProjectId::new("project.peer-winner").unwrap();
        let canonical_source = codex_observation_source_v2(session_id).unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let stale_start_cursor = admission
            .get_source_cursor(&canonical_source, &scope)
            .await
            .unwrap();
        assert!(stale_start_cursor.is_none());
        try_admit_codex_jsonl_observations_for_project_with_admission(
            &path,
            &project,
            project_id.clone(),
            &admission,
            None,
        )
        .await
        .unwrap();
        let target = admission
            .get_source_cursor(&canonical_source, &scope)
            .await
            .unwrap()
            .expect("peer winner cursor");
        let usage = json!({
            "timestamp": "2026-09-04T12:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"last_token_usage": {
                    "input_tokens": 17,
                    "output_tokens": 3,
                    "cached_input_tokens": 2,
                    "reasoning_output_tokens": 1,
                    "total_tokens": 20
                }}
            }
        });
        let usage_line = usage.to_string() + "\n";
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(usage_line.as_bytes()).unwrap();
        drop(file);

        let cancellation = ObservationCancellation::default();
        let parsed_meta = shared_session_meta_with_provenance(&path, &cancellation)
            .await
            .unwrap();
        let admission_scope = CodexObservationAdmission::Project {
            root: &project,
            project_id: project_id.clone(),
        };
        let ordinary_source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new(PROVIDER).unwrap(),
            SessionId::new(session_id).unwrap(),
        )
        .unwrap();
        let context = CodexAdmissionContext {
            path: &path,
            scope: &admission_scope,
            admission: &admission,
            meta: &parsed_meta.meta,
            native_thread_id: parsed_meta.native_thread_id.as_deref(),
            cancellation: &cancellation,
        };
        let old_writer = admit_codex_jsonl_page(
            context,
            ordinary_source.clone(),
            None,
            None,
            CodexAdmissionMode::Ordinary,
        )
        .await
        .unwrap();
        assert_eq!(old_writer.frames_persisted, 3);
        let usage_count = || {
            admission
                .observations()
                .iter()
                .filter(|stored| {
                    serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                        stored.observation().payload().clone(),
                    )
                    .is_ok_and(|envelope| {
                        envelope.facts().iter().any(|fact| {
                            matches!(fact, CanonicalObservationFactV1::ProviderUsage { .. })
                        })
                    })
                })
                .count()
        };
        assert_eq!(usage_count(), 1);

        let stale_replay = admit_codex_jsonl_page(
            context,
            canonical_source.clone(),
            Some((&ordinary_source, target.generation())),
            Some(usage_line.len() as u64),
            CodexAdmissionMode::CurrentUserMessageReplay {
                expected_start_cursor: stale_start_cursor,
                generation: target.generation().generation_id(),
                through: target.position(),
            },
        )
        .await
        .unwrap();
        assert!(stale_replay.source_deferred);
        assert_eq!(stale_replay.bytes_consumed, 0);
        assert_eq!(stale_replay.frames_persisted, 0);

        let refreshed = try_admit_codex_jsonl_observations_for_project_with_admission(
            &path, &project, project_id, &admission, None,
        )
        .await
        .unwrap();
        assert!(refreshed.source_deferred);
        assert_eq!(usage_count(), 1);
    }
}
