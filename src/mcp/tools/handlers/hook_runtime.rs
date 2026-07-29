use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus, SharedHostAdmissionBroker, TerminalReason,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::automation::config_error;
use crate::automation::run_ledger::AutomationRunStatus;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::claude_observation::ClaudeObservationIngestError;
use crate::sessions::source::TranscriptSource;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId, UtcMicros,
};
use tracedecay_store::StoreShardScopeV1;

use super::{SessionAuthorities, rendered_tool_json};

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error(format!("missing required parameter `{key}`")))
}

pub async fn handle_hook_runtime(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    accounting_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let output = match action {
        "reset_counter" => {
            cg.reset_local_counter().await?;
            json!({ "action": action, "reset": true })
        }
        "accounting_receipt" => accounting_receipt(cg, accounting_db).await?,
        "hook_v2_admit" | "hook_v2_guidance_lookup" => {
            hook_v2_admit(cg, &args, action, required_project_db(session_authorities)?).await?
        }
        "hook_v2_scout_prepare" => hook_v2_scout_prepare(cg, &args).await?,
        "hook_v2_delivery_receipt" => hook_v2_delivery_receipt(cg, &args).await?,
        "hook_v2_feedback_notice_delivery" => hook_v2_feedback_notice_delivery(cg, &args).await?,
        "hook_v2_feedback" => hook_v2_feedback(cg, &args).await?,
        "hook_v2_cancel" => hook_v2_cancel(cg, &args).await?,
        "hook_v2_status" => hook_v2_status(cg, &args).await?,
        "opencode_lsp_updated" => {
            opencode_lsp_updated(cg, &args, required_project_db(session_authorities)?).await?
        }
        action if ContextScoutReadSurfaceV1::from_action(action).is_some() => {
            hook_v2_scout_read(cg, &args, action).await?
        }
        "ingest_transcript" => {
            if args.get("user_scope").and_then(Value::as_bool) == Some(true) {
                return Err(config_error(
                    "user transcript ingest requires projectless daemon routing",
                ));
            }
            ingest_transcript(Some(cg), &args, None, global_db, session_authorities).await?
        }
        "user_review" | "hermes_receipt" => {
            return Err(config_error(format!(
                "hook action `{action}` requires projectless daemon routing"
            )));
        }
        "codex_compact" => codex_compact(cg, &args, session_authorities).await?,
        "cursor_compact" => cursor_compact(cg, &args, session_authorities).await?,
        other => {
            return Err(config_error(format!(
                "unknown hook runtime action: {other}"
            )));
        }
    };
    Ok(rendered_tool_json(Some(cg.project_root()), &args, &output))
}

async fn opencode_lsp_updated(
    cg: &TraceDecay,
    args: &Value,
    project_sessions: &RegisteredGlobalDb,
) -> Result<Value> {
    let event = args
        .get("event")
        .ok_or_else(|| config_error("missing required parameter `event`"))?;
    let payload = serde_json::to_vec(event)
        .map_err(|error| config_error(format!("invalid OpenCode LSP event: {error}")))?;
    tracedecay_hooks::decode_opencode_lsp_event(&payload)
        .map_err(|error| config_error(format!("invalid OpenCode LSP event: {error}")))?;
    crate::application::event_lane::publish(
        project_sessions,
        crate::application::event_lane::ActivityFamilyV1::Hook,
        cg.project_root(),
        None,
        1,
        Some("opencode_lsp_updated"),
    )
    .await;
    Ok(json!({
        "action": "opencode_lsp_updated",
        "status": "accepted",
    }))
}

pub(crate) async fn handle_projectless_hook_runtime(
    args: Value,
    profile_root: &Path,
    session_runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    global_db: &RegisteredGlobalDb,
    session_authorities: SessionAuthorities<'_>,
    host_admission_broker: std::result::Result<&SharedHostAdmissionBroker, HostAdmissionOutcome>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    if !projectless_action_allowed(action, &args) {
        return Err(config_error(format!(
            "projectless hook runtime action `{action}` is forbidden"
        )));
    }
    let output = match action {
        "ingest_transcript" => {
            ingest_transcript(
                None,
                &args,
                Some(profile_root),
                Some(global_db),
                session_authorities,
            )
            .await?
        }
        "user_review" => user_review(&args, profile_root, &session_runtime_registry).await?,
        "hermes_receipt" => {
            let host_admission_broker =
                host_admission_broker.map_err(map_host_admission_outcome)?;
            hermes_receipt(
                &args,
                profile_root,
                Some(&session_runtime_registry),
                required_user_db(session_authorities)?,
                host_admission_broker,
            )
            .await?
        }
        _ => unreachable!("projectless hook action validated above"),
    };
    Ok(rendered_tool_json(None, &args, &output))
}

fn projectless_action_allowed(action: &str, args: &Value) -> bool {
    matches!(action, "user_review" | "hermes_receipt")
        || (action == "ingest_transcript"
            && args.get("user_scope").and_then(Value::as_bool) == Some(true))
}

fn hook_now() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_micros().min(i64::MAX as u128) as i64
            }),
    )
}

fn hook_v2_envelope(args: &Value, action: &str) -> Result<tracedecay_hooks::HookEventEnvelopeV2> {
    let envelope = args
        .get("envelope")
        .cloned()
        .ok_or_else(|| config_error(format!("{action} requires envelope")))
        .and_then(|value| {
            serde_json::from_value::<tracedecay_hooks::HookEventEnvelopeV2>(value)
                .map_err(|error| config_error(format!("invalid Hook V2 envelope: {error}")))
        })?;
    Ok(envelope)
}

fn hook_v2_native_session_id(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<SessionId> {
    let session = SessionId::new(args.get("native_session_id")?.as_str()?.to_owned()).ok()?;
    (crate::hooks::hook_v2_protected_session_id_for_native(session.as_str())
        == envelope.protected_session_id)
        .then_some(session)
}

async fn hook_v2_context_scout_lifecycle(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1> {
    hook_v2_context_scout_lifecycle_for_session(envelope, hook_v2_native_session_id(args, envelope))
        .await
}

async fn hook_v2_context_scout_lifecycle_for_session(
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

fn hook_v2_native_context_scout_lifecycle(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<crate::hooks::NativeContextScoutLifecycleV1> {
    let lifecycle: crate::hooks::NativeContextScoutLifecycleV1 =
        serde_json::from_value(args.get("native_lifecycle")?.clone()).ok()?;
    lifecycle.matches_envelope(envelope).then_some(lifecycle)
}

async fn admit_native_context_scout_lifecycle(
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
    matches!(
        facade.capture_observation(request).await,
        Ok(CaptureObservationOutcome::Persisted { .. })
    )
}

enum HookV2BindingAdmission {
    Bound(tracedecay_hooks::HookConfigurationSnapshotV1),
    Unavailable,
    CatchupRequired,
}

const MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS: usize = 256;

fn retained_hook_v2_delivery_claims() -> &'static StdMutex<
    BTreeMap<([u8; 16], [u8; 16]), crate::agents::context_scout_v2::ContextScoutDurableClaimV1>,
> {
    static CLAIMS: OnceLock<
        StdMutex<
            BTreeMap<
                ([u8; 16], [u8; 16]),
                crate::agents::context_scout_v2::ContextScoutDurableClaimV1,
            >,
        >,
    > = OnceLock::new();
    CLAIMS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

fn retain_hook_v2_delivery_claim(
    project_id: [u8; 16],
    claim: crate::agents::context_scout_v2::ContextScoutDurableClaimV1,
    now: UtcMicros,
) -> std::result::Result<(), crate::agents::context_scout_v2::ContextScoutDurableClaimV1> {
    let key = (project_id, claim.entry.envelope.envelope_id);
    let Ok(mut claims) = retained_hook_v2_delivery_claims().lock() else {
        return Err(claim);
    };
    claims.retain(|_, claim| claim.lease.expires_at.0 > now.0);
    if claims.contains_key(&key) || claims.len() >= MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS {
        return Err(claim);
    }
    claims.insert(key, claim);
    Ok(())
}

fn lookup_hook_v2_delivery_claim(
    project_id: [u8; 16],
    envelope_id: [u8; 16],
) -> Option<crate::agents::context_scout_v2::ContextScoutDurableClaimV1> {
    retained_hook_v2_delivery_claims()
        .lock()
        .ok()?
        .get(&(project_id, envelope_id))
        .cloned()
}

fn remove_hook_v2_delivery_claim(project_id: [u8; 16], envelope_id: [u8; 16]) {
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

fn classify_hook_v2_binding(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    outcome: tracedecay_hooks::HookConfigurationReadOutcomeV1,
) -> HookV2BindingAdmission {
    let tracedecay_hooks::HookConfigurationReadOutcomeV1::Bound(snapshot) = outcome else {
        return HookV2BindingAdmission::Unavailable;
    };
    if envelope.validate(&snapshot.binding).is_err() {
        return HookV2BindingAdmission::CatchupRequired;
    }
    HookV2BindingAdmission::Bound(snapshot)
}

fn hook_v2_binding_admission(
    cg: &TraceDecay,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    now: UtcMicros,
) -> HookV2BindingAdmission {
    let layout = cg.hook_store_layout();
    let subscriber = tracedecay_hooks::HookConfigurationSubscriberV1::new(
        tracedecay_hooks::HookConfigurationFileReaderV1::new(
            tracedecay_hooks::hook_configuration_path(&layout.data_root, envelope.producer),
        ),
    );
    classify_hook_v2_binding(envelope, subscriber.load_current(envelope.producer, now))
}

fn hook_v2_catchup_response(action: &str) -> Value {
    json!({
        "action": action,
        "status": "rejected",
        "disposition": tracedecay_hooks::HookTransportDispositionV1::CatchupRequired,
    })
}

/// Where the daemon keeps the durable admission idempotency ledgers. One
/// ledger per (hook data root, producing host) — the same daemon-owned hook
/// data root that already holds the published bindings and the replay spool.
/// No migrated database participates.
pub(crate) fn hook_v2_admission_ledger_root(
    data_root: &Path,
    host: tracedecay_hooks::HookHostV1,
) -> std::path::PathBuf {
    data_root.join("hook-v2-admissions").join(host.as_key())
}

/// Bound on distinct ledgers held open at once. A daemon serves one profile, so
/// this is (projects opened) x (bound hosts); beyond it admission reports
/// backpressure and the hook spools the envelope for replay rather than
/// admitting something it cannot deduplicate.
const MAX_OPEN_HOOK_V2_ADMISSION_LEDGERS: usize = 64;

type HookV2AdmissionLedgers =
    BTreeMap<(std::path::PathBuf, &'static str), tracedecay_hooks::HookAdmissionLedgerV1>;

fn hook_v2_admission_ledgers() -> &'static StdMutex<HookV2AdmissionLedgers> {
    static LEDGERS: OnceLock<StdMutex<HookV2AdmissionLedgers>> = OnceLock::new();
    LEDGERS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

fn hook_v2_pending_work_root(
    data_root: &Path,
    host: tracedecay_hooks::HookHostV1,
) -> std::path::PathBuf {
    data_root.join("hook-v2-pending-work").join(host.as_key())
}

fn hook_v2_pending_work_gate() -> &'static StdMutex<()> {
    static GATE: OnceLock<StdMutex<()>> = OnceLock::new();
    GATE.get_or_init(|| StdMutex::new(()))
}

fn complete_hook_v2_pending_work(
    data_root: &Path,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    sequence: u64,
    now: UtcMicros,
) -> bool {
    let key = (data_root.to_path_buf(), envelope.producer.as_key());
    let Some(mut ledgers) = hook_v2_admission_ledgers().lock().ok() else {
        return false;
    };
    let Some(ledger) = ledgers.get_mut(&key) else {
        return false;
    };
    if ledger.mark_work_completed(envelope).is_err() {
        return false;
    }
    drop(ledgers);

    // Producer completion is the durable effect fence. Pending transport
    // cleanup may fail or be interrupted after this point; a completed exact
    // duplicate retries only the acknowledgement and never the producer work.
    let Some(_gate) = hook_v2_pending_work_gate().lock().ok() else {
        return false;
    };
    let Ok((mut spool, _)) = tracedecay_hooks::HookSpoolV1::open(
        hook_v2_pending_work_root(data_root, envelope.producer),
        tracedecay_hooks::HookSpoolConfigV1::stock(envelope.producer),
        now,
    ) else {
        return false;
    };
    if spool
        .acknowledge(
            tracedecay_hooks::HookSpoolAckV1 {
                sequence,
                receipt_id: envelope.event_id,
                disposition: tracedecay_hooks::HookSpoolAckDispositionV1::Committed,
            },
            now,
        )
        .is_err()
    {
        return false;
    }
    true
}

fn retain_hook_v2_pending_work(
    data_root: &Path,
    pending_envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    ledger_envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    binding: &tracedecay_hooks::HookScopeBindingV1,
    now: UtcMicros,
) -> Option<Arc<dyn Fn() + Send + Sync + 'static>> {
    let _gate = hook_v2_pending_work_gate().lock().ok()?;
    let (mut spool, _) = tracedecay_hooks::HookSpoolV1::open(
        hook_v2_pending_work_root(data_root, pending_envelope.producer),
        tracedecay_hooks::HookSpoolConfigV1::stock(pending_envelope.producer),
        now,
    )
    .ok()?;
    let record = spool.append(pending_envelope.clone(), binding, now).ok()?;
    drop(spool);
    drop(_gate);
    let data_root = data_root.to_path_buf();
    let envelope = ledger_envelope.clone();
    Some(Arc::new(move || {
        let _ = complete_hook_v2_pending_work(&data_root, &envelope, record.sequence, hook_now());
    }))
}

pub(crate) fn hook_v2_pending_work_envelopes(
    data_root: &Path,
    host: tracedecay_hooks::HookHostV1,
    now: UtcMicros,
) -> Vec<tracedecay_hooks::HookEventEnvelopeV2> {
    let Some(_gate) = hook_v2_pending_work_gate().lock().ok() else {
        return Vec::new();
    };
    let Ok((mut spool, _)) = tracedecay_hooks::HookSpoolV1::open(
        hook_v2_pending_work_root(data_root, host),
        tracedecay_hooks::HookSpoolConfigV1::stock(host),
        now,
    ) else {
        return Vec::new();
    };
    let mut records = spool.expired_records(now);
    if let Ok(batches) = spool.claim_replay_batches(now, 4) {
        for batch in batches {
            records.extend(batch.records);
            let _ = spool.release_replay_claim(batch.claim_id);
        }
    }
    records.sort_unstable_by_key(|record| record.sequence);
    records.dedup_by_key(|record| record.sequence);
    records.into_iter().map(|record| record.envelope).collect()
}

/// Durably record one admission identity. `None` means the ledger itself is
/// unavailable — the caller must not claim an admission it cannot deduplicate.
pub(crate) fn record_hook_v2_admission(
    data_root: &Path,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    now: UtcMicros,
) -> Option<tracedecay_hooks::HookAdmissionLedgerReceiptV1> {
    let key = (data_root.to_path_buf(), envelope.producer.as_key());
    let mut ledgers = hook_v2_admission_ledgers().lock().ok()?;
    if !ledgers.contains_key(&key) {
        if ledgers.len() >= MAX_OPEN_HOOK_V2_ADMISSION_LEDGERS {
            return None;
        }
        let (ledger, _report) = tracedecay_hooks::HookAdmissionLedgerV1::open(
            hook_v2_admission_ledger_root(data_root, envelope.producer),
            envelope.producer,
            tracedecay_hooks::HookAdmissionLedgerLimitsV1::stock(),
            now,
        )
        .ok()?;
        ledgers.insert(key.clone(), ledger);
    }
    ledgers
        .get_mut(&key)?
        .admit_with_receipt(envelope, now)
        .ok()
}

#[cfg(test)]
fn forget_hook_v2_admission_ledger_for_test(data_root: &Path, host: tracedecay_hooks::HookHostV1) {
    hook_v2_admission_ledgers()
        .lock()
        .unwrap()
        .remove(&(data_root.to_path_buf(), host.as_key()));
}

fn hook_v2_requires_producer_work(envelope: &tracedecay_hooks::HookEventEnvelopeV2) -> bool {
    matches!(
        &envelope.event,
        tracedecay_hooks::HookEventV2::SavedEdit { .. }
            | tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::End
                    | tracedecay_hooks::HookBoundaryV1::TurnComplete
            }
    )
}

fn hook_v2_lifecycle_range(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    receipt: tracedecay_hooks::HookAdmissionLedgerReceiptV1,
) -> Option<ObservationSourceRangeV1> {
    let start = match envelope.ordering {
        tracedecay_hooks::HookOrderingV1::ProviderSequence(sequence) if sequence > 0 => sequence,
        tracedecay_hooks::HookOrderingV1::ProviderSequence(_) => return None,
        tracedecay_hooks::HookOrderingV1::Unknown => receipt.order.checked_add(1)?,
    };
    ObservationSourceRangeV1::new(start, start.checked_add(1)?).ok()
}

fn daemon_mint_hook_v2_id(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    domain: &[u8],
    native_id: [u8; 16],
) -> [u8; 16] {
    let producer = envelope.producer.as_key().as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.hook-v2.daemon-id.v1");
    hasher.update(envelope.binding_token);
    hasher.update(envelope.project_id);
    hasher.update(envelope.repository_id);
    hasher.update(envelope.worktree_id);
    hasher.update(envelope.worktree_epoch.to_le_bytes());
    hasher.update(envelope.protected_session_id);
    hasher.update((producer.len() as u64).to_le_bytes());
    hasher.update(producer);
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    hasher.update(native_id);
    let digest = hasher.finalize();
    let mut canonical_id = [0; 16];
    canonical_id.copy_from_slice(&digest[..16]);
    canonical_id
}

fn hook_v2_event_identity_domain(event: &tracedecay_hooks::HookEventV2) -> &'static [u8] {
    use tracedecay_hooks::{HookBoundaryV1, HookEventV2, HookLifecyclePhaseV1};

    match event {
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::Start,
        } => b"event.session.start",
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::End,
        } => b"event.session.end",
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        } => b"event.session.turn-complete",
        HookEventV2::PromptBoundary => b"event.prompt",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Started,
            ..
        } => b"event.tool.started",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Completed,
            ..
        } => b"event.tool.completed",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Failed,
            ..
        } => b"event.tool.failed",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Cancelled,
            ..
        } => b"event.tool.cancelled",
        HookEventV2::SavedEdit { .. } => b"event.saved-edit",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Started,
            ..
        } => b"event.test.started",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Completed,
            ..
        } => b"event.test.completed",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Failed,
            ..
        } => b"event.test.failed",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Cancelled,
            ..
        } => b"event.test.cancelled",
    }
}

