use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tracedecay_domain::{ProviderId, SessionId, UtcMicros};

use super::context_scout::{
    admit_native_context_scout_lifecycle, hook_v2_context_scout_lifecycle_for_session,
    hook_v2_native_context_scout_lifecycle, lookup_hook_v2_delivery_claim,
    remove_hook_v2_delivery_claim, retain_hook_v2_delivery_claim,
};
use super::envelope::{
    daemon_mint_hook_v2_envelope, hook_now, hook_v2_envelope, hook_v2_family_label,
    hook_v2_lifecycle_range, hook_v2_native_session_id, hook_v2_requires_producer_work,
};

pub(super) enum HookV2BindingAdmission {
    Bound(tracedecay_hooks::HookConfigurationSnapshotV1),
    Unavailable,
    CatchupRequired,
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

pub(super) fn hook_v2_binding_admission(
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

pub(super) fn hook_v2_catchup_response(action: &str) -> Value {
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
    data_root.join("hook-v2-admissions").join(host.hook_key())
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
    data_root.join("hook-v2-pending-work").join(host.hook_key())
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
    let key = (data_root.to_path_buf(), envelope.producer.hook_key());
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
    let key = (data_root.to_path_buf(), envelope.producer.hook_key());
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
        .remove(&(data_root.to_path_buf(), host.hook_key()));
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
        let Ok(provider) = ProviderId::new(envelope.producer.hook_key()) else {
            return HookV2AdmissionOutcomeV1::Backpressured;
        };
        if !admit_native_context_scout_lifecycle(
            project_sessions,
            provider,
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
                        let _ = owner.requeue(*claim).await;
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

pub(super) async fn hook_v2_admit(
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

#[cfg(test)]
mod tests;
