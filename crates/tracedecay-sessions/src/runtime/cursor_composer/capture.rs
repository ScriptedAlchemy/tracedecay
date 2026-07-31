//! Capture-request construction for Cursor composer bubble and envelope
//! observations (identity, checkpoints, and admission).

use serde_json::Value;
pub use tracedecay_capture::cursor_composer::{
    composer_envelope_todo_checkpoint, cursor_composer_envelope_native_record_id,
    cursor_composer_envelope_source, cursor_composer_native_record_id,
};
use tracedecay_capture::cursor_composer::{
    composer_observation_with_session, normalize_cursor_composer_envelope_observation,
    normalize_cursor_composer_observation_with_message_id,
};
use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ProviderId, RetentionClass, SessionId,
};

use crate::application::host_admission::HostAdmissionFacade;
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::privacy::parse_normalized_observation_record_v1;
use crate::runtime::source::TranscriptIngestError;

use super::PROVIDER;

const COMPOSER_OBSERVATION_RETENTION: &str = "retention.provider-observation";

#[allow(clippy::too_many_arguments)]
pub fn build_cursor_composer_capture_request_for_project(
    composer_id: &str,
    bubble_id: &str,
    bubble: &Value,
    project_path: Option<&str>,
    envelope: Option<&Value>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: &ObservationCancellation,
) -> Result<CaptureObservationRequest, String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer position: {error}"))?;
    let native = composer_observation_with_session(bubble, project_path, envelope);
    let encoded = serde_json::to_vec(&native)
        .map_err(|error| format!("could not encode Cursor composer bubble: {error}"))?;
    let native_record_id = cursor_composer_native_record_id(composer_id, bubble_id)?;
    let projected_message_id = ObservationId::new(format!("{composer_id}:{bubble_id}"))
        .map_err(|error| format!("invalid Cursor composer V1 message identity: {error}"))?;
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            normalize_cursor_composer_observation_with_message_id(
                &native,
                composer_id,
                native_record_id.clone(),
                projected_message_id.clone(),
                range,
                position,
            )
        },
    )
    .map_err(|error| format!("could not parse Cursor composer bubble: {error}"))?;
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)
            .map_err(|error| format!("invalid Cursor provider id: {error}"))?,
        SessionId::new(composer_id)
            .map_err(|error| format!("invalid Cursor composer id: {error}"))?,
    )
    .map_err(|error| format!("invalid Cursor composer source: {error}"))?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        native_record_id,
    )
    .map_err(|error| format!("invalid Cursor composer identity: {error}"))?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(COMPOSER_OBSERVATION_RETENTION)
            .map_err(|error| format!("invalid Cursor composer retention: {error}"))?,
        cancellation.clone(),
    )
    .map_err(|error| format!("invalid Cursor composer capture request: {error}"))
}

pub fn build_cursor_composer_capture_request(
    composer_id: &str,
    bubble_id: &str,
    bubble: &Value,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    build_cursor_composer_capture_request_for_project(
        composer_id,
        bubble_id,
        bubble,
        None,
        None,
        scope,
        generation,
        position,
        expected_cursor,
        &ObservationCancellation::default(),
    )
}

pub async fn capture_cursor_composer_observation(
    admission: &HostAdmissionFacade<'_>,
    request: CaptureObservationRequest,
) -> Result<CaptureObservationOutcome, TranscriptIngestError> {
    admission
        .capture_observation(request)
        .await
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
}

pub fn build_cursor_composer_envelope_capture_request_for_project(
    composer_id: &str,
    envelope: &Value,
    project_path: Option<&str>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: &ObservationCancellation,
) -> Result<CaptureObservationRequest, String> {
    let position = expected_cursor
        .as_ref()
        .filter(|cursor| cursor.generation() == generation)
        .map_or(0, ObservationSourceCursorV1::position);
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer envelope position: {error}"))?;
    let checkpoint = composer_envelope_todo_checkpoint(envelope)
        .ok_or_else(|| "Cursor composer envelope has no admittable todo checkpoint".to_string())?;
    let encoded = serde_json::to_vec(envelope)
        .map_err(|error| format!("could not encode Cursor composer envelope: {error}"))?;
    let native_record_id = cursor_composer_envelope_native_record_id(composer_id, checkpoint)?;
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            normalize_cursor_composer_envelope_observation(
                &native,
                composer_id,
                project_path,
                native_record_id.clone(),
                range,
                position,
            )
        },
    )
    .map_err(|error| format!("could not parse Cursor composer envelope: {error}"))?;
    let source = cursor_composer_envelope_source(composer_id)?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        native_record_id,
    )
    .map_err(|error| format!("invalid Cursor composer envelope identity: {error}"))?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(COMPOSER_OBSERVATION_RETENTION)
            .map_err(|error| format!("invalid Cursor composer retention: {error}"))?,
        cancellation.clone(),
    )
    .map_err(|error| format!("invalid Cursor composer envelope capture request: {error}"))
    .map(|request| request.with_resume_checkpoint(generation.file_id(), checkpoint))
}

/// Capture a `composerData:<composerId>` envelope observation when native
/// `todos[{id,content,status}]` are present. Uses a distinct `source_key` so the
/// bubble cursor stream is not displaced by envelope list updates.
///
/// Envelope native identity includes a todo checkpoint (authentic
/// `lastUpdatedAt` when present, otherwise a deterministic content fingerprint
/// over native todo id/content/status/order) so pending→completed and content
/// or order revisions admit as new observations without inventing
/// `WorkflowLifecycle.revision`.
pub fn build_cursor_composer_envelope_capture_request(
    composer_id: &str,
    envelope: &Value,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    build_cursor_composer_envelope_capture_request_for_project(
        composer_id,
        envelope,
        None,
        scope,
        generation,
        expected_cursor,
        &ObservationCancellation::default(),
    )
}