/// Mint canonical daemon-owned identities only after binding validation.
/// Hook-provided fixed-size values are typed native identity material, never
/// canonical ledger or orchestration identities.
fn daemon_mint_hook_v2_envelope(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> tracedecay_hooks::HookEventEnvelopeV2 {
    let mut canonical = envelope.clone();
    canonical.event_id = daemon_mint_hook_v2_id(
        envelope,
        hook_v2_event_identity_domain(&envelope.event),
        envelope.event_id,
    );
    canonical.event = match &envelope.event {
        tracedecay_hooks::HookEventV2::SessionBoundary { boundary } => {
            tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: *boundary,
            }
        }
        tracedecay_hooks::HookEventV2::PromptBoundary => {
            tracedecay_hooks::HookEventV2::PromptBoundary
        }
        tracedecay_hooks::HookEventV2::ToolLifecycle {
            tool_id,
            phase,
            effect_receipt_id,
        } => tracedecay_hooks::HookEventV2::ToolLifecycle {
            tool_id: daemon_mint_hook_v2_id(envelope, b"tool", *tool_id),
            phase: *phase,
            effect_receipt_id: effect_receipt_id
                .map(|id| daemon_mint_hook_v2_id(envelope, b"effect-receipt", id)),
        },
        tracedecay_hooks::HookEventV2::SavedEdit {
            file_id,
            changed_range_count,
        } => tracedecay_hooks::HookEventV2::SavedEdit {
            file_id: daemon_mint_hook_v2_id(envelope, b"file", *file_id),
            changed_range_count: *changed_range_count,
        },
        tracedecay_hooks::HookEventV2::TestLifecycle {
            test_run_id,
            test_count,
            phase,
            receipt_id,
        } => tracedecay_hooks::HookEventV2::TestLifecycle {
            test_run_id: daemon_mint_hook_v2_id(envelope, b"test-run", *test_run_id),
            test_count: *test_count,
            phase: *phase,
            receipt_id: receipt_id.map(|id| daemon_mint_hook_v2_id(envelope, b"test-receipt", id)),
        },
    };
    canonical
}

/// The daemon-side result of admitting one Hook V2 envelope, shared by the
/// synchronous hook action and by spool replay so both converge on the same
/// idempotency record.
pub(crate) enum HookV2AdmissionOutcomeV1 {
    Admitted {
        orchestration: crate::daemon::Pr13HookOrchestrationAdmissionV1,
        ready_guidance: Value,
        feedback_notice: Value,
    },
    /// This exact envelope was already admitted; no work is repeated.
    ExactDuplicate,
    /// The same event identity previously carried different bytes.
    Conflict,
    /// The binding no longer authorizes this envelope.
    CatchupRequired,
    /// Idempotency could not be recorded, so nothing was admitted.
    Backpressured,
    Unavailable,
}

pub(crate) async fn admit_hook_v2_envelope(
    cg: &TraceDecay,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    native_session_id: Option<SessionId>,
    now: UtcMicros,
) -> HookV2AdmissionOutcomeV1 {
    admit_hook_v2_envelope_with_lifecycle(cg, envelope, native_session_id, None, None, now).await
}

