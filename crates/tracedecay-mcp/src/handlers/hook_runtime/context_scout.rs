use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use tracedecay_agent_hosts::agents::context_scout::ContextScoutDurableStoreOutcomeV1;
use tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutLifecycleAddressV1;
use tracedecay_automation_runtime::automation::config_error;
use tracedecay_contracts::context_scout::{
    ContextScoutDeliveryReceiptV1, ContextScoutDurableClaimV1,
};
use tracedecay_contracts::retrieval::{
    ContextScoutStoreStatusV1, HookRuntimeDispositionV1, HookV2NoticeDeliveryResultV1,
};
use tracedecay_domain::errors::Result;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId, UtcMicros,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_project::project::TraceDecay;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use tracedecay_store::{ObservationPersistOutcome, StoreShardScopeV1};

use super::admission::{HookV2BindingAdmission, hook_v2_binding_admission};
use super::envelope::{hook_now, hook_v2_envelope};

pub(super) async fn hook_v2_context_scout_lifecycle_for_session(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    session_id: Option<SessionId>,
) -> Option<ContextScoutLifecycleAddressV1> {
    let session_id = session_id?;
    tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
        envelope.project_id,
        envelope.worktree_id,
        &session_id,
    )
    .await
}

pub(super) fn hook_v2_native_context_scout_lifecycle(
    native_lifecycle: Option<Value>,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1> {
    let lifecycle: tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1 =
        serde_json::from_value(native_lifecycle?).ok()?;
    lifecycle.matches_envelope(envelope).then_some(lifecycle)
}

#[hotpath::measure(future = true, label = "mcp.hook_runtime.scout_lifecycle")]
#[cfg_attr(
    not(feature = "hotpath"),
    expect(
        clippy::too_many_lines,
        reason = "Context-scout admission is one native lifecycle bind for the scout claim."
    )
)]
pub(super) async fn admit_native_context_scout_lifecycle(
    sessions: &RegisteredGlobalDb,
    background_cpu: Option<&std::sync::Arc<ProcessBackgroundCpuV1>>,
    provider: ProviderId,
    lifecycle: &tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1,
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
    let authorities = HostAdmissionAuthorities::for_project(
        binding.shard_id.brain_id.clone(),
        binding.shard_id.profile_id.clone(),
        project_id,
        sessions,
    );
    let facade = HostAdmissionFacade::new(match background_cpu {
        Some(background_cpu) => {
            authorities.with_background_cpu(std::sync::Arc::clone(background_cpu))
        }
        None => authorities,
    });
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
type HookV2DeliveryClaims = StdMutex<BTreeMap<HookV2DeliveryClaimKey, ContextScoutDurableClaimV1>>;

fn retained_hook_v2_delivery_claims() -> &'static HookV2DeliveryClaims {
    static CLAIMS: OnceLock<HookV2DeliveryClaims> = OnceLock::new();
    CLAIMS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(super) fn retain_hook_v2_delivery_claim(
    project_id: [u8; 16],
    claim: ContextScoutDurableClaimV1,
    now: UtcMicros,
) -> std::result::Result<(), Box<ContextScoutDurableClaimV1>> {
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
) -> Option<ContextScoutDurableClaimV1> {
    retained_hook_v2_delivery_claims()
        .lock()
        .ok()?
        .get(&(project_id, envelope_id))
        .cloned()
}

pub(super) fn lookup_hook_v2_delivery_claim_for_event(
    project_id: [u8; 16],
    event_id: [u8; 16],
    now: UtcMicros,
) -> Option<ContextScoutDurableClaimV1> {
    let claims = retained_hook_v2_delivery_claims().lock().ok()?;
    let mut matching = claims
        .iter()
        .filter(|((candidate_project_id, _), claim)| {
            *candidate_project_id == project_id && claim.lease.lease_id == event_id
        })
        .map(|(_, claim)| claim);
    let claim = matching.next()?;
    if matching.next().is_some() || claim.lease.expires_at.0 <= now.0 {
        return None;
    }
    Some(claim.clone())
}

pub(super) fn remove_hook_v2_delivery_claim(project_id: [u8; 16], envelope_id: [u8; 16]) {
    if let Ok(mut claims) = retained_hook_v2_delivery_claims().lock() {
        claims.remove(&(project_id, envelope_id));
    }
}

fn release_hook_v2_delivery_claim(
    project_id: [u8; 16],
    envelope_id: [u8; 16],
    outcome: ContextScoutDurableStoreOutcomeV1,
) -> bool {
    remove_hook_v2_delivery_claim(project_id, envelope_id);
    outcome == ContextScoutDurableStoreOutcomeV1::Unavailable
}

#[hotpath::measure(future = true, label = "mcp.hook_runtime.scout_delivery")]
pub(super) async fn hook_v2_delivery_receipt(
    cg: &TraceDecay,
    receipt: Value,
) -> Result<ContextScoutStoreStatusV1> {
    let receipt = serde_json::from_value::<ContextScoutDeliveryReceiptV1>(receipt)
        .map_err(|error| config_error(format!("invalid Context Scout receipt: {error}")))?;
    let Some(owner) = cg.context_scout_owner() else {
        return Ok(ContextScoutStoreStatusV1::Unavailable);
    };
    let Some(project_id) =
        tracedecay_agent_hosts::hooks::hook_project_id_for_layout(cg.hook_store_layout())
    else {
        return Ok(ContextScoutStoreStatusV1::Unavailable);
    };
    let Some(claim) = lookup_hook_v2_delivery_claim(project_id, receipt.envelope_id) else {
        return Ok(ContextScoutStoreStatusV1::Unavailable);
    };
    let outcome = owner.record_delivery(&claim, &receipt).await;
    if release_hook_v2_delivery_claim(project_id, receipt.envelope_id, outcome) {
        let _ = owner.requeue(claim).await;
    }
    Ok(scout_store_outcome(outcome))
}

#[hotpath::measure(label = "mcp.hook_runtime.scout_notice")]
pub(super) fn hook_v2_feedback_notice_delivery(
    cg: &TraceDecay,
    envelope: Value,
    feedback_notice: Value,
) -> Result<HookV2NoticeDeliveryResultV1> {
    let envelope = hook_v2_envelope(envelope)?;
    match hook_v2_binding_admission(cg, &envelope, hook_now()) {
        HookV2BindingAdmission::Bound(_) => {}
        HookV2BindingAdmission::Unavailable => {
            return Ok(HookV2NoticeDeliveryResultV1::Unavailable {});
        }
        HookV2BindingAdmission::CatchupRequired => {
            return Ok(HookV2NoticeDeliveryResultV1::Rejected {
                disposition: HookRuntimeDispositionV1::CatchupRequired,
            });
        }
    }
    let notice = serde_json::from_value::<
        tracedecay_application::advisory::AdvisoryHookLookupNoticeV1,
    >(feedback_notice)
    .map_err(|error| config_error(format!("invalid advisory feedback notice: {error}")))?;
    Ok(
        if tracedecay_application::advisory::acknowledge_advisory_hook_notice(
            envelope.project_id,
            envelope.worktree_id,
            &notice,
        ) {
            HookV2NoticeDeliveryResultV1::Stored {}
        } else {
            HookV2NoticeDeliveryResultV1::Unavailable {}
        },
    )
}

fn scout_store_outcome(outcome: ContextScoutDurableStoreOutcomeV1) -> ContextScoutStoreStatusV1 {
    match outcome {
        ContextScoutDurableStoreOutcomeV1::Stored => ContextScoutStoreStatusV1::Stored,
        ContextScoutDurableStoreOutcomeV1::Duplicate => ContextScoutStoreStatusV1::Duplicate,
        ContextScoutDurableStoreOutcomeV1::Superseded => ContextScoutStoreStatusV1::Superseded,
        ContextScoutDurableStoreOutcomeV1::Unavailable => ContextScoutStoreStatusV1::Unavailable,
    }
}

#[cfg(test)]
mod tests;
