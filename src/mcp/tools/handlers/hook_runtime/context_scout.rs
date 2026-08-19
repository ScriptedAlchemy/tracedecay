use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use tracedecay_agent_hosts::automation::config_error;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId, UtcMicros,
};
use tracedecay_store::{ObservationPersistOutcome, StoreShardScopeV1};
use tracedecay_usecases::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_usecases::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

use super::admission::{
    HookV2BindingAdmission, hook_v2_binding_admission, hook_v2_catchup_response,
};
use super::envelope::{hook_now, hook_v2_envelope, hook_v2_native_session_id};
use super::required_value;

async fn hook_v2_context_scout_lifecycle(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1> {
    hook_v2_context_scout_lifecycle_for_session(envelope, hook_v2_native_session_id(args, envelope))
        .await
}

pub(super) async fn hook_v2_context_scout_lifecycle_for_session(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    session_id: Option<SessionId>,
) -> Option<crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1> {
    let session_id = session_id?;
    crate::daemon::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
        envelope.project_id,
        envelope.worktree_id,
        &session_id,
    )
    .await
}

pub(super) fn hook_v2_native_context_scout_lifecycle(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<crate::hooks::NativeContextScoutLifecycleV1> {
    let lifecycle: crate::hooks::NativeContextScoutLifecycleV1 =
        serde_json::from_value(args.get("native_lifecycle")?.clone()).ok()?;
    lifecycle.matches_envelope(envelope).then_some(lifecycle)
}

pub(super) async fn admit_native_context_scout_lifecycle(
    sessions: &RegisteredGlobalDb,
    provider: ProviderId,
    lifecycle: &crate::hooks::NativeContextScoutLifecycleV1,
    range: ObservationSourceRangeV1,
) -> bool {
    let StoreShardScopeV1::ProjectSessions { project_id } = &sessions.binding().shard_id.scope
    else {
        return false;
    };
    let project_id = project_id.clone();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let raw = match serde_json::to_vec(lifecycle) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let session_id = lifecycle.session_id.clone();
    let call_id = lifecycle.call_id.clone();
    let canonical_provider = provider.clone();
    let parsed = match parse_normalized_observation_record_v1(
        &raw,
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        move |_| {
            CanonicalObservationEnvelopeV1::new(
                canonical_provider,
                "hook_tool_after",
                call_id.clone(),
                CanonicalObservationRelationsV1::new(session_id.clone())
                    .with_thread_id(
                        ObservationId::new(session_id.as_str().to_owned())
                            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
                    )
                    .with_turn_id(call_id.clone())
                    .with_agent_id(
                        ObservationId::new(session_id.as_str().to_owned())
                            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
                    )
                    .with_message_id(call_id.clone()),
                vec![CanonicalObservationFactV1::Boundary {
                    boundary_kind: CanonicalBoundaryKindV1::TurnEnd,
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::DaemonSequence,
                    range,
                ),
            )
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
        },
    ) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    let source =
        match ObservationSourceIdentityV1::for_provider(provider, lifecycle.session_id.clone()) {
            Ok(source) => source,
            Err(_) => return false,
        };
    let binding = sessions.binding();
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::registered_for_project(
        binding.shard_id.brain_id.clone(),
        binding.shard_id.profile_id.clone(),
        project_id,
        sessions,
    ));
    let expected_cursor = match facade.get_source_cursor(&source, &scope).await {
        Ok(None) => None,
        Ok(Some(cursor))
            if cursor.generation().file_id() == 1
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence
                && cursor.position() == range.start() =>
        {
            Some(cursor)
        }
        Ok(Some(cursor))
            if cursor.generation().file_id() == 1
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence
                && cursor.position() == range.end() =>
        {
            None
        }
        Ok(Some(_)) | Err(_) => return false,
    };
    let identity = match ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        match ObservationSourceGenerationV1::new(1) {
            Ok(generation) => generation,
            Err(_) => return false,
        },
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        lifecycle.call_id.clone(),
    ) {
        Ok(identity) => identity,
        Err(_) => return false,
    };
    let request = match CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        match RetentionClass::new("transcript.hook-lifecycle.v1") {
            Ok(retention) => retention,
            Err(_) => return false,
        },
        ObservationCancellation::default(),
    ) {
        Ok(request) => request,
        Err(_) => return false,
    };
    // Admission is the durable commit of the lifecycle observation. Providers
    // routed through the external-source replay path commit the same durable
    // record while projection continues as bounded background work, so a
    // queued projection never blocks Scout lifecycle admission. Idempotent
    // re-admission of the exact same record remains admitted.
    match facade.capture_observation(request).await {
        Ok(CaptureObservationOutcome::Persisted { .. }) => true,
        Ok(CaptureObservationOutcome::AcceptedForReplay { outcome, .. }) => matches!(
            *outcome,
            ObservationPersistOutcome::Committed(_) | ObservationPersistOutcome::ExactDuplicate(_)
        ),
        Ok(
            CaptureObservationOutcome::Rejected { .. }
            | CaptureObservationOutcome::Quarantined { .. },
        )
        | Err(_) => false,
    }
}

const MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS: usize = 256;

type HookV2DeliveryClaimKey = ([u8; 16], [u8; 16]);
type HookV2DeliveryClaims = StdMutex<
    BTreeMap<HookV2DeliveryClaimKey, crate::agents::context_scout_v2::ContextScoutDurableClaimV1>,
>;

fn retained_hook_v2_delivery_claims() -> &'static HookV2DeliveryClaims {
    static CLAIMS: OnceLock<HookV2DeliveryClaims> = OnceLock::new();
    CLAIMS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(super) fn retain_hook_v2_delivery_claim(
    project_id: [u8; 16],
    claim: crate::agents::context_scout_v2::ContextScoutDurableClaimV1,
    now: UtcMicros,
) -> std::result::Result<(), Box<crate::agents::context_scout_v2::ContextScoutDurableClaimV1>> {
    let key = (project_id, claim.entry.envelope.envelope_id);
    let Ok(mut claims) = retained_hook_v2_delivery_claims().lock() else {
        return Err(Box::new(claim));
    };
    claims.retain(|_, claim| claim.lease.expires_at.0 > now.0);
    if claims.contains_key(&key) || claims.len() >= MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS {
        return Err(Box::new(claim));
    }
    claims.insert(key, claim);
    Ok(())
}

pub(super) fn lookup_hook_v2_delivery_claim(
    project_id: [u8; 16],
    envelope_id: [u8; 16],
) -> Option<crate::agents::context_scout_v2::ContextScoutDurableClaimV1> {
    retained_hook_v2_delivery_claims()
        .lock()
        .ok()?
        .get(&(project_id, envelope_id))
        .cloned()
}

pub(super) fn remove_hook_v2_delivery_claim(project_id: [u8; 16], envelope_id: [u8; 16]) {
    if let Ok(mut claims) = retained_hook_v2_delivery_claims().lock() {
        claims.remove(&(project_id, envelope_id));
    }
}

fn release_hook_v2_delivery_claim(
    project_id: [u8; 16],
    envelope_id: [u8; 16],
    outcome: crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1,
) -> bool {
    remove_hook_v2_delivery_claim(project_id, envelope_id);
    outcome == crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable
}

pub(super) async fn hook_v2_scout_prepare(cg: &TraceDecay, args: &Value) -> Result<Value> {
    let envelope = hook_v2_envelope(args, "hook_v2_scout_prepare")?;
    let now = hook_now();
    let snapshot = match hook_v2_binding_admission(cg, &envelope, now) {
        HookV2BindingAdmission::Bound(snapshot) => snapshot,
        HookV2BindingAdmission::Unavailable => {
            return Ok(json!({
                "action": "hook_v2_scout_prepare",
                "status": "unavailable",
            }));
        }
        HookV2BindingAdmission::CatchupRequired => {
            return Ok(hook_v2_catchup_response("hook_v2_scout_prepare"));
        }
    };
    let lifecycle = hook_v2_context_scout_lifecycle(args, &envelope).await;
    Ok(orchestration_response(
        "hook_v2_scout_prepare",
        crate::daemon::admit_registered_hook_orchestration(
            envelope.clone(),
            snapshot.binding,
            lifecycle,
            snapshot.revision,
            true,
            None,
        ),
    ))
}

