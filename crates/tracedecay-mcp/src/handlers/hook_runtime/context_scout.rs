use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use tracedecay_agent_hosts::agents::context_scout::ContextScoutDurableStoreOutcomeV1;
use tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutLifecycleAddressV1;
use tracedecay_agent_hosts::agents::context_scout::intent_pass::intent_hint_emitted_event;
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
    CanonicalObservationFactV1, CanonicalObservationIdV1, CanonicalObservationRelationsV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
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
use tracedecay_store::{ObservationPersistOutcome, ObservationStore, StoreShardScopeV1};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeLifecycleAdmissionV1 {
    Ready,
    Superseded,
    Unavailable,
}

fn native_lifecycle_authorities<'a>(
    sessions: &'a RegisteredGlobalDb,
    background_cpu: Option<&std::sync::Arc<ProcessBackgroundCpuV1>>,
    provider: ProviderId,
    session: SessionId,
) -> Option<(
    HostAdmissionFacade<'a>,
    ObservationScopeV1,
    ObservationSourceIdentityV1,
)> {
    let StoreShardScopeV1::ProjectSessions { project_id } = &sessions.binding().shard_id.scope
    else {
        return None;
    };
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let source = ObservationSourceIdentityV1::for_provider(provider, session).ok()?;
    let binding = sessions.binding();
    let authorities = HostAdmissionAuthorities::for_project(
        binding.shard_id.brain_id.clone(),
        binding.shard_id.profile_id.clone(),
        project_id.clone(),
        sessions,
    );
    let facade = HostAdmissionFacade::new(match background_cpu {
        Some(cpu) => authorities.with_background_cpu(std::sync::Arc::clone(cpu)),
        None => authorities,
    });
    Some((facade, scope, source))
}

/// The compatible durable source cursor is a floor only for a new daemon
/// allocation. A missing cursor starts this source; incompatible or unavailable
/// authority cannot be converted into a zero floor.
pub(super) async fn native_lifecycle_order_floor(
    sessions: &RegisteredGlobalDb,
    provider: ProviderId,
    session: SessionId,
) -> Option<u64> {
    let (facade, scope, source) = native_lifecycle_authorities(sessions, None, provider, session)?;
    match facade.get_source_cursor(&source, &scope).await {
        Ok(None) => Some(0),
        Ok(Some(cursor))
            if cursor.source() == &source
                && cursor.scope() == &scope
                && cursor.generation().file_id() == 1
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence
                && cursor.position().checked_add(1).is_some() =>
        {
            Some(cursor.position().saturating_sub(1))
        }
        Ok(Some(_)) | Err(_) => None,
    }
}