async fn admit_hook_v2_envelope_with_lifecycle(
    cg: &TraceDecay,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    native_session_id: Option<SessionId>,
    native_lifecycle: Option<crate::hooks::NativeContextScoutLifecycleV1>,
    project_sessions: Option<&RegisteredGlobalDb>,
    now: UtcMicros,
) -> HookV2AdmissionOutcomeV1 {
    let provider_envelope = envelope;
    let snapshot = match hook_v2_binding_admission(cg, envelope, now) {
        HookV2BindingAdmission::Bound(snapshot) => snapshot,
        HookV2BindingAdmission::Unavailable => return HookV2AdmissionOutcomeV1::Unavailable,
        HookV2BindingAdmission::CatchupRequired => {
            return HookV2AdmissionOutcomeV1::CatchupRequired;
        }
    };
    let canonical_envelope = daemon_mint_hook_v2_envelope(envelope);
    let envelope = &canonical_envelope;
    // The durable ledger supplies stable daemon order when the provider has no
    // native sequence. Exact-duplicate retries reuse that order and retry only
    // the lifecycle prerequisite before suppressing downstream effects.
    let Some(receipt) = record_hook_v2_admission(&cg.hook_store_layout().data_root, envelope, now)
    else {
        return HookV2AdmissionOutcomeV1::Backpressured;
    };
    match receipt.decision {
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
        | tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate => {}
        tracedecay_hooks::HookAdmissionDecisionV1::Conflict => {
            return HookV2AdmissionOutcomeV1::Conflict;
        }
    }
    if let (Some(native_lifecycle), Some(project_sessions)) =
        (native_lifecycle.as_ref(), project_sessions)
    {
        let Some(range) = hook_v2_lifecycle_range(envelope, receipt) else {
            return HookV2AdmissionOutcomeV1::Backpressured;
        };
        if !admit_native_context_scout_lifecycle(
            project_sessions,
            ProviderId::new(envelope.producer.as_key()).expect("static Hook V2 provider id"),
            native_lifecycle,
            range,
        )
        .await
        {
            return HookV2AdmissionOutcomeV1::Backpressured;
        }
    }
    let requires_producer_work = hook_v2_requires_producer_work(envelope);
    if receipt.decision == tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
        && !requires_producer_work
    {
        return HookV2AdmissionOutcomeV1::ExactDuplicate;
    }
    if receipt.decision == tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
        && receipt.work_completed
    {
        let Some(cleanup) = retain_hook_v2_pending_work(
            &cg.hook_store_layout().data_root,
            provider_envelope,
            envelope,
            &snapshot.binding,
            now,
        ) else {
            return HookV2AdmissionOutcomeV1::Backpressured;
        };
        cleanup();
        return HookV2AdmissionOutcomeV1::ExactDuplicate;
    }
    let completion = if requires_producer_work {
        let Some(completion) = retain_hook_v2_pending_work(
            &cg.hook_store_layout().data_root,
            provider_envelope,
            envelope,
            &snapshot.binding,
            now,
        ) else {
            return HookV2AdmissionOutcomeV1::Backpressured;
        };
        Some(completion)
    } else {
        None
    };
    let first_admission = receipt.decision == tracedecay_hooks::HookAdmissionDecisionV1::Admitted;
    // Live-activity tap: a bound hook-v2 envelope reaching admission IS an agent
    // working in this project — the primary live hook path for every v2-bound
    // host. Publish it here, where the project scope is already resolved; the
    // application lane retains it across dashboard disconnects and restarts.
    if first_admission && let Some(project_sessions) = project_sessions {
        crate::application::event_lane::publish(
            project_sessions,
            crate::application::event_lane::ActivityFamilyV1::Hook,
            cg.project_root(),
            cg.store_layout().identity.project_id.as_deref(),
            1,
            Some(hook_v2_family_label(envelope.event.family())),
        )
        .await;
    }
    let lifecycle = hook_v2_context_scout_lifecycle_for_session(envelope, native_session_id).await;
    let claim_authority = match (
        crate::agents::context_scout_ports::AdmittedContextScoutHookV1::new(
            envelope.clone(),
            &snapshot.binding,
        ),
        lifecycle.as_ref(),
    ) {
        (Some(hook), Some(lifecycle)) => {
            cg.resolve_current_context_scout_claim_authority(&hook, lifecycle, now)
                .await
        }
        _ => None,
    };
    let ready_guidance = match (first_admission, cg.context_scout_owner(), claim_authority) {
        (true, Some(owner), Some((address, input_watermark))) => match owner
            .claim_ready_guidance_exact(envelope, address, input_watermark, snapshot.revision, now)
            .await
        {
            Some((guidance, claim)) => {
                let envelope_id = claim.entry.envelope.envelope_id;
                match retain_hook_v2_delivery_claim(envelope.project_id, claim, now) {
                    Ok(()) => match serde_json::to_value(guidance) {
                        Ok(guidance) => guidance,
                        Err(_) => {
                            if let Some(claim) =
                                lookup_hook_v2_delivery_claim(envelope.project_id, envelope_id)
                            {
                                remove_hook_v2_delivery_claim(envelope.project_id, envelope_id);
                                let _ = owner.requeue(claim).await;
                            }
                            Value::Null
                        }
                    },
                    Err(claim) => {
                        let _ = owner.requeue(claim).await;
                        Value::Null
                    }
                }
            }
            None => Value::Null,
        },
        _ => Value::Null,
    };
    let orchestration = crate::daemon::admit_registered_pr13_hook_orchestration(
        envelope.clone(),
        snapshot.binding.clone(),
        lifecycle,
        snapshot.revision,
        false,
        completion,
    );
    let feedback_notice = if first_admission {
        crate::application::advisory::peek_pr13_advisory_hook_notice(
            envelope.project_id,
            envelope.worktree_id,
        )
        .and_then(|notice| serde_json::to_value(notice).ok())
        .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    HookV2AdmissionOutcomeV1::Admitted {
        orchestration,
        ready_guidance,
        feedback_notice,
    }
}

async fn hook_v2_admit(
    cg: &TraceDecay,
    args: &Value,
    action: &str,
    project_sessions: &RegisteredGlobalDb,
) -> Result<Value> {
    let envelope = hook_v2_envelope(args, action)?;
    let now = hook_now();
    let native_session_id = hook_v2_native_session_id(args, &envelope);
    let native_lifecycle = hook_v2_native_context_scout_lifecycle(args, &envelope);
    Ok(
        match admit_hook_v2_envelope_with_lifecycle(
            cg,
            &envelope,
            native_session_id,
            native_lifecycle,
            Some(project_sessions),
            now,
        )
        .await
        {
            HookV2AdmissionOutcomeV1::Admitted {
                orchestration,
                ready_guidance,
                feedback_notice,
            } => json!({
                "action": action,
                "status": "accepted",
                "disposition": tracedecay_hooks::HookTransportDispositionV1::Accepted,
                "orchestration": orchestration,
                "ready_guidance": ready_guidance,
                "feedback_notice": feedback_notice,
            }),
            HookV2AdmissionOutcomeV1::ExactDuplicate => json!({
                "action": action,
                "status": "exact_duplicate",
                "disposition": tracedecay_hooks::HookTransportDispositionV1::Accepted,
            }),
            HookV2AdmissionOutcomeV1::Conflict => json!({
                "action": action,
                "status": "rejected",
                "disposition": tracedecay_hooks::HookTransportDispositionV1::CatchupRequired,
                "reason": "admission_identity_conflict",
            }),
            HookV2AdmissionOutcomeV1::CatchupRequired => hook_v2_catchup_response(action),
            HookV2AdmissionOutcomeV1::Backpressured => json!({
                "action": action,
                "status": "backpressured",
            }),
            HookV2AdmissionOutcomeV1::Unavailable => json!({
                "action": action,
                "status": "unavailable",
            }),
        },
    )
}

/// Short, stable label for the dashboard's activity payload. Kept here rather
/// than on the domain type: it is a display detail of the live tap, not part of
/// the hook contract.
const fn hook_v2_family_label(family: tracedecay_hooks::HookEventFamily) -> &'static str {
    match family {
        tracedecay_hooks::HookEventFamily::SessionBoundary => "session_boundary",
        tracedecay_hooks::HookEventFamily::PromptBoundary => "prompt_boundary",
        tracedecay_hooks::HookEventFamily::ToolLifecycle => "tool_lifecycle",
        tracedecay_hooks::HookEventFamily::SavedEdit => "saved_edit",
        tracedecay_hooks::HookEventFamily::TestLifecycle => "test_lifecycle",
    }
}

async fn hook_v2_scout_prepare(cg: &TraceDecay, args: &Value) -> Result<Value> {
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
        crate::daemon::admit_registered_pr13_hook_orchestration(
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
    outcome: crate::daemon::Pr13HookOrchestrationAdmissionV1,
) -> Value {
    use crate::daemon::Pr13HookOrchestrationAdmissionV1 as Admission;
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

async fn hook_v2_delivery_receipt(cg: &TraceDecay, args: &Value) -> Result<Value> {
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
            let Some(project_id) =
                crate::hooks::hook_v2_project_id_for_layout(cg.hook_store_layout())
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

async fn hook_v2_feedback_notice_delivery(cg: &TraceDecay, args: &Value) -> Result<Value> {
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
    let notice = serde_json::from_value::<
        crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
    >(required_value(args, "feedback_notice")?)
    .map_err(|error| config_error(format!("invalid advisory feedback notice: {error}")))?;
    let status = if crate::application::advisory::acknowledge_pr13_advisory_hook_notice(
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

async fn hook_v2_feedback(cg: &TraceDecay, args: &Value) -> Result<Value> {
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

async fn hook_v2_cancel(cg: &TraceDecay, args: &Value) -> Result<Value> {
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

async fn hook_v2_status(cg: &TraceDecay, args: &Value) -> Result<Value> {
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
enum ContextScoutReadSurfaceV1 {
    Recent,
    Explain,
    Capability,
    Budget,
}

impl ContextScoutReadSurfaceV1 {
    fn from_action(action: &str) -> Option<Self> {
        match action {
            "hook_v2_scout_recent" => Some(Self::Recent),
            "hook_v2_scout_explain" => Some(Self::Explain),
            "hook_v2_scout_capability" => Some(Self::Capability),
            "hook_v2_scout_budget" => Some(Self::Budget),
            _ => None,
        }
    }
}

async fn hook_v2_scout_read(cg: &TraceDecay, args: &Value, action: &str) -> Result<Value> {
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
        ContextScoutReadSurfaceV1::Recent => {
            owner.recent_exact(address, limit).await.and_then(|recent| {
                serde_json::to_value(recent).map_err(|_| {
                    crate::agents::context_scout_v2::ContextScoutErrorV1::InvalidLimits
                })
            })
        }
        ContextScoutReadSurfaceV1::Explain => {
            owner
                .explain_exact(address, limit)
                .await
                .and_then(|explanation| {
                    serde_json::to_value(explanation).map_err(|_| {
                        crate::agents::context_scout_v2::ContextScoutErrorV1::InvalidLimits
                    })
                })
        }
        ContextScoutReadSurfaceV1::Capability => owner.capability().await.and_then(|capability| {
            serde_json::to_value(capability)
                .map_err(|_| crate::agents::context_scout_v2::ContextScoutErrorV1::InvalidLimits)
        }),
        ContextScoutReadSurfaceV1::Budget => owner.budget().await.and_then(|budget| {
            serde_json::to_value(budget)
                .map_err(|_| crate::agents::context_scout_v2::ContextScoutErrorV1::InvalidLimits)
        }),
    };
    match value {
        Ok(value) => Ok(json!({ "action": action, "status": "ready", "value": value })),
        Err(_) => Ok(json!({ "action": action, "status": "unavailable" })),
    }
}

fn required_value(args: &Value, key: &str) -> Result<Value> {
    args.get(key)
        .cloned()
        .ok_or_else(|| config_error(format!("missing required field `{key}`")))
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

fn host_admission_facade<'a>(
    cg: Option<&TraceDecay>,
    scope: HostAdmissionScope,
    authorities: SessionAuthorities<'a>,
) -> Result<HostAdmissionFacade<'a>> {
    let authority = match scope {
        HostAdmissionScope::Project => match (
            authorities.project,
            authorities.profile_identity,
            authorities.project_registered,
        ) {
            (Some(_), Some(identity), registered) => {
                let project_id = project_observation_id(
                    cg.ok_or_else(|| config_error("project admission requires a project"))?,
                )?;
                match registered {
                    Some(registered) => HostAdmissionAuthorities::for_project(
                        identity.brain_id().clone(),
                        identity.profile_id().clone(),
                        project_id,
                        registered,
                    ),
                    None => HostAdmissionAuthorities::unavailable_for_project(
                        identity.brain_id().clone(),
                        identity.profile_id().clone(),
                        project_id,
                    ),
                }
            }
            (Some(_), None, _) | (None, _, _) => HostAdmissionAuthorities::default(),
        },
        HostAdmissionScope::Profile => match (
            authorities.user,
            authorities.profile_identity,
            authorities.profile_registered,
        ) {
            (Some(_), Some(identity), Some(registered)) => HostAdmissionAuthorities::for_profile(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                registered,
            ),
            (Some(_), Some(identity), None) => HostAdmissionAuthorities::unavailable_for_profile(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            ),
            (Some(_), None, _) | (None, _, _) => HostAdmissionAuthorities::default(),
        },
    };
    Ok(HostAdmissionFacade::new(authority))
}

fn required_project_db(authorities: SessionAuthorities<'_>) -> Result<&RegisteredGlobalDb> {
    authorities
        .project
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon project session database is unavailable"))
}

fn required_user_db(authorities: SessionAuthorities<'_>) -> Result<&RegisteredGlobalDb> {
    authorities
        .user
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon user session database is unavailable"))
}

fn project_observation_id(cg: &TraceDecay) -> Result<ProjectId> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| config_error("project observation identity is unavailable"))?;
    ProjectId::new(project_id.to_string())
        .map_err(|_| config_error("project observation identity is invalid"))
}

/// Admits every Codex rollout that belongs to `project_root` under one shared
/// byte budget, reporting whether any source was left unfinished.
///
/// `max_new_bytes` is a budget for the whole pass, not an allowance per
/// rollout: spending it across sources is what keeps one large rollout from
/// silently consuming the cap and reporting the pass as complete.
async fn admit_codex_project_rollouts(
    admission: &HostAdmissionFacade<'_>,
    source: &crate::sessions::codex::CodexSource,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> Result<bool> {
    let mut budget = max_new_bytes;
    let mut deferred = false;
    let mut paths = source.transcript_paths(project_root).into_iter().peekable();
    while let Some(path) = paths.next() {
        let progress =
            crate::sessions::codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
                &path,
                project_root,
                project_id.clone(),
                admission,
                budget,
                cancellation,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
        deferred |= progress.source_deferred;
        if let Some(remaining) = budget.as_mut() {
            *remaining = remaining.saturating_sub(progress.bytes_consumed);
            if *remaining == 0 {
                deferred |= paths.peek().is_some();
                break;
            }
        }
    }
    Ok(deferred)
}

async fn drain_host_observation_projections(
    admission: &HostAdmissionFacade<'_>,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> Result<u64> {
    let stats =
        crate::sessions::claude_observation::drain_projection_queue(admission, scope, cancellation)
            .await
            .map_err(|error| map_claude_observation_ingest_error(&error))?;
    Ok(stats.transcript.messages_upserted)
}

async fn codex_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let db = required_project_db(session_authorities)?;
    if let Some(source) = crate::sessions::codex::CodexSource::new() {
        let project_id = project_observation_id(cg)?;
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission =
            host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
        for path in source.transcript_paths(cg.project_root()) {
            crate::sessions::codex::try_admit_codex_jsonl_observations_for_project_with_admission(
                &path,
                cg.project_root(),
                project_id.clone(),
                &admission,
                None,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
        }
        let cancellation = ObservationCancellation::default();
        drain_host_observation_projections(&admission, &scope, &cancellation).await?;
    }
    let session_id = serde_json::from_str::<Value>(event_json)
        .ok()
        .as_ref()
        .and_then(|value| {
            ["session_id", "conversation_id", "thread_id"]
                .iter()
                .find_map(|key| value.get(*key).and_then(Value::as_str))
                .map(str::to_string)
        });
    let mut pending = db
        .pending_codex_compaction_summary_requests(session_id.as_deref(), 1)
        .await
        .map_err(|error| config_error(format!("load Codex compaction request failed: {error}")))?;
    let Some(pending) = pending.pop() else {
        return Ok(json!({
            "action": "codex_compact",
            "status": "skipped",
            "reason": "no pending compaction summary",
        }));
    };
    let config = crate::sessions::codex_app_server::CodexAppServerSummaryConfig::from_env();
    let summary = crate::sessions::codex_app_server::summarize_with_codex_app_server(
        &pending.request,
        &config,
    )
    .map_err(|error| config_error(format!("Codex summary failed: {error}")))?;
    let published = db
        .publish_codex_compaction_summary_successor(
            &pending.node_id,
            &summary.text,
            "codex_app_server",
            summary.model.as_deref().or(config.model.as_deref()),
        )
        .await
        .map_err(|error| config_error(format!("store Codex compaction summary failed: {error}")))?;
    Ok(json!({
        "action": "codex_compact",
        "status": "completed",
        "node_id": published.node_id,
        "predecessor_node_id": pending.node_id,
    }))
}

async fn cursor_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let db = required_project_db(session_authorities)?;
    let project_id = project_observation_id(cg)?;
    let admission =
        host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Cursor preCompact event omitted session id"))?;
    let ingest = crate::sessions::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
        event_json, project_id, &admission, None,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    let messages_to_compact = event_usize(&parsed, &["messages_to_compact", "compact_count"]);
    if messages_to_compact == Some(0) {
        return Ok(cursor_compact_skipped("no messages to compact"));
    }
    let message_count = event_usize(&parsed, &["message_count", "messages_count"]);
    let fresh_tail_count = message_count
        .zip(messages_to_compact)
        .map(|(count, compact)| count.saturating_sub(compact));
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);
    let first = db
        .lcm_compress(cursor_lcm_request(
            session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::HermesAuxiliary,
            None,
        ))
        .await
        .map_err(|error| config_error(format!("prepare Cursor compaction failed: {error}")))?;
    let Some(summary_request) = first.summary_request else {
        return Ok(cursor_compact_skipped(first.reason));
    };
    let config = crate::sessions::cursor_agent::CursorAgentSummaryConfig::from_env();
    let summary =
        crate::sessions::cursor_agent::summarize_with_cursor_agent(&summary_request, &config)
            .map_err(|error| config_error(format!("cursor-agent summary failed: {error}")))?;
    let second = db
        .lcm_compress(cursor_lcm_request(
            session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::Provided {
                summary_text: summary,
                route: Some("cursor_agent".to_string()),
            },
            first.frontier.current_frontier_store_id.or(Some(0)),
        ))
        .await
        .map_err(|error| config_error(format!("store Cursor compaction failed: {error}")))?;
    Ok(json!({
        "status": second.status,
        "reason": second.reason,
        "summary_nodes_created": second.summary_nodes_created,
        "summary_node_ids": second.summary_nodes.into_iter().map(|node| node.node_id).collect::<Vec<_>>(),
        "messages_upserted": ingest.messages_upserted,
    }))
}

fn cursor_compact_skipped(reason: impl Into<String>) -> Value {
    json!({
        "status": "skipped",
        "reason": reason.into(),
        "summary_nodes_created": 0,
        "summary_node_ids": [],
    })
}

fn event_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str()?.parse().ok())
    })
}

