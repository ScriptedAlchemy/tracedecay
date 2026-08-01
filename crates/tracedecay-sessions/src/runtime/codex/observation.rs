//! Codex rollout observation admission and canonical-envelope normalization.

use std::path::{Path, PathBuf};

pub(super) use tracedecay_capture::codex::codex_native_record_id;
#[cfg(test)]
pub use tracedecay_capture::codex::normalize_codex_observation;
use tracedecay_capture::codex::{
    CodexObservationLocation, codex_observation_record_supported,
    normalize_codex_observation_with_location,
};
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use super::PROVIDER;
use super::context::CodexContextState;
use super::meta::session_meta_with_provenance;
use crate::admission::HostAdmission;
use crate::host_ports::unregistered_admission;
use crate::observation::ObservationCancellation;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, PersistedCursorUpdate,
    admit_jsonl_observations,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
};

const CODEX_OBSERVATION_RETENTION: &str = "retention.provider-observation";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexJsonlAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
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
        });
    }
    let parsed_meta = session_meta_with_provenance(path).ok_or_else(|| {
        TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        }
    })?;
    let native_thread_id = parsed_meta.native_thread_id;
    let meta = parsed_meta.meta;
    if !admission_scope.accepts_session(&meta.session_id) {
        return Ok(CodexJsonlAdmissionProgress::default());
    }
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)?,
        SessionId::new(meta.session_id.clone())?,
    )?;
    let scope = admission_scope.scope();
    let scope_matcher = admission_scope.scope_matcher();
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
    .with_cancellation(cancellation.clone());
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            if scan.resumed {
                CodexContextState::scan_prior(path, scan.start_offset, &meta)
            } else {
                CodexContextState::from_meta(&meta)
            }
        },
        |context_state, bytes, range, _| {
            let mut stable_record_id = None;
            let mut non_durable_reason = None;
            let parsed = parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    context_state.observe_context_record(&native, path, &meta);
                    if !scope_matcher.accepts(context_state.cwd.as_deref()) {
                        non_durable_reason = Some(ObservationCoverageReason::OutOfScope);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    if !codex_observation_record_supported(&native) {
                        non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    let record_id = codex_native_record_id(&meta.session_id, &native)
                        .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                    let envelope = normalize_codex_observation_with_location(
                        &native,
                        &meta.session_id,
                        native_thread_id.as_deref(),
                        record_id.clone(),
                        range,
                        CodexObservationLocation {
                            project_path: admission_scope
                                .projection_project_path(context_state.cwd.as_deref()),
                            location_path: context_state.cwd.as_deref(),
                            transcript_path: path,
                        },
                    )?;
                    stable_record_id = Some(record_id);
                    Ok(envelope)
                },
            );
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
    })
}