fn orchestration_response(
    action: &str,
    outcome: crate::daemon::HookOrchestrationAdmissionV1,
) -> Value {
    use crate::daemon::HookOrchestrationAdmissionV1 as Admission;
    match outcome {
        Admission::Enqueued => json!({ "action": action, "status": "accepted" }),
        Admission::Backpressured => json!({ "action": action, "status": "deferred" }),
        Admission::UnsupportedTrigger => json!({ "action": action, "status": "unsupported" }),
        Admission::Unavailable => json!({
            "action": action,
            "status": "unavailable",
            "reason": "orchestration_unavailable",
        }),
    }
}

pub(super) async fn hook_v2_delivery_receipt(cg: &TraceDecay, args: &Value) -> Result<Value> {
    let receipt = required_value(args, "receipt")?;
    let receipt = serde_json::from_value::<
        crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    >(receipt)
    .map_err(|error| config_error(format!("invalid Context Scout receipt: {error}")))?;
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(json!({ "status": "unavailable" }));
    };
    let mut retained_project_id = None;
    let claim = match args.get("claim").cloned() {
        Some(claim) => serde_json::from_value::<
            crate::agents::context_scout_v2::ContextScoutDurableClaimV1,
        >(claim)
        .map_err(|error| config_error(format!("invalid Context Scout claim: {error}")))?,
        None => {
            let Some(project_id) = crate::hooks::hook_project_id_for_layout(cg.hook_store_layout())
            else {
                return Ok(json!({ "status": "unavailable" }));
            };
            let Some(claim) = lookup_hook_v2_delivery_claim(project_id, receipt.envelope_id) else {
                return Ok(json!({ "status": "unavailable" }));
            };
            retained_project_id = Some(project_id);
            claim
        }
    };
    let outcome = owner.record_delivery(&claim, &receipt).await;
    if let Some(project_id) = retained_project_id
        && release_hook_v2_delivery_claim(project_id, receipt.envelope_id, outcome)
    {
        let _ = owner.requeue(claim).await;
    }
    Ok(json!({ "status": scout_store_outcome(outcome) }))
}

pub(super) async fn hook_v2_feedback_notice_delivery(
    cg: &TraceDecay,
    args: &Value,
) -> Result<Value> {
    const ACTION: &str = "hook_v2_feedback_notice_delivery";
    let envelope = hook_v2_envelope(args, ACTION)?;
    match hook_v2_binding_admission(cg, &envelope, hook_now()) {
        HookV2BindingAdmission::Bound(_) => {}
        HookV2BindingAdmission::Unavailable => {
            return Ok(json!({ "status": "unavailable" }));
        }
        HookV2BindingAdmission::CatchupRequired => {
            return Ok(hook_v2_catchup_response(ACTION));
        }
    }
    let notice =
        serde_json::from_value::<tracedecay_usecases::advisory::AdvisoryHookLookupNoticeV1>(
            required_value(args, "feedback_notice")?,
        )
        .map_err(|error| config_error(format!("invalid advisory feedback notice: {error}")))?;
    let status = if tracedecay_usecases::advisory::acknowledge_advisory_hook_notice(
        envelope.project_id,
        envelope.worktree_id,
        &notice,
    ) {
        "stored"
    } else {
        "unavailable"
    };
    Ok(json!({ "status": status }))
}

pub(super) async fn hook_v2_feedback(cg: &TraceDecay, args: &Value) -> Result<Value> {
    let receipt = serde_json::from_value::<
        crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    >(required_value(args, "receipt")?)
    .map_err(|error| config_error(format!("invalid Context Scout receipt: {error}")))?;
    let feedback =
        serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutFeedbackV1>(
            required_value(args, "feedback")?,
        )
        .map_err(|error| config_error(format!("invalid Context Scout feedback: {error}")))?;
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(json!({ "status": "unavailable" }));
    };
    Ok(json!({
        "status": scout_store_outcome(owner.record_feedback(&receipt, feedback).await),
    }))
}

pub(super) async fn hook_v2_cancel(cg: &TraceDecay, args: &Value) -> Result<Value> {
    let work = serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutWorkV1>(
        required_value(args, "work")?,
    )
    .map_err(|error| config_error(format!("invalid Context Scout work: {error}")))?;
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(json!({ "status": "unavailable" }));
    };
    let status = owner
        .cancel(work)
        .await
        .map_or("unavailable", scout_store_outcome);
    Ok(json!({ "status": status }))
}

