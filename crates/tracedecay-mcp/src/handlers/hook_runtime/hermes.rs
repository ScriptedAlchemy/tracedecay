use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tracedecay_automation_runtime::automation::config_error;
use tracedecay_contracts::retrieval::HermesReceiptStatusV1;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_host_admission::{SharedHostAdmissionBroker, TerminalReason};
use tracedecay_sessions::admission::HostAdmissionOutcome;

use crate::map_host_admission_outcome;

/// Session review needs automation, which only a pinned project
/// configuration carries; a projectless route can never run it.
pub(super) fn user_review_unavailable() -> tracedecay_domain::errors::TraceDecayError {
    config_error(
        "projectless Hermes review is unavailable: automation requires a pinned project configuration",
    )
}

async fn apply_projectless_hermes_receipt_plan(
    profile_root: &Path,
    plan: crate::hook_events::HookEventPlan,
) -> HostAdmissionOutcome {
    let dashboard_root =
        tracedecay_automation_runtime::automation::runner::user_automation_root(profile_root);
    match plan {
        crate::hook_events::HookEventPlan::RecordTerminalReceipt { route, receipt } => {
            match tracedecay_automation_runtime::automation::host_receipts::record(
                &dashboard_root,
                route,
                receipt,
            )
            .await
            {
                Ok(true) => HostAdmissionOutcome::replay_completed(true, false),
                Ok(false) => HostAdmissionOutcome::replay_completed(false, true),
                Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
            }
        }
        crate::hook_events::HookEventPlan::MarkTurnIngested {
            route,
            transcript_watermark,
        } => match tracedecay_automation_runtime::automation::host_receipts::mark_turn_ingested(
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

#[hotpath::measure(future = true, label = "mcp.hook_runtime.replay")]
#[cfg_attr(
    not(feature = "hotpath"),
    expect(
        clippy::too_many_lines,
        reason = "Projectless Hermes replay is one receipt pass without a mounted project route."
    )
)]
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
            retained_outcome.get_or_insert(outcome.clone());
            if target_seq == Some(record.seq) {
                target_outcome = Some(outcome);
            }
            continue;
        }
        let plan = match crate::hook_events::decode_durable_hook_event_plan(&record.payload) {
            Ok(plan) => plan,
            Err(crate::hook_events::DurableHookEventDecodeError::UnsupportedVersion) => {
                let outcome = HostAdmissionOutcome::durable_payload_unsupported_version();
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                retained_outcome.get_or_insert(outcome.clone());
                if target_seq == Some(record.seq) {
                    target_outcome = Some(outcome);
                }
                continue;
            }
            Err(crate::hook_events::DurableHookEventDecodeError::Malformed) => {
                let outcome = HostAdmissionOutcome::durable_payload_malformed();
                match replay
                    .quarantine(record.seq, TerminalReason::MalformedPayload)
                    .await
                {
                    Ok(_) => {
                        retained_outcome.get_or_insert(outcome.clone());
                        if target_seq == Some(record.seq) {
                            target_outcome = Some(outcome);
                        }
                    }
                    Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                        blocked_sources.insert(record.source);
                        retained_leases.push(record.seq);
                        retained_outcome.get_or_insert(failure.clone());
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
        let outcome = if canonical_outcome.status.commits_replay_record() {
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
            retained_outcome.get_or_insert(canonical_outcome.clone());
            canonical_outcome
        };
        if target_seq == Some(record.seq) {
            target_outcome = Some(outcome);
        }
    }
    for seq in retained_leases.into_iter().rev() {
        replay.defer(seq).await?;
    }
    if target_outcome.is_none()
        && let Some(seq) = target_seq
    {
        // The concurrent profile worker may have already committed this
        // seq. `HostAdmissionRuntime::commit` returns Ok(0) when
        // `seq <= committed_through` without requiring a lease; that is
        // the spool watermark, not an inferred ExactDuplicate. Any other
        // commit result is the broker's typed failure (lost / never
        // committed). `accepted_for_replay` stays only for a full drain.
        target_outcome = Some(match replay.commit(seq).await {
            Ok(_) => HostAdmissionOutcome::replay_completed(true, false),
            Err(outcome) => outcome,
        });
    }
    Ok(terminal_outcome
        .or(target_outcome)
        .or(retained_outcome)
        .unwrap_or_else(HostAdmissionOutcome::accepted_for_replay))
}

pub async fn replay_projectless_hermes_host_admission(
    broker: &SharedHostAdmissionBroker,
    profile_root: &Path,
) -> HostAdmissionOutcome {
    replay_projectless_hermes_receipts(broker, profile_root, None)
        .await
        .unwrap_or_else(|outcome| outcome)
}

async fn continue_projectless_hermes_review(
    profile_root: &Path,
    session_db: &RegisteredGlobalDb,
) -> Result<HermesReceiptStatusV1> {
    let dashboard_root =
        tracedecay_automation_runtime::automation::runner::user_automation_root(profile_root);
    let Some(ready) =
        tracedecay_automation_runtime::automation::host_receipts::oldest_ready(&dashboard_root)
            .await?
    else {
        return Ok(HermesReceiptStatusV1::Ingested);
    };
    if session_db
        .lcm_raw_message_store_id("hermes", &ready.transcript_watermark)
        .await
        .map_err(
            |error| tracedecay_domain::errors::TraceDecayError::Database {
                operation: "read Hermes transcript watermark".to_owned(),
                message: error.to_string(),
            },
        )?
        .is_none()
    {
        return Ok(HermesReceiptStatusV1::AwaitingTranscript);
    }
    Err(user_review_unavailable())
}

#[hotpath::measure(future = true, label = "mcp.hook_runtime.hermes")]
pub(super) async fn hermes_receipt(
    event_value: Value,
    profile_root: &Path,
    session_db: &RegisteredGlobalDb,
    broker: &SharedHostAdmissionBroker,
) -> Result<HermesReceiptStatusV1> {
    let event: tracedecay_hooks::core_events::DaemonHookEvent =
        serde_json::from_value(event_value.clone())?;
    if event.receipt.is_none() {
        return Err(config_error("Hermes event omitted receipt"));
    }
    let hook_event = crate::hook_events::parse_hook_event(Some(&event_value)).ok_or_else(|| {
        config_error(format!("unsupported Hermes receipt event: {}", event.event))
    })?;
    let plan = crate::hook_events::plan_hook_event(&hook_event, profile_root, None);
    let is_turn_ingested = matches!(
        plan,
        crate::hook_events::HookEventPlan::MarkTurnIngested { .. }
    );
    if !matches!(
        plan,
        crate::hook_events::HookEventPlan::RecordTerminalReceipt { .. }
            | crate::hook_events::HookEventPlan::MarkTurnIngested { .. }
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
    let payload = crate::hook_events::encode_durable_hook_event_plan(&plan)
        .map_err(|_| config_error("invalid Hermes receipt host event plan"))?;
    let admitted = broker
        .admit(&hook_event.admission_source(), &payload)
        .await
        .map_err(|outcome| map_host_admission_outcome(&outcome))?;
    let outcome = replay_projectless_hermes_receipts(broker, profile_root, Some(admitted.seq))
        .await
        .map_err(|outcome| map_host_admission_outcome(&outcome))?;
    if !outcome.status.commits_replay_record() {
        return Err(map_host_admission_outcome(&outcome));
    }
    if is_turn_ingested {
        return continue_projectless_hermes_review(profile_root, session_db).await;
    }
    Ok(HermesReceiptStatusV1::Recorded)
}

#[cfg(test)]
mod tests;