fn native_lifecycle_observation_envelope(
    provider: ProviderId,
    lifecycle: &tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1,
    range: ObservationSourceRangeV1,
) -> std::result::Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let session = lifecycle.session_id.clone();
    let call = lifecycle.call_id.clone();
    let agent = ObservationId::new(session.as_str().to_owned())
        .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
    CanonicalObservationEnvelopeV1::new(
        provider,
        "hook_tool_after",
        call.clone(),
        CanonicalObservationRelationsV1::new(session)
            .with_thread_id(agent.clone())
            .with_turn_id(call.clone())
            .with_agent_id(agent)
            .with_message_id(call),
        vec![CanonicalObservationFactV1::Boundary {
            boundary_kind: CanonicalBoundaryKindV1::TurnEnd,
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::DaemonSequence, range),
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

/// A reconstructed legacy position never proves that producer work ran or
/// became obsolete. Recover its immutable range from the canonical native
/// observation, and compare only that range with the compatible source cursor.
pub(super) async fn recovered_native_lifecycle_order(
    sessions: &RegisteredGlobalDb,
    provider: ProviderId,
    lifecycle: &tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1,
    reconstructed_range: ObservationSourceRangeV1,
) -> NativeLifecycleAdmissionV1 {
    let Some((facade, scope, source)) = native_lifecycle_authorities(
        sessions,
        None,
        provider.clone(),
        lifecycle.session_id.clone(),
    ) else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    let Ok(generation) = ObservationSourceGenerationV1::new(1) else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    // Native identity derivation uses source/scope/call, not the candidate's
    // range. No reconstructed range is persisted or used for cursor ordering.
    let Ok(candidate) = ObservationIdentityMaterialV1::for_native_record(
        source.clone(),
        scope.clone(),
        generation,
        reconstructed_range,
        ObservationOrderingDomainV1::DaemonSequence,
        lifecycle.call_id.clone(),
    ) else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    let Ok(id) = CanonicalObservationIdV1::derive(&candidate) else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    let Ok(Some(stored)) = sessions.observation_store().get_observation(&id).await else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    let observation = stored.observation();
    let identity = observation.identity();
    if identity.source() != &source
        || identity.scope() != &scope
        || identity.generation() != generation
        || identity.ordering_domain() != ObservationOrderingDomainV1::DaemonSequence
        || identity.native_record_id() != Some(&lifecycle.call_id)
    {
        return NativeLifecycleAdmissionV1::Unavailable;
    }
    let range = identity.position();
    let Ok(expected) =
        native_lifecycle_observation_envelope(provider, lifecycle, range).and_then(|envelope| {
            serde_json::to_value(envelope)
                .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
        })
    else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    let committed = stored.committed_cursor();
    if observation.payload() != &expected
        || committed.source() != &source
        || committed.scope() != &scope
        || committed.generation() != generation
        || committed.ordering_domain() != ObservationOrderingDomainV1::DaemonSequence
        || committed.position() != range.end()
    {
        return NativeLifecycleAdmissionV1::Unavailable;
    }
    match facade.get_source_cursor(&source, &scope).await {
        Ok(Some(cursor))
            if cursor.source() == &source
                && cursor.scope() == &scope
                && cursor.generation() == generation
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence =>
        {
            match cursor.position().cmp(&range.end()) {
                std::cmp::Ordering::Equal => NativeLifecycleAdmissionV1::Ready,
                std::cmp::Ordering::Greater => NativeLifecycleAdmissionV1::Superseded,
                std::cmp::Ordering::Less => NativeLifecycleAdmissionV1::Unavailable,
            }
        }
        Ok(None | Some(_)) | Err(_) => NativeLifecycleAdmissionV1::Unavailable,
    }
}

/// A later canonical lifecycle is proof that an older boundary is superseded,
/// including after its successor has left the in-flight orchestration maps.
pub(super) async fn native_lifecycle_order(
    sessions: &RegisteredGlobalDb,
    background_cpu: Option<&std::sync::Arc<ProcessBackgroundCpuV1>>,
    provider: ProviderId,
    session: SessionId,
    range: ObservationSourceRangeV1,
) -> NativeLifecycleAdmissionV1 {
    let Some((facade, scope, source)) =
        native_lifecycle_authorities(sessions, background_cpu, provider, session)
    else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    match facade.get_source_cursor(&source, &scope).await {
        Ok(None) => NativeLifecycleAdmissionV1::Ready,
        Ok(Some(cursor))
            if cursor.source() == &source
                && cursor.scope() == &scope
                && cursor.generation().file_id() == 1
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence =>
        {
            if cursor.position() > range.end() {
                NativeLifecycleAdmissionV1::Superseded
            } else {
                NativeLifecycleAdmissionV1::Ready
            }
        }
        Ok(Some(_)) | Err(_) => NativeLifecycleAdmissionV1::Unavailable,
    }
}

#[tracing::instrument(name = "mcp.hook_runtime.scout_lifecycle", level = "trace", skip_all)]
pub(super) async fn admit_native_context_scout_lifecycle(
    sessions: &RegisteredGlobalDb,
    background_cpu: Option<&std::sync::Arc<ProcessBackgroundCpuV1>>,
    provider: ProviderId,
    lifecycle: &tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1,
    range: ObservationSourceRangeV1,
) -> NativeLifecycleAdmissionV1 {
    let Some((facade, scope, source)) = native_lifecycle_authorities(
        sessions,
        background_cpu,
        provider.clone(),
        lifecycle.session_id.clone(),
    ) else {
        return NativeLifecycleAdmissionV1::Unavailable;
    };
    // Admission order is per host, so another session's events, or this
    // session's events without a lifecycle, leave gaps between this session's
    // lifecycle positions. The observation covers from the session cursor to
    // this event; a cursor already at or past it means this record, or a later
    // one that supersedes it, is committed.
    let (range, expected_cursor) = match facade.get_source_cursor(&source, &scope).await {
        Ok(None) => (range, None),
        Ok(Some(cursor))
            if cursor.source() == &source
                && cursor.scope() == &scope
                && cursor.generation().file_id() == 1
                && cursor.ordering_domain() == ObservationOrderingDomainV1::DaemonSequence =>
        {
            if cursor.position() > range.end() {
                return NativeLifecycleAdmissionV1::Superseded;
            }
            if cursor.position() == range.end() {
                return NativeLifecycleAdmissionV1::Ready;
            }
            let Ok(covered) = ObservationSourceRangeV1::new(cursor.position(), range.end()) else {
                return NativeLifecycleAdmissionV1::Unavailable;
            };
            (covered, Some(cursor))
        }
        Ok(Some(_)) | Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
    };
    let raw = match serde_json::to_vec(lifecycle) {
        Ok(raw) => raw,
        Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
    };
    let parsed = match parse_normalized_observation_record_v1(
        &raw,
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        move |_| native_lifecycle_observation_envelope(provider, lifecycle, range),
    ) {
        Ok(parsed) => parsed,
        Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
    };
    let identity = match ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        match ObservationSourceGenerationV1::new(1) {
            Ok(generation) => generation,
            Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
        },
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        lifecycle.call_id.clone(),
    ) {
        Ok(identity) => identity,
        Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
    };
    let request = match CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        match RetentionClass::new("transcript.hook-lifecycle.v1") {
            Ok(retention) => retention,
            Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
        },
        ObservationCancellation::default(),
    ) {
        Ok(request) => request,
        Err(_) => return NativeLifecycleAdmissionV1::Unavailable,
    };
    // Admission is the durable commit of the lifecycle observation. Providers
    // routed through the external-source replay path commit the same durable
    // record while projection continues as bounded background work, so a
    // queued projection never blocks Scout lifecycle admission. Idempotent
    // re-admission of the exact same record remains admitted.
    match facade.capture_observation(request).await {
        Ok(CaptureObservationOutcome::Persisted { .. }) => NativeLifecycleAdmissionV1::Ready,
        Ok(CaptureObservationOutcome::AcceptedForReplay { outcome, .. })
            if matches!(
                *outcome,
                ObservationPersistOutcome::Committed(_)
                    | ObservationPersistOutcome::ExactDuplicate(_)
            ) =>
        {
            NativeLifecycleAdmissionV1::Ready
        }
        Ok(_) | Err(_) => NativeLifecycleAdmissionV1::Unavailable,
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

#[tracing::instrument(name = "mcp.hook_runtime.scout_delivery", level = "trace", skip_all)]
pub(super) async fn hook_v2_delivery_receipt(
    cg: &TraceDecay,
    receipt: Value,
    analytics: Option<&RegisteredGlobalDb>,
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
    if outcome == ContextScoutDurableStoreOutcomeV1::Stored
        && let Some(analytics) = analytics
        && let Some(event) = intent_hint_emitted_event(
            &RegisteredGlobalDb::canonical_project_key(cg.project_root()),
            &claim.entry,
            &receipt,
        )
        && let Err(error) = analytics.append_analytics_events(&[event]).await
    {
        tracing::warn!(%error, "Context Scout intent hint_emitted analytics not recorded");
    }
    if release_hook_v2_delivery_claim(project_id, receipt.envelope_id, outcome) {
        let _ = owner.requeue(claim).await;
    }
    Ok(scout_store_outcome(outcome))
}

#[tracing::instrument(name = "mcp.hook_runtime.scout_notice", level = "trace", skip_all)]
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