pub(super) async fn hook_v2_status(cg: &TraceDecay, args: &Value) -> Result<Value> {
    let control = serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutControlV1>(
        required_value(args, "control")?,
    )
    .map_err(|error| config_error(format!("invalid Context Scout control: {error}")))?;
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(json!({ "status": "unavailable" }));
    };
    let status = owner
        .status(control)
        .await
        .map_err(|error| config_error(format!("Context Scout status unavailable: {error}")))?;
    serde_json::to_value(status)
        .map_err(|error| config_error(format!("Context Scout status encoding failed: {error}")))
}

#[derive(Clone, Copy)]
pub(super) enum ContextScoutReadSurfaceV1 {
    Recent,
    Explain,
    Capability,
    Budget,
}

impl ContextScoutReadSurfaceV1 {
    pub(super) fn from_action(action: &str) -> Option<Self> {
        match action {
            "hook_v2_scout_recent" => Some(Self::Recent),
            "hook_v2_scout_explain" => Some(Self::Explain),
            "hook_v2_scout_capability" => Some(Self::Capability),
            "hook_v2_scout_budget" => Some(Self::Budget),
            _ => None,
        }
    }
}

pub(super) async fn hook_v2_scout_read(
    cg: &TraceDecay,
    args: &Value,
    action: &str,
) -> Result<Value> {
    let surface = ContextScoutReadSurfaceV1::from_action(action)
        .ok_or_else(|| config_error("unknown Context Scout read surface"))?;
    let envelope = hook_v2_envelope(args, action)?;
    let observed_at = hook_now();
    let snapshot = match hook_v2_binding_admission(cg, &envelope, observed_at) {
        HookV2BindingAdmission::Bound(snapshot) => snapshot,
        HookV2BindingAdmission::Unavailable => {
            return Ok(json!({ "action": action, "status": "unavailable" }));
        }
        HookV2BindingAdmission::CatchupRequired => {
            return Ok(hook_v2_catchup_response(action));
        }
    };
    let Some(lifecycle) = hook_v2_context_scout_lifecycle(args, &envelope).await else {
        return Ok(json!({ "action": action, "status": "unavailable" }));
    };
    let Some(hook) = crate::agents::context_scout_ports::AdmittedContextScoutHookV1::new(
        envelope,
        &snapshot.binding,
    ) else {
        return Ok(json!({ "action": action, "status": "unavailable" }));
    };
    let Some((address, _)) = cg
        .resolve_current_context_scout_claim_authority(&hook, &lifecycle, observed_at)
        .await
    else {
        return Ok(json!({ "action": action, "status": "unavailable" }));
    };
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(json!({ "action": action, "status": "unavailable" }));
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(8);
    let value = match surface {
        ContextScoutReadSurfaceV1::Recent => scout_read_payload(owner.recent_exact(address, limit).await),
        ContextScoutReadSurfaceV1::Explain => {
            scout_read_payload(owner.explain_exact(address, limit).await)
        }
        ContextScoutReadSurfaceV1::Capability => scout_read_payload(owner.capability().await),
        ContextScoutReadSurfaceV1::Budget => scout_read_payload(owner.budget().await),
    };
    Ok(match value {
        Some(value) => json!({ "action": action, "status": "ready", "value": value }),
        None => json!({ "action": action, "status": "unavailable" }),
    })
}

/// Collapses a typed Scout read into the hook response payload. A failed read
/// and an unserializable value are both the typed `unavailable` state.
fn scout_read_payload<T: serde::Serialize, E>(read: std::result::Result<T, E>) -> Option<Value> {
    read.ok()
        .and_then(|value| serde_json::to_value(value).ok())
}

fn scout_store_outcome(
    outcome: crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1,
) -> &'static str {
    use crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1;
    match outcome {
        ContextScoutDurableStoreOutcomeV1::Stored => "stored",
        ContextScoutDurableStoreOutcomeV1::Duplicate => "duplicate",
        ContextScoutDurableStoreOutcomeV1::Superseded => "superseded",
        ContextScoutDurableStoreOutcomeV1::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests;