fn event_usize(value: &Value, keys: &[&str]) -> Option<usize> {
    event_i64(value, keys).and_then(|value| usize::try_from(value).ok())
}

fn cursor_lcm_request(
    session_id: &str,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
    summarizer: crate::sessions::lcm::LcmSummarizerMode,
    expected_current_frontier_store_id: Option<i64>,
) -> crate::sessions::lcm::LcmCompressionRequest {
    crate::sessions::lcm::LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens,
        focus_topic: Some("Cursor context compaction".to_string()),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length,
        reserve_tokens_floor: None,
        summarizer,
    }
}

async fn accounting_receipt(
    cg: &TraceDecay,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<Value> {
    let global_db = global_db.ok_or_else(|| {
        config_error("daemon accounting database is unavailable; local fallback is forbidden")
    })?;
    let stats = crate::accounting::parser::ingest(global_db).await;
    let tokens_saved = cg.get_tokens_saved().await.unwrap_or(0);
    let efficiency = if tokens_saved + stats.tokens_consumed > 0 {
        (tokens_saved as f64 / (tokens_saved + stats.tokens_consumed) as f64) * 100.0
    } else {
        0.0
    };
    Ok(json!({
        "action": "accounting_receipt",
        "turns_inserted": stats.turns_inserted,
        "cost_usd": stats.cost_usd,
        "tokens_consumed": stats.tokens_consumed,
        "tokens_saved": tokens_saved,
        "efficiency": efficiency,
    }))
}

async fn ingest_transcript(
    cg: Option<&TraceDecay>,
    args: &Value,
    profile_root: Option<&Path>,
    global_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let provider = required_str(args, "provider")?;
    let user_scope = args
        .get("user_scope")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_new_bytes = args.get("max_new_bytes").and_then(Value::as_u64);
    let admission_scope = if user_scope {
        HostAdmissionScope::Profile
    } else {
        HostAdmissionScope::Project
    };
    let facade = host_admission_facade(cg, admission_scope, session_authorities)?;
    let admission = facade.accept_replay(provider, admission_scope);
    match admission.status {
        HostAdmissionStatus::Unavailable => {
            let (reason_code, retryable) = match admission.reason_code {
                Some("project_authority_unbound" | "registered_authority_unavailable") => {
                    ("authority_unavailable", true)
                }
                reason_code => (
                    reason_code.unwrap_or("authority_unavailable"),
                    admission.retryable,
                ),
            };
            return Err(TraceDecayError::hook_runtime(
                reason_code,
                retryable,
                "daemon observation authority is unavailable",
            ));
        }
        HostAdmissionStatus::Unknown => {
            return Err(TraceDecayError::hook_runtime(
                admission.reason_code.unwrap_or("unknown_provider"),
                admission.retryable,
                "transcript provider is unsupported",
            ));
        }
        _ => {}
    }
    let mut claude_observation_stats = None;
    let mut snapshot_capture = None;
    let mut codex_source_deferred = false;
    let cancellation = ObservationCancellation::default();
    let messages_upserted = match (provider, user_scope) {
        ("claude", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let session_id = required_str(args, "session_id")?.to_string();
            required_user_db(session_authorities)?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            let stats = crate::sessions::claude_observation::ingest_user_sessions_with_admission(
                profile_root,
                Some(session_id),
                roots,
                &facade,
                Some(
                    max_new_bytes
                        .unwrap_or(crate::sessions::claude_observation::CLAUDE_HOOK_MAX_NEW_BYTES),
                ),
                cancellation.clone(),
            )
            .await
            .map_err(|error| map_claude_observation_ingest_error(&error))?;
            let messages_upserted = stats.transcript.messages_upserted;
            claude_observation_stats = Some(stats);
            messages_upserted
        }
        ("codex", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let session_id = required_str(args, "session_id")?.to_string();
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            crate::sessions::try_ingest_user_codex_sessions_with_db_and_admission(
                profile_root,
                Some(session_id),
                roots,
                &facade,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("cursor", true) => {
            profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let event_json = required_str(args, "event_json")?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            crate::sessions::cursor::try_ingest_cursor_user_transcript_event_capped_with_admission(
                event_json,
                &facade,
                max_new_bytes,
                &roots,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("codex", false) => {
            let cg =
                cg.ok_or_else(|| config_error("project transcript ingest requires a project"))?;
            let source = crate::sessions::codex::CodexSource::new()
                .ok_or_else(|| config_error("Codex transcript source is unavailable"))?;
            let project_id = project_observation_id(cg)?;
            let scope = ObservationScopeV1::Project {
                project_id: project_id.clone(),
            };
            codex_source_deferred = admit_codex_project_rollouts(
                &facade,
                &source,
                cg.project_root(),
                project_id,
                max_new_bytes,
                &cancellation,
            )
            .await?;
            drain_host_observation_projections(&facade, &scope, &cancellation).await?
        }
        ("cursor", false) => {
            let cg =
                cg.ok_or_else(|| config_error("project transcript ingest requires a project"))?;
            let event_json = required_str(args, "event_json")?;
            crate::sessions::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
                event_json,
                project_observation_id(cg)?,
                &facade,
                max_new_bytes,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("kiro", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let source = crate::sessions::kiro::KiroSource::new()
                .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            let source = source.for_user_scope(roots);
            let capture = crate::sessions::kiro::capture_kiro_snapshot_observations(
                &facade,
                &source,
                profile_root,
                ObservationScopeV1::Profile,
                max_new_bytes,
                &cancellation,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
            snapshot_capture = Some(capture);
            drain_host_observation_projections(&facade, &ObservationScopeV1::Profile, &cancellation)
                .await?
        }
        ("kiro", false) => {
            let cg =
                cg.ok_or_else(|| config_error("project transcript ingest requires a project"))?;
            let source = crate::sessions::kiro::KiroSource::new()
                .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
            let project_id = project_observation_id(cg)?;
            let scope = ObservationScopeV1::Project {
                project_id: project_id.clone(),
            };
            let capture = crate::sessions::kiro::capture_kiro_snapshot_observations(
                &facade,
                &source,
                cg.project_root(),
                scope.clone(),
                max_new_bytes,
                &cancellation,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
            snapshot_capture = Some(capture);
            drain_host_observation_projections(&facade, &scope, &cancellation).await?
        }
        _ => {
            return Err(config_error(format!(
                "unsupported transcript route: provider={provider} user_scope={user_scope}"
            )));
        }
    };
    let authority_changed = messages_upserted > 0
        || snapshot_capture
            .as_ref()
            .is_some_and(|capture| capture.stats.messages_upserted > 0)
        || claude_observation_stats
            .as_ref()
            .is_some_and(|stats| stats.observations_committed > 0 || stats.cursor_advances > 0);
    let exact_duplicate = !authority_changed
        && claude_observation_stats
            .as_ref()
            .is_some_and(|stats| stats.observation_duplicates > 0 || stats.cursor_duplicates > 0);
    let deferred_by_byte_cap = codex_source_deferred
        || snapshot_capture
            .as_ref()
            .is_some_and(|capture| capture.deferred_by_byte_cap);
    let admission = complete_ingest_admission(
        admission,
        authority_changed,
        exact_duplicate,
        deferred_by_byte_cap,
    );
    let mut output = json!({
        "action": "ingest_transcript",
        "provider": provider,
        "user_scope": user_scope,
        "completed": !deferred_by_byte_cap,
        "status": admission.status,
        "admission": admission,
        "messages_upserted": messages_upserted,
    });
    if let Some(capture) = snapshot_capture {
        output["observations_committed"] = json!(capture.stats.messages_upserted);
        output["bytes_consumed"] = json!(capture.bytes_consumed);
        output["deferred_by_byte_cap"] = json!(capture.deferred_by_byte_cap);
    }
    if let Some(stats) = claude_observation_stats {
        output["observations_committed"] = json!(stats.observations_committed);
        output["observation_duplicates"] = json!(stats.observation_duplicates);
        output["cursor_advances"] = json!(stats.cursor_advances);
        output["cursor_duplicates"] = json!(stats.cursor_duplicates);
        output["records_rejected"] = json!(stats.records_rejected);
        output["records_quarantined"] = json!(stats.records_quarantined);
        output["projections_completed"] = json!(stats.projections_completed);
        output["projections_skipped"] = json!(stats.projections_skipped);
        output["projection_duplicates"] = json!(stats.projection_duplicates);
        output["deferred_sources"] = json!(stats.deferred_sources);
        output["source_bytes_scanned"] = json!(stats.source_bytes_scanned);
    }
    Ok(output)
}

fn complete_ingest_admission(
    admission: HostAdmissionOutcome,
    authority_changed: bool,
    exact_duplicate: bool,
    deferred_by_byte_cap: bool,
) -> HostAdmissionOutcome {
    if deferred_by_byte_cap {
        HostAdmissionOutcome::retained_backpressured("ingest_pass_backpressured")
    } else if admission.status == HostAdmissionStatus::AcceptedForReplay {
        HostAdmissionOutcome::replay_completed(authority_changed, exact_duplicate)
    } else {
        admission
    }
}

fn map_transcript_ingest_error(
    error: &crate::sessions::source::TranscriptIngestError,
) -> TraceDecayError {
    let failure = crate::sessions::classify_transcript_ingest_failure("requested", "hook", error);
    TraceDecayError::hook_runtime(
        failure.reason_code,
        failure.retryable,
        format!("transcript ingest failed: {}", failure.reason_code),
    )
}

fn map_claude_observation_ingest_error(error: &ClaudeObservationIngestError) -> TraceDecayError {
    let failure = crate::sessions::classify_claude_observation_failure(error);
    TraceDecayError::hook_runtime(failure.reason_code, failure.retryable, error.to_string())
}

pub(crate) fn structured_hook_error_data(error: &TraceDecayError) -> Option<Value> {
    let (reason_code, retryable, detail) = error.hook_runtime_context()?;
    Some(json!({
        "tool": "tracedecay_hook_runtime",
        "status": hook_admission_error_status(reason_code),
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    }))
}

fn hook_admission_error_status(reason_code: &str) -> HostAdmissionStatus {
    match reason_code {
        "unknown_provider" => HostAdmissionStatus::Unknown,
        "authority_unavailable" | "authority_write_failed" | "observation_storage_failed" => {
            HostAdmissionStatus::Unavailable
        }
        "cursor_conflict" | "observation_cursor_conflict" | "observation_cancelled" => {
            HostAdmissionStatus::Backpressured
        }
        _ => HostAdmissionStatus::Degraded,
    }
}

async fn user_review(
    args: &Value,
    profile_root: &Path,
    session_runtime_registry: &Arc<DaemonSessionRuntimeRegistryV1>,
) -> Result<Value> {
    use crate::automation::run_ledger::AutomationTrigger;

    let provider = required_str(args, "provider")?;
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let run_id = args
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if crate::automation::scheduler::load_scheduler_control(
        &crate::automation::runner::user_automation_root(profile_root),
    )
    .await?
    .paused
    {
        return Ok(json!({ "action": "user_review", "status": "paused" }));
    }
    let run = run_user_review(
        profile_root,
        Arc::clone(session_runtime_registry),
        provider,
        session_id,
        run_id,
        AutomationTrigger::HostReceipt,
    )
    .await?;
    Ok(json!({
        "action": "user_review",
        "status": "completed",
        "session_reflector": run.session_reflector.ledger_record.status,
        "memory_curator": run.memory_curator.ledger_record.status,
        "skill_writer": run.skill_writer.ledger_record.status,
    }))
}

async fn run_user_review(
    profile_root: &std::path::Path,
    session_runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    provider: &str,
    session_id: Option<String>,
    run_id: Option<String>,
    trigger: crate::automation::run_ledger::AutomationTrigger,
) -> Result<crate::automation::runner::UserSessionAutomationRun> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::runner::{
        MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, UserSessionAutomationOptions,
        run_user_session_automation_with_backend,
    };

    let global = crate::user_config::UserConfig::load().automation;
    let config = crate::automation::config::effective_user_automation_config(
        profile_root,
        &global,
        crate::user_config::automation_is_configured(),
    )
    .await?;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    run_user_session_automation_with_backend(
        profile_root,
        session_runtime_registry,
        &config,
        &backend,
        UserSessionAutomationOptions {
            session_reflector: SessionReflectorAutomationOptions {
                trigger,
                run_id,
                provider: provider.to_string(),
                session_id,
                ..SessionReflectorAutomationOptions::default()
            },
            memory_curator: MemoryCuratorAutomationOptions {
                trigger,
                ..MemoryCuratorAutomationOptions::default()
            },
            skill_writer: SkillWriterAutomationOptions {
                trigger,
                provider: provider.to_string(),
                ..SkillWriterAutomationOptions::default()
            },
        },
    )
    .await
}

fn map_host_admission_outcome(outcome: HostAdmissionOutcome) -> TraceDecayError {
    TraceDecayError::hook_runtime(
        outcome.reason_code.unwrap_or("canonical_admission_failed"),
        outcome.retryable,
        "projectless Hermes receipt host admission failed",
    )
}

async fn apply_projectless_hermes_receipt_plan(
    profile_root: &Path,
    plan: crate::mcp::hook_events::HookEventPlan,
) -> HostAdmissionOutcome {
    let dashboard_root = crate::automation::runner::user_automation_root(profile_root);
    match plan {
        crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt { route, receipt } => {
            match crate::automation::host_receipts::record(&dashboard_root, route, receipt).await {
                Ok(true) => HostAdmissionOutcome::replay_completed(true, false),
                Ok(false) => HostAdmissionOutcome::replay_completed(false, true),
                Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
            }
        }
        crate::mcp::hook_events::HookEventPlan::MarkTurnIngested {
            route,
            transcript_watermark,
        } => match crate::automation::host_receipts::mark_turn_ingested(
            &dashboard_root,
            route,
            &transcript_watermark,
        )
        .await
        {
            Ok(()) => HostAdmissionOutcome::replay_completed(true, false),
            Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
        },
        _ => HostAdmissionOutcome::degraded("invalid_host_event_plan"),
    }
}

async fn replay_projectless_hermes_receipts(
    broker: &SharedHostAdmissionBroker,
    profile_root: &Path,
    target_seq: Option<u64>,
) -> std::result::Result<HostAdmissionOutcome, HostAdmissionOutcome> {
    const MAX_RECORDS_PER_PASS: usize = 64;

    let replay = broker.begin_replay().await?;
    let mut attempted = HashSet::new();
    let mut blocked_sources = HashSet::new();
    let mut retained_leases = Vec::new();
    let mut retained_outcome = None;
    let mut target_outcome = None;
    let mut terminal_outcome = None;
    for _ in 0..MAX_RECORDS_PER_PASS {
        let record = match replay.lease_next().await {
            Ok(Some(record)) => record,
            Ok(None) => break,
            Err(outcome) => {
                terminal_outcome = Some(outcome);
                break;
            }
        };
        if blocked_sources.contains(&record.source) {
            retained_leases.push(record.seq);
            continue;
        }
        if !attempted.insert(record.seq) {
            let outcome = HostAdmissionOutcome::spool_ack_conflict();
            blocked_sources.insert(record.source);
            retained_leases.push(record.seq);
            retained_outcome.get_or_insert(outcome);
            if target_seq == Some(record.seq) {
                target_outcome = Some(outcome);
            }
            continue;
        }
        let plan = match crate::mcp::hook_events::decode_durable_hook_event_plan(&record.payload) {
            Ok(plan) => plan,
            Err(crate::mcp::hook_events::DurableHookEventDecodeError::UnsupportedVersion) => {
                let outcome = HostAdmissionOutcome::durable_payload_unsupported_version();
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                retained_outcome.get_or_insert(outcome);
                if target_seq == Some(record.seq) {
                    target_outcome = Some(outcome);
                }
                continue;
            }
            Err(crate::mcp::hook_events::DurableHookEventDecodeError::Malformed) => {
                let outcome = HostAdmissionOutcome::durable_payload_malformed();
                match replay
                    .quarantine(record.seq, TerminalReason::MalformedPayload)
                    .await
                {
                    Ok(_) => {
                        retained_outcome.get_or_insert(outcome);
                        if target_seq == Some(record.seq) {
                            target_outcome = Some(outcome);
                        }
                    }
                    Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                        blocked_sources.insert(record.source);
                        retained_leases.push(record.seq);
                        retained_outcome.get_or_insert(failure);
                        if target_seq == Some(record.seq) {
                            target_outcome = Some(failure);
                        }
                    }
                    Err(failure) => {
                        terminal_outcome = Some(failure);
                        break;
                    }
                }
                continue;
            }
        };
        let canonical_outcome = apply_projectless_hermes_receipt_plan(profile_root, plan).await;
        let outcome = if matches!(
            canonical_outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ) {
            match replay.commit(record.seq).await {
                Ok(_) => canonical_outcome,
                Err(outcome) => {
                    terminal_outcome = Some(outcome);
                    break;
                }
            }
        } else {
            blocked_sources.insert(record.source);
            retained_leases.push(record.seq);
            retained_outcome.get_or_insert(canonical_outcome);
            canonical_outcome
        };
        if target_seq == Some(record.seq) {
            target_outcome = Some(outcome);
        }
    }
    for seq in retained_leases.into_iter().rev() {
        replay.defer(seq).await?;
    }
    Ok(terminal_outcome
        .or(target_outcome)
        .or(retained_outcome)
        .unwrap_or_else(HostAdmissionOutcome::accepted_for_replay))
}

pub(crate) async fn replay_projectless_hermes_host_admission(
    broker: &SharedHostAdmissionBroker,
    profile_root: &Path,
) -> HostAdmissionOutcome {
    replay_projectless_hermes_receipts(broker, profile_root, None)
        .await
        .unwrap_or_else(|outcome| outcome)
}

async fn continue_projectless_hermes_review(
    profile_root: &Path,
    session_runtime_registry: &Arc<DaemonSessionRuntimeRegistryV1>,
    session_db: &RegisteredGlobalDb,
) -> Result<Value> {
    let dashboard_root = crate::automation::runner::user_automation_root(profile_root);
    let Some(ready) = crate::automation::host_receipts::oldest_ready(&dashboard_root).await? else {
        return Ok(json!({ "action": "hermes_receipt", "status": "ingested" }));
    };
    if session_db
        .lcm_load_raw_message("hermes", &ready.transcript_watermark)
        .await
        .is_none()
    {
        return Ok(json!({ "action": "hermes_receipt", "status": "awaiting_transcript" }));
    }
    if crate::automation::scheduler::load_scheduler_control(&dashboard_root)
        .await?
        .paused
    {
        return Ok(json!({ "action": "hermes_receipt", "status": "paused" }));
    }
    let session_id = ready
        .pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let run = run_user_review(
        profile_root,
        Arc::clone(session_runtime_registry),
        "hermes",
        session_id,
        Some(format!("user_host_receipt_{}", ready.pending.generation)),
        crate::automation::run_ledger::AutomationTrigger::HostReceipt,
    )
    .await?;
    if run.session_reflector.ledger_record.status == AutomationRunStatus::Succeeded
        && run.memory_curator.ledger_record.status != AutomationRunStatus::Failed
        && run.skill_writer.ledger_record.status == AutomationRunStatus::Succeeded
    {
        crate::automation::host_receipts::mark_consumed(
            &dashboard_root,
            &ready.pending.session_key,
            ready.pending.generation,
        )
        .await?;
    }
    Ok(json!({ "action": "hermes_receipt", "status": "reviewed" }))
}

async fn hermes_receipt(
    args: &Value,
    profile_root: &Path,
    session_runtime_registry: Option<&Arc<DaemonSessionRuntimeRegistryV1>>,
    session_db: &RegisteredGlobalDb,
    broker: &SharedHostAdmissionBroker,
) -> Result<Value> {
    let event_value = args
        .get("event")
        .cloned()
        .ok_or_else(|| config_error("missing required parameter `event`"))?;
    let event: crate::daemon::DaemonHookEvent = serde_json::from_value(event_value.clone())?;
    if event.receipt.is_none() {
        return Err(config_error("Hermes event omitted receipt"));
    }
    let hook_event =
        crate::mcp::hook_events::parse_hook_event(Some(&event_value)).ok_or_else(|| {
            config_error(format!("unsupported Hermes receipt event: {}", event.event))
        })?;
    let plan = crate::mcp::hook_events::plan_hook_event(&hook_event, profile_root, None);
    let is_turn_ingested = matches!(
        plan,
        crate::mcp::hook_events::HookEventPlan::MarkTurnIngested { .. }
    );
    if !matches!(
        plan,
        crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt { .. }
            | crate::mcp::hook_events::HookEventPlan::MarkTurnIngested { .. }
    ) {
        return Err(config_error(format!(
            "unsupported Hermes receipt event: {}",
            event.event
        )));
    }
    if is_turn_ingested
        && event
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.transcript_watermark.as_deref())
            .is_none_or(str::is_empty)
    {
        return Err(config_error(
            "Hermes turnIngested omitted transcript watermark",
        ));
    }
    let payload = crate::mcp::hook_events::encode_durable_hook_event_plan(&plan)
        .map_err(|()| config_error("invalid Hermes receipt host event plan"))?;
    let admitted = broker
        .admit(&hook_event.admission_source(), &payload)
        .await
        .map_err(map_host_admission_outcome)?;
    let outcome = replay_projectless_hermes_receipts(broker, profile_root, Some(admitted.seq))
        .await
        .map_err(map_host_admission_outcome)?;
    if !matches!(
        outcome.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
    ) {
        return Err(map_host_admission_outcome(outcome));
    }
    if is_turn_ingested {
        let session_runtime_registry = session_runtime_registry.ok_or_else(|| {
            config_error("Hermes review requires retained profile runtime registry authority")
        })?;
        return continue_projectless_hermes_review(
            profile_root,
            session_runtime_registry,
            session_db,
        )
        .await;
    }
    Ok(json!({ "action": "hermes_receipt", "status": "recorded" }))
}

#[cfg(test)]
mod tests {
    use std::io;

    use tracedecay_domain::{
        CanonicalObservationIdV1, ObservationCollisionOutcomeV1, PayloadDigestV1,
    };
    use tracedecay_store::{ObservationStoreError, ProjectionStoreError};

    use super::*;
    use crate::application::host_admission::HostAdmissionTestRuntimeV1;
    use crate::application::observation::{
        CaptureClaudeObservationRequestError, ObservationApplicationError,
    };

    static RETAINED_CLAIM_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn admission_test_envelope(event_id: u8, epoch: u64) -> tracedecay_hooks::HookEventEnvelopeV2 {
        tracedecay_hooks::HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: [event_id; 16],
            producer: tracedecay_hooks::HookHostV1::ClaudeCode,
            protected_session_id: [5; 32],
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: epoch,
            binding_token: [4; 32],
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            observed_at: UtcMicros(11),
            event: tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
            },
        }
    }

    fn admission_test_binding(epoch: u64) -> tracedecay_hooks::HookScopeBindingV1 {
        let host = tracedecay_hooks::HookHostV1::ClaudeCode;
        tracedecay_hooks::HookScopeBindingV1 {
            host,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: epoch,
            binding_token: [4; 32],
            capabilities: [
                tracedecay_hooks::HookEventFamily::SessionBoundary,
                tracedecay_hooks::HookEventFamily::PromptBoundary,
                tracedecay_hooks::HookEventFamily::ToolLifecycle,
                tracedecay_hooks::HookEventFamily::SavedEdit,
                tracedecay_hooks::HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| tracedecay_hooks::HookCapabilityV1 {
                family,
                support: tracedecay_hooks::stock_event_support(host, family),
            })
            .collect(),
        }
    }

    #[test]
    fn daemon_minted_hook_ids_are_replay_stable_typed_and_binding_scoped() {
        let mut native = admission_test_envelope(9, 7);
        native.event = tracedecay_hooks::HookEventV2::SavedEdit {
            file_id: [9; 16],
            changed_range_count: 1,
        };

        let first = daemon_mint_hook_v2_envelope(&native);
        let replay = daemon_mint_hook_v2_envelope(&native);
        assert_eq!(replay, first);

        let tracedecay_hooks::HookEventV2::SavedEdit { file_id, .. } = first.event else {
            panic!("expected saved-edit envelope");
        };
        assert_ne!(first.event_id, file_id);

        let mut different_binding = native.clone();
        different_binding.binding_token = [8; 32];
        let different_binding = daemon_mint_hook_v2_envelope(&different_binding);
        assert_ne!(different_binding.event_id, first.event_id);
        let tracedecay_hooks::HookEventV2::SavedEdit {
            file_id: different_file_id,
            ..
        } = different_binding.event
        else {
            panic!("expected saved-edit envelope");
        };
        assert_ne!(different_file_id, file_id);

        let mut different_session = native.clone();
        different_session.protected_session_id = [6; 32];
        let different_session = daemon_mint_hook_v2_envelope(&different_session);
        assert_ne!(different_session.event_id, first.event_id);
    }

    #[test]
    fn daemon_admission_is_idempotent_per_identity_and_conflicts_on_different_bytes() {
        let data_root = tempfile::tempdir().unwrap();
        let now = UtcMicros(1_000);

        let first = record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 7), now)
            .unwrap();
        let duplicate =
            record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 7), now)
                .unwrap();
        let conflict =
            record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 8), now)
                .unwrap();
        let second =
            record_hook_v2_admission(data_root.path(), &admission_test_envelope(10, 7), now)
                .unwrap();
        assert_eq!(
            first.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(
            duplicate.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
        );
        assert_eq!(duplicate.order, first.order);
        assert_eq!(
            conflict.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::Conflict
        );
        assert_eq!(conflict.order, first.order);
        assert_eq!(
            second.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(second.order, first.order + 1);
        assert!(
            hook_v2_admission_ledger_root(
                data_root.path(),
                tracedecay_hooks::HookHostV1::ClaudeCode
            )
            .join("admissions.v1.bin")
            .is_file()
        );
    }

    #[test]
    fn completion_persists_before_pending_ack_failure_and_cleanup_retries() {
        let data_root = tempfile::tempdir().unwrap();
        let now = UtcMicros(1_000);
        let envelope = admission_test_envelope(31, 7);
        let binding = admission_test_binding(7);
        let first = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
        assert!(!first.work_completed);
        let unavailable =
            retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
                .expect("durable pending work");
        drop(unavailable);
        assert_eq!(
            hook_v2_pending_work_envelopes(
                data_root.path(),
                tracedecay_hooks::HookHostV1::ClaudeCode,
                now,
            ),
            std::slice::from_ref(&envelope)
        );
        let pending_sequence = {
            let (mut spool, _) = tracedecay_hooks::HookSpoolV1::open(
                hook_v2_pending_work_root(
                    data_root.path(),
                    tracedecay_hooks::HookHostV1::ClaudeCode,
                ),
                tracedecay_hooks::HookSpoolConfigV1::stock(
                    tracedecay_hooks::HookHostV1::ClaudeCode,
                ),
                now,
            )
            .unwrap();
            let batch = spool.claim_replay_batches(now, 1).unwrap().remove(0);
            let sequence = batch.records[0].sequence;
            spool.release_replay_claim(batch.claim_id).unwrap();
            sequence
        };

        assert!(
            !complete_hook_v2_pending_work(data_root.path(), &envelope, pending_sequence + 1, now),
            "invalid acknowledgement must retain pending work"
        );

        let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
        assert_eq!(
            duplicate.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
        );
        assert!(
            duplicate.work_completed,
            "completed producer work must stay fenced when pending acknowledgement fails"
        );
        assert_eq!(
            hook_v2_pending_work_envelopes(
                data_root.path(),
                tracedecay_hooks::HookHostV1::ClaudeCode,
                now,
            ),
            std::slice::from_ref(&envelope)
        );

        assert!(complete_hook_v2_pending_work(
            data_root.path(),
            &envelope,
            pending_sequence,
            now,
        ));
        assert!(
            hook_v2_pending_work_envelopes(
                data_root.path(),
                tracedecay_hooks::HookHostV1::ClaudeCode,
                now,
            )
            .is_empty()
        );
        assert!(
            record_hook_v2_admission(data_root.path(), &envelope, now)
                .unwrap()
                .work_completed
        );
        forget_hook_v2_admission_ledger_for_test(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
        );
        assert!(
            record_hook_v2_admission(data_root.path(), &envelope, now)
                .unwrap()
                .work_completed,
            "producer-work completion must survive daemon restart"
        );
    }

    #[test]
    fn completed_restart_duplicate_cleans_pending_without_work_redrive() {
        let data_root = tempfile::tempdir().unwrap();
        let now = UtcMicros(1_000);
        let envelope = admission_test_envelope(32, 7);
        let binding = admission_test_binding(7);
        record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
        let completion =
            retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
                .expect("durable pending work");
        {
            let key = (
                data_root.path().to_path_buf(),
                tracedecay_hooks::HookHostV1::ClaudeCode.as_key(),
            );
            let mut ledgers = hook_v2_admission_ledgers().lock().unwrap();
            assert!(
                ledgers
                    .get_mut(&key)
                    .unwrap()
                    .mark_work_completed(&envelope)
                    .unwrap()
            );
        }
        drop(completion);
        forget_hook_v2_admission_ledger_for_test(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
        );

        let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
        assert_eq!(
            duplicate.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
        );
        assert!(duplicate.work_completed);
        let cleanup =
            retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
                .expect("completed duplicate pending cleanup");
        cleanup();

        assert!(
            hook_v2_pending_work_envelopes(
                data_root.path(),
                tracedecay_hooks::HookHostV1::ClaudeCode,
                now,
            )
            .is_empty(),
            "completed duplicate must clear pending transport state without rerunning producer work"
        );
    }

    #[test]
    fn lifecycle_range_prefers_native_sequence_and_reuses_unknown_ledger_order() {
        let receipt = tracedecay_hooks::HookAdmissionLedgerReceiptV1 {
            decision: tracedecay_hooks::HookAdmissionDecisionV1::Admitted,
            order: 7,
            work_completed: false,
        };
        let unknown = admission_test_envelope(21, 7);
        assert_eq!(
            hook_v2_lifecycle_range(&unknown, receipt),
            ObservationSourceRangeV1::new(8, 9).ok()
        );

        let mut native = admission_test_envelope(22, 7);
        native.ordering = tracedecay_hooks::HookOrderingV1::ProviderSequence(41);
        assert_eq!(
            hook_v2_lifecycle_range(&native, receipt),
            ObservationSourceRangeV1::new(41, 42).ok()
        );
    }

    fn retained_claim(id: u8) -> crate::agents::context_scout_v2::ContextScoutDurableClaimV1 {
        use crate::agents::context_scout_v2::{
            ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1,
            ContextScoutDeliveryWindowV1, ContextScoutDurableClaimV1,
            ContextScoutDurableQueueEntryV1, ContextScoutEvidenceBindingV1,
            ContextScoutEvidenceGenerationV1, ContextScoutLeaseV1, ContextScoutModelRunOutcomeV1,
            ContextScoutRouteV1, ContextScoutSuggestionEnvelopeV1, ContextScoutWorkV1,
        };

        let address = ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: [3; 32],
            thread_id: [4; 16],
            turn_id: [5; 16],
            agent_id: [6; 16],
            logical_message_id: [id; 16],
            project_id: [201; 16],
        };
        let input_watermark = [7; 32];
        let envelope = ContextScoutSuggestionEnvelopeV1 {
            envelope_id: [id; 16],
            address,
            input_watermark,
            configuration_revision: [8; 32],
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            candidate: ContextScoutCandidateV1 {
                dedupe_key: [id; 32],
                category: ContextScoutCategoryV1::Diagnostic,
                relevance_score: 1,
                suggestion_text: "bounded".to_owned(),
                evidence: vec![ContextScoutEvidenceBindingV1 {
                    anchor_id: [9; 16],
                    content_identity: [10; 32],
                    generation: ContextScoutEvidenceGenerationV1::SavedContent,
                }],
                expires_at: UtcMicros(2_000),
            },
        };
        ContextScoutDurableClaimV1 {
            entry: ContextScoutDurableQueueEntryV1 {
                work: ContextScoutWorkV1 {
                    address,
                    generation: 1,
                    input_watermark,
                },
                route: ContextScoutRouteV1::Deterministic,
                model_outcome: ContextScoutModelRunOutcomeV1::NotRequested,
                model_receipt: None,
                envelope,
            },
            lease: ContextScoutLeaseV1 {
                lease_id: [id; 16],
                expires_at: UtcMicros(1_000),
            },
        }
    }

    #[test]
    fn exact_retained_claim_lookup_commits_beyond_thirty_two_entries() {
        let _guard = RETAINED_CLAIM_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project_id = [201; 16];
        for id in 1..=40 {
            assert!(
                retain_hook_v2_delivery_claim(project_id, retained_claim(id), UtcMicros(1)).is_ok()
            );
        }
        for id in 1..=40 {
            assert_eq!(
                lookup_hook_v2_delivery_claim(project_id, [id; 16])
                    .expect("exact retained claim")
                    .entry
                    .envelope
                    .envelope_id,
                [id; 16]
            );
            remove_hook_v2_delivery_claim(project_id, [id; 16]);
        }
    }

    #[test]
    fn retained_claims_backpressure_at_a_deterministic_bound() {
        let _guard = RETAINED_CLAIM_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for index in 0..MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS as u16 {
            let mut project_id = [202; 16];
            project_id[0] = (index >> 8) as u8;
            assert!(
                retain_hook_v2_delivery_claim(
                    project_id,
                    retained_claim(index as u8),
                    UtcMicros(1),
                )
                .is_ok()
            );
        }
        assert!(retain_hook_v2_delivery_claim([203; 16], retained_claim(1), UtcMicros(1)).is_err());
        for index in 0..MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS as u16 {
            let mut project_id = [202; 16];
            project_id[0] = (index >> 8) as u8;
            remove_hook_v2_delivery_claim(project_id, [index as u8; 16]);
        }
    }

    #[test]
    fn receipt_outcomes_release_claims_and_only_retry_unavailable() {
        use crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1;

        let _guard = RETAINED_CLAIM_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project_id = [204; 16];
        for (id, outcome, retryable) in [
            (1, ContextScoutDurableStoreOutcomeV1::Stored, false),
            (2, ContextScoutDurableStoreOutcomeV1::Duplicate, false),
            (3, ContextScoutDurableStoreOutcomeV1::Superseded, false),
            (4, ContextScoutDurableStoreOutcomeV1::Unavailable, true),
        ] {
            assert!(
                retain_hook_v2_delivery_claim(project_id, retained_claim(id), UtcMicros(1)).is_ok()
            );
            assert_eq!(
                release_hook_v2_delivery_claim(project_id, [id; 16], outcome),
                retryable
            );
            assert!(lookup_hook_v2_delivery_claim(project_id, [id; 16]).is_none());
        }
    }

    #[test]
    fn required_str_rejects_missing_and_empty_values() {
        assert!(required_str(&json!({}), "action").is_err());
        assert!(required_str(&json!({ "action": "" }), "action").is_err());
        assert_eq!(
            required_str(&json!({ "action": "reset_counter" }), "action").unwrap(),
            "reset_counter"
        );
    }

    #[test]
    fn projectless_runtime_rejects_project_database_actions() {
        assert!(!projectless_action_allowed("reset_counter", &json!({})));
        assert!(!projectless_action_allowed(
            "ingest_transcript",
            &json!({ "user_scope": false }),
        ));
        assert!(projectless_action_allowed(
            "ingest_transcript",
            &json!({ "user_scope": true }),
        ));
    }

    #[test]
    fn scout_read_actions_are_closed_and_read_only() {
        for action in [
            "hook_v2_scout_recent",
            "hook_v2_scout_explain",
            "hook_v2_scout_capability",
            "hook_v2_scout_budget",
        ] {
            assert!(ContextScoutReadSurfaceV1::from_action(action).is_some());
        }
        assert!(ContextScoutReadSurfaceV1::from_action("hook_v2_scout_apply").is_none());
    }

    #[test]
    fn bounded_snapshot_deferral_is_typed_retryable_backpressure() {
        let deferred = complete_ingest_admission(
            HostAdmissionOutcome::accepted_for_replay(),
            true,
            false,
            true,
        );
        assert_eq!(deferred.status, HostAdmissionStatus::Backpressured);
        assert!(deferred.retryable);
        assert_eq!(deferred.reason_code, Some("ingest_pass_backpressured"));

        let completed = complete_ingest_admission(
            HostAdmissionOutcome::accepted_for_replay(),
            true,
            false,
            false,
        );
        assert_eq!(completed.status, HostAdmissionStatus::Committed);
    }

    #[test]
    fn cursor_compaction_response_matches_hook_contract() {
        let value = cursor_compact_skipped("no messages to compact");
        let outcome: crate::hooks::CursorPreCompactOutcome = serde_json::from_value(value).unwrap();
        assert_eq!(outcome.status, "skipped");
        assert_eq!(outcome.reason, "no messages to compact");
        assert_eq!(outcome.summary_nodes_created, 0);
        assert!(outcome.summary_node_ids.is_empty());
    }

    #[test]
    fn session_authority_roles_fail_closed_independently() {
        let none = SessionAuthorities::default();
        assert!(required_project_db(none).is_err());
        assert!(required_user_db(none).is_err());
    }

    #[test]
    fn hook_v2_scout_prepare_accepts_no_caller_candidates() {
        let response = orchestration_response(
            "hook_v2_scout_prepare",
            crate::daemon::Pr13HookOrchestrationAdmissionV1::Unavailable,
        );
        assert_eq!(response["status"], "unavailable");
        assert_eq!(response["reason"], "orchestration_unavailable");
        assert!(!response.to_string().contains("candidate"));
        assert!(!response.to_string().contains("control"));
    }

    #[test]
    fn hook_v2_native_session_requires_exact_protected_locator() {
        let session_id = "native-session-1";
        let mut envelope = hook_v2_envelope_for_test();
        envelope.protected_session_id =
            crate::hooks::hook_v2_protected_session_id_for_native(session_id);
        assert_eq!(
            hook_v2_native_session_id(&json!({ "native_session_id": session_id }), &envelope)
                .as_ref()
                .map(SessionId::as_str),
            Some(session_id)
        );

        envelope.protected_session_id = [9; 32];
        assert!(
            hook_v2_native_session_id(&json!({ "native_session_id": session_id }), &envelope)
                .is_none()
        );
    }

    #[tokio::test]
    async fn kimi_and_opencode_queued_lifecycle_delivery_prepares_scout_lookup() {
        let temporary = tempfile::tempdir().unwrap();
        let project_id = ProjectId::new("project.native-hook-scout").unwrap();
        let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            temporary.path().join("profile"),
            temporary.path().join("project"),
            project_id.clone(),
        )
        .await
        .unwrap();
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        let worktree_id = tracedecay_domain::WorktreeId::new("worktree.native-hook-scout").unwrap();
        let hook_project_id = [71; 16];
        let hook_worktree_id = [72; 16];
        assert!(
            crate::daemon::context_scout_lifecycle::register_context_scout_lifecycle_authority(
                hook_project_id,
                hook_worktree_id,
                project_id,
                worktree_id,
                &sessions,
            )
        );

        for (provider, session, first_call, latest_call) in [
            (
                "kimi",
                "session.kimi.native",
                "call.kimi.first",
                "call.kimi.latest",
            ),
            (
                "opencode",
                "session.opencode.native",
                "call.opencode.first",
                "call.opencode.latest",
            ),
        ] {
            for (order, call) in [first_call, latest_call].into_iter().enumerate() {
                let identity =
                    crate::hooks::NativeContextScoutLifecycleV1::new(session, call).unwrap();
                let range = ObservationSourceRangeV1::new(
                    u64::try_from(order).unwrap() + 1,
                    u64::try_from(order).unwrap() + 2,
                )
                .unwrap();
                assert!(
                    admit_native_context_scout_lifecycle(
                        &sessions,
                        ProviderId::new(provider).unwrap(),
                        &identity,
                        range,
                    )
                    .await
                );
                assert!(
                    admit_native_context_scout_lifecycle(
                        &sessions,
                        ProviderId::new(provider).unwrap(),
                        &identity,
                        range,
                    )
                    .await
                );
            }
            let lifecycle =
                crate::daemon::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
                    hook_project_id,
                    hook_worktree_id,
                    &SessionId::new(session.to_owned()).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(lifecycle.provider_id.as_str(), provider);
            assert_eq!(lifecycle.thread_id.as_str(), session);
            assert_eq!(lifecycle.agent_id.as_str(), session);
            assert_eq!(lifecycle.turn_id.as_str(), latest_call);
            assert_eq!(lifecycle.logical_message_id.as_str(), latest_call);
        }
    }

    fn hook_v2_snapshot() -> tracedecay_hooks::HookConfigurationSnapshotV1 {
        tracedecay_hooks::HookConfigurationSnapshotV1 {
            schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision: 1,
            published_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            binding: tracedecay_hooks::HookScopeBindingV1 {
                host: tracedecay_hooks::HookHostV1::ClaudeCode,
                project_id: [1; 16],
                repository_id: [2; 16],
                worktree_id: [3; 16],
                worktree_epoch: 4,
                binding_token: [5; 32],
                capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
                    family: tracedecay_hooks::HookEventFamily::SessionBoundary,
                    support: tracedecay_hooks::HookEventSupportV1::Native,
                }],
            },
        }
    }

    fn hook_v2_envelope_for_test() -> tracedecay_hooks::HookEventEnvelopeV2 {
        tracedecay_hooks::HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: [6; 16],
            producer: tracedecay_hooks::HookHostV1::ClaudeCode,
            protected_session_id: [7; 32],
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [5; 32],
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            observed_at: UtcMicros(2),
            event: tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::Start,
            },
        }
    }

    #[test]
    fn hook_v2_binding_epoch_mismatch_requires_authoritative_catchup() {
        let mut envelope = hook_v2_envelope_for_test();
        envelope.worktree_epoch += 1;

        assert!(matches!(
            classify_hook_v2_binding(
                &envelope,
                tracedecay_hooks::HookConfigurationReadOutcomeV1::Bound(hook_v2_snapshot()),
            ),
            HookV2BindingAdmission::CatchupRequired
        ));
    }

    #[test]
    fn hook_v2_binding_capability_rejection_requires_authoritative_catchup() {
        let mut envelope = hook_v2_envelope_for_test();
        envelope.event = tracedecay_hooks::HookEventV2::PromptBoundary;

        assert!(matches!(
            classify_hook_v2_binding(
                &envelope,
                tracedecay_hooks::HookConfigurationReadOutcomeV1::Bound(hook_v2_snapshot()),
            ),
            HookV2BindingAdmission::CatchupRequired
        ));
    }

    #[test]
    fn hook_v2_missing_configuration_remains_transiently_unavailable() {
        assert!(matches!(
            classify_hook_v2_binding(
                &hook_v2_envelope_for_test(),
                tracedecay_hooks::HookConfigurationReadOutcomeV1::Missing,
            ),
            HookV2BindingAdmission::Unavailable
        ));
    }

    #[test]
    fn hook_v2_catchup_response_propagates_transport_disposition() {
        let response = hook_v2_catchup_response("hook_v2_admit");
        assert_eq!(response["status"], "rejected");
        assert_eq!(response["disposition"], "catchup_required");
    }

    #[tokio::test]
    async fn daemon_profile_ingest_rejects_an_unregistered_database() {
        let temp = tempfile::TempDir::new().unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(temp.path())
            .await
            .unwrap();
        let admission = host_admission_facade(
            None,
            HostAdmissionScope::Profile,
            fixture.unregistered_mcp_session_authorities_for_test(HostAdmissionScope::Profile),
        )
        .unwrap()
        .accept_replay("cursor", HostAdmissionScope::Profile);

        assert_eq!(admission.status, HostAdmissionStatus::Unavailable);
        assert_eq!(
            admission.reason_code,
            Some("registered_authority_unavailable")
        );
    }

    #[tokio::test]
    async fn transcript_admission_rejects_unknown_provider_without_echoing_hook_payload() {
        let secret = "hook-secret-unknown-provider";
        let error = ingest_transcript(
            None,
            &json!({
                "provider": "unknown-provider-v99",
                "event_json": format!("{{\"raw_source\":\"{secret}\"}}"),
            }),
            None,
            None,
            SessionAuthorities::default(),
        )
        .await
        .unwrap_err();

        let data = structured_hook_error_data(&error).unwrap();
        assert_eq!(data["status"], "unknown");
        assert_eq!(data["reason_code"], "unknown_provider");
        assert_eq!(data["retryable"], false);
        assert!(!error.to_string().contains(secret));
        assert!(!data.to_string().contains(secret));
    }

    #[tokio::test]
    async fn supported_transcript_admission_requires_its_authority_without_echoing_payload() {
        let secret = "hook-secret-unavailable-authority";
        let error = ingest_transcript(
            None,
            &json!({
                "provider": "claude",
                "event_json": format!("{{\"malformed\":\"{secret}\"}}"),
            }),
            None,
            None,
            SessionAuthorities::default(),
        )
        .await
        .unwrap_err();

        let data = structured_hook_error_data(&error).unwrap();
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason_code"], "authority_unavailable");
        assert_eq!(data["retryable"], true);
        assert!(!error.to_string().contains(secret));
        assert!(!data.to_string().contains(secret));
    }

    #[test]
    fn hook_error_response_fixtures_are_legal_and_redacted() {
        let secret = "hook-secret-error-fixture";
        let fixtures = [
            ("malformed", "malformed_event", false, "degraded"),
            ("unknown-version", "unknown_version", false, "degraded"),
            ("degraded", "source_degraded", true, "degraded"),
            ("no-source", "source_unavailable", true, "degraded"),
            (
                "repeated-delivery",
                "observation_duplicate",
                false,
                "degraded",
            ),
        ];

        for (fixture, reason_code, retryable, status) in fixtures {
            let error = TraceDecayError::hook_runtime(
                reason_code,
                retryable,
                format!("transcript fixture {fixture} failed"),
            );
            let data = structured_hook_error_data(&error).unwrap();
            let snapshot = data.to_string();

            assert_eq!(data["tool"], "tracedecay_hook_runtime", "{fixture}");
            assert_eq!(data["status"], status, "{fixture}");
            assert_eq!(data["reason_code"], reason_code, "{fixture}");
            assert_eq!(data["retryable"], retryable, "{fixture}");
            assert!(!error.to_string().contains(secret), "{fixture}");
            assert!(!snapshot.contains(secret), "{fixture}");
        }
    }

    #[test]
    fn cursor_event_numbers_accept_numeric_and_string_forms() {
        let event = json!({ "tokens": "42", "message_count": 7 });
        assert_eq!(event_i64(&event, &["tokens"]), Some(42));
        assert_eq!(event_usize(&event, &["message_count"]), Some(7));
    }

    #[test]
    fn claude_observation_request_errors_are_bounded_hook_errors() {
        let error = ClaudeObservationIngestError::Request(
            CaptureClaudeObservationRequestError::SourceRangeMismatch,
        );
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation request is invalid"));
        assert!(!rendered.contains("source range"));
        let data = structured_hook_error_data(&mapped).unwrap();
        assert_eq!(data["status"], "degraded");
        assert_eq!(data["reason_code"], "observation_request_invalid");
        assert_eq!(data["retryable"], false);
    }

    #[test]
    fn claude_observation_store_errors_keep_bounded_context_without_source_detail() {
        let error = ClaudeObservationIngestError::Store(ObservationStoreError::Storage {
            operation: "private store operation",
            source: Box::new(io::Error::other("private store source detail")),
        });
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation store operation failed"));
        assert!(!rendered.contains("private store operation"));
        assert!(!rendered.contains("private store source detail"));
        let data = structured_hook_error_data(&mapped).unwrap();
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason_code"], "observation_storage_failed");
        assert_eq!(data["retryable"], true);
    }

    #[test]
    fn claude_observation_application_store_errors_keep_bounded_context() {
        let error = ClaudeObservationIngestError::Application(ObservationApplicationError::Store(
            ObservationStoreError::Storage {
                operation: "private application store operation",
                source: Box::new(io::Error::other("private application store source detail")),
            },
        ));
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation application failed"));
        assert!(!rendered.contains("private application store operation"));
        assert!(!rendered.contains("private application store source detail"));
    }

    #[test]
    fn unavailable_persisted_observation_is_a_bounded_hook_error() {
        let error = ClaudeObservationIngestError::Application(
            ObservationApplicationError::PersistedObservationUnavailable,
        );
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation application failed"));
        assert!(!rendered.contains("persisted Claude observation"));
    }

    #[test]
    fn claude_observation_projection_errors_keep_bounded_context_without_source_detail() {
        let error = ClaudeObservationIngestError::Projection(ProjectionStoreError::Storage {
            operation: "private projection operation",
            source: Box::new(io::Error::other("private projection source detail")),
        });
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation projection failed"));
        assert!(!rendered.contains("private projection operation"));
        assert!(!rendered.contains("private projection source detail"));
    }

    #[test]
    fn claude_observation_failures_expose_stable_retry_contracts() {
        let cases = [
            (
                ClaudeObservationIngestError::Store(ObservationStoreError::CursorConflict {
                    expected: Box::new(None),
                    actual: Box::new(None),
                }),
                "observation_cursor_conflict",
                true,
            ),
            (
                ClaudeObservationIngestError::Store(ObservationStoreError::ObservationCollision {
                    observation_id: Box::new(
                        CanonicalObservationIdV1::new(format!("sha256:{}", "1".repeat(64)))
                            .unwrap(),
                    ),
                    existing_digest: Box::new(
                        PayloadDigestV1::new(format!("sha256:{}", "2".repeat(64))).unwrap(),
                    ),
                    candidate_digest: Box::new(
                        PayloadDigestV1::new(format!("sha256:{}", "3".repeat(64))).unwrap(),
                    ),
                    outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                }),
                "observation_identity_collision",
                false,
            ),
            (
                ClaudeObservationIngestError::Store(
                    ObservationStoreError::SanitizationReceiptCollision,
                ),
                "sanitization_receipt_collision",
                false,
            ),
            (
                ClaudeObservationIngestError::Application(ObservationApplicationError::Cancelled),
                "observation_cancelled",
                true,
            ),
            (
                ClaudeObservationIngestError::Projection(ProjectionStoreError::Gap {
                    expected: 4,
                    actual: 6,
                }),
                "observation_projection_checkpoint_gap",
                false,
            ),
        ];

        for (error, reason_code, retryable) in cases {
            let mapped = map_claude_observation_ingest_error(&error);
            let data = structured_hook_error_data(&mapped).unwrap();
            assert_eq!(data["reason_code"], reason_code);
            assert_eq!(data["retryable"], retryable);
        }
    }

    #[test]
    fn transcript_hook_errors_keep_bounded_retry_data_without_cursor_detail() {
        let error = crate::sessions::source::TranscriptIngestError::CursorKeyMismatch {
            expected: "private expected cursor".to_string(),
            actual: "private actual cursor".to_string(),
        };
        let mapped = map_transcript_ingest_error(&error);
        let data = structured_hook_error_data(&mapped).unwrap();

        assert_eq!(data["reason_code"], "transcript_cursor_key_mismatch");
        assert_eq!(data["retryable"], false);
        let rendered = data.to_string();
        assert!(!rendered.contains("private expected cursor"));
        assert!(!rendered.contains("private actual cursor"));
    }

    fn hermes_turn_completed_event(session_id: &str, watermark: &str) -> Value {
        json!({
            "agent": "hermes",
            "event": "turnCompleted",
            "route": { "session_id": session_id },
            "receipt": {
                "status": "success",
                "transcript_watermark": watermark
            }
        })
    }

    #[tokio::test]
    async fn projectless_hermes_receipt_uses_user_profile_without_local_writer() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes_home = temp.path().join("hermes-home");
        let hermes_profile = hermes_home.join("profiles/test");
        std::fs::create_dir_all(&hermes_profile).unwrap();
        std::fs::create_dir_all(&profile_root).unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let broker = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();

        let result = hermes_receipt(
            &json!({
                "action": "hermes_receipt",
                "event": hermes_turn_completed_event("session-local-writer", "wm-local-1"),
            }),
            &profile_root,
            None,
            required_user_db(fixture.mcp_session_authorities()).unwrap(),
            &broker,
        )
        .await
        .expect("projectless Hermes receipt should commit through the user-profile broker");

        assert_eq!(result["action"], "hermes_receipt");
        assert_eq!(result["status"], "recorded");
        assert_eq!(broker.pending_count().await, 0);
        let automation_root = crate::automation::runner::user_automation_root(&profile_root);
        assert!(
            automation_root.join("host_receipts.json").is_file(),
            "receipt watermark state must live under the user TraceDecay profile"
        );
        for forbidden in [
            hermes_profile.join("host_receipts.json"),
            hermes_profile.join("sessions.db"),
            hermes_profile.join(".tracedecay"),
            hermes_home.join("host_receipts.json"),
            hermes_home.join(".tracedecay"),
        ] {
            assert!(
                !forbidden.exists(),
                "projectless Hermes receipt must not create a local fallback writer at {}",
                forbidden.display()
            );
        }
    }

    #[tokio::test]
    async fn projectless_hermes_receipt_is_durable_before_apply_and_replays_after_restart() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("tracedecay-profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let automation_root = crate::automation::runner::user_automation_root(&profile_root);
        // Block canonical apply so admission can prove durability-before-attempt.
        std::fs::write(&automation_root, "not-a-directory").unwrap();

        let broker = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let err = hermes_receipt(
            &json!({
                "action": "hermes_receipt",
                "event": hermes_turn_completed_event("session-restart", "wm-restart-1"),
            }),
            &profile_root,
            None,
            required_user_db(fixture.mcp_session_authorities()).unwrap(),
            &broker,
        )
        .await
        .expect_err("blocked user-automation root must retain the durable Hermes receipt");
        let data = structured_hook_error_data(&err).expect("bounded hook error");
        assert_eq!(data["reason_code"], "canonical_admission_failed");
        assert_eq!(data["retryable"], true);
        assert_eq!(broker.pending_count().await, 1);
        drop(broker);

        std::fs::remove_file(&automation_root).unwrap();
        let recovered = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let outcome = replay_projectless_hermes_host_admission(&recovered, &profile_root).await;
        // A full drain with no target seq reports accepted_for_replay once the
        // retained prefix is committed; the durable watermark is the authority.
        assert!(matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::AcceptedForReplay
        ));
        assert_eq!(recovered.pending_count().await, 0);
        assert!(
            automation_root.join("host_receipts.json").is_file(),
            "restart replay must write receipts only under the user TraceDecay profile"
        );
    }

    fn valid_hermes_terminal_receipt_payload(session_id: &str, watermark: &str) -> Vec<u8> {
        let plan = crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt {
            route: Some(crate::daemon::HookRouteMetadata {
                session_id: Some(session_id.to_string()),
                thread_id: None,
                cwd: None,
                worktree: None,
                branch: None,
            }),
            receipt: crate::daemon::HookTerminalReceipt {
                tool_call_id: None,
                turn_id: None,
                status: Some("success".to_string()),
                duration_ms: Some(1),
                transcript_watermark: Some(watermark.to_string()),
            },
        };
        crate::mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap()
    }

    #[tokio::test]
    async fn malformed_profile_source_does_not_starve_valid_sibling_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("tracedecay-profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let broker = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let valid_payload =
            valid_hermes_terminal_receipt_payload("session-sibling", "wm-sibling-1");

        let malformed = broker
            .admit(
                "hermes:malformed-source",
                br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
            )
            .await
            .unwrap();
        broker
            .admit("hermes:valid-source", &valid_payload)
            .await
            .unwrap();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            replay_projectless_hermes_receipts(&broker, &profile_root, Some(malformed.seq)),
        )
        .await
        .expect("bounded profile replay must not spin on the malformed record")
        .expect("replay should finish with a typed disposition");

        assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
        assert!(!outcome.retryable);
        assert_eq!(
            broker.pending_count().await,
            0,
            "terminal evidence is quarantined and the committed sibling releases active capacity"
        );
        assert_eq!(broker.quarantine_count().await, 1);
        let automation_root = crate::automation::runner::user_automation_root(&profile_root);
        assert!(
            automation_root.join("host_receipts.json").is_file(),
            "valid sibling must apply under the user TraceDecay profile"
        );

        let reopen = replay_projectless_hermes_host_admission(&broker, &profile_root).await;
        assert_eq!(reopen.status, HostAdmissionStatus::AcceptedForReplay);
        assert_eq!(broker.pending_count().await, 0);
        assert_eq!(broker.quarantine_count().await, 1);
    }

    #[tokio::test]
    async fn malformed_profile_payload_is_quarantined_across_reopen() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("tracedecay-profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let broker = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let admitted = broker
            .admit(
                "hermes:invalid-plan-fixture",
                br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
            )
            .await
            .unwrap();

        let outcome =
            replay_projectless_hermes_receipts(&broker, &profile_root, Some(admitted.seq))
                .await
                .expect("replay should finish with a typed disposition");

        assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
        assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
        assert!(!outcome.retryable);
        assert_eq!(broker.pending_count().await, 0);
        assert_eq!(broker.quarantine_count().await, 1);
        let rendered = serde_json::to_string(&outcome).unwrap();
        assert!(!rendered.contains("invalid-plan-fixture"));
        assert!(!rendered.contains("\"branch\":\"\""));
        drop(broker);

        let recovered = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        assert_eq!(recovered.pending_count().await, 0);
        assert_eq!(recovered.quarantine_count().await, 1);
        let reopen = replay_projectless_hermes_host_admission(&recovered, &profile_root).await;
        assert_eq!(reopen.status, HostAdmissionStatus::AcceptedForReplay);
        assert_eq!(recovered.pending_count().await, 0);
        assert_eq!(recovered.quarantine_count().await, 1);
    }

    #[tokio::test]
    async fn unsupported_profile_payload_version_is_retained_without_apply() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("tracedecay-profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let broker = fixture
            .host_admission_broker_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let admitted = broker
            .admit(
                "hermes:future-plan-fixture",
                br#"{"version":2,"plan":{"kind":"future_host_event","opaque":"private"}}"#,
            )
            .await
            .unwrap();

        let outcome =
            replay_projectless_hermes_receipts(&broker, &profile_root, Some(admitted.seq))
                .await
                .expect("replay should finish with a typed disposition");

        assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
        assert_eq!(
            outcome.reason_code,
            Some("host_event_payload_unsupported_version")
        );
        assert!(outcome.retryable);
        assert_eq!(broker.pending_count().await, 1);
        assert_eq!(broker.quarantine_count().await, 0);
        let automation_root = crate::automation::runner::user_automation_root(&profile_root);
        assert!(
            !automation_root.join("host_receipts.json").is_file(),
            "unsupported version must not attempt canonical profile apply"
        );
    }
}
