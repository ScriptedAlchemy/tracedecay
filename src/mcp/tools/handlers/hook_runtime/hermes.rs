use crate::application::host_admission::{
    HostAdmissionOutcome, HostAdmissionStatus, SharedHostAdmissionBroker, TerminalReason,
};
use crate::automation::config_error;
use crate::automation::run_ledger::AutomationRunStatus;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use super::errors::map_host_admission_outcome;
use super::required_str;

pub(super) async fn user_review(
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

pub(super) async fn hermes_receipt(
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
mod tests;
