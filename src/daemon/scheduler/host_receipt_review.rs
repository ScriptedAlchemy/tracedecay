use std::path::Path;

use tracedecay_agent_hosts::automation::AutomationRunControl;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::{
    DaemonEngine, DaemonHandshake, effective_automation_config_for_project, log_daemon_event,
};

pub(super) async fn run_host_receipt_review(
    project_path: &Path,
    cg: &TraceDecay,
    _handshake: &DaemonHandshake,
    engine: &DaemonEngine,
    run_control: &AutomationRunControl,
) -> Result<()> {
    use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        CombinedReviewAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, registered_project_automation_retrieval,
    };

    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let Some(ready) =
        tracedecay_agent_hosts::automation::host_receipts::oldest_ready(&dashboard_root).await?
    else {
        return Ok(());
    };
    let pending = ready.pending;
    if tracedecay_agent_hosts::automation::scheduler::load_scheduler_control(&dashboard_root)
        .await?
        .paused
    {
        return Ok(());
    }
    let configuration = effective_automation_config_for_project(cg).await?;
    let config = &configuration.settings;
    let session_id = pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let Some(authoritative_project_id) = cg.store_layout().identity.project_id.as_deref() else {
        return Ok(());
    };
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_string())
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "host receipt review has an invalid authoritative project identity: {error}"
            ),
        })?;
    let session_database = engine
        .store_administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let watermark_durable =
        {
            let snapshot = session_database.read_snapshot().await.map_err(|error| {
                TraceDecayError::Config {
                    message: format!("host receipt session snapshot unavailable: {error}"),
                }
            })?;
            let mut rows = snapshot
                .query(
                    "SELECT 1
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2
                 LIMIT 1",
                    crate::db::engine::params!["hermes", ready.transcript_watermark.as_str()],
                )
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark query failed: {error}"),
                })?;
            rows.next()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark read failed: {error}"),
                })?
                .is_some()
        };
    if !watermark_durable {
        // Never review a terminal receipt until the exact completed-turn
        // watermark is durable in LCM.
        return Ok(());
    }
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval =
        registered_project_automation_retrieval(session_database, &profile_identity, &project_id)
            .await?;
    let backend = CodexAppServerBackend::from_automation_config(config);
    let host_run_id = format!("host_receipt_{}", pending.generation);
    let combined_options = CombinedReviewAutomationOptions {
        session_reflector: SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::HostReceipt,
            provider: "hermes".to_string(),
            session_id,
            ..SessionReflectorAutomationOptions::default()
        },
        skill_writer: SkillWriterAutomationOptions {
            trigger: AutomationTrigger::HostReceipt,
            provider: "hermes".to_string(),
            profile_root: Some(profile_identity.profile_root().to_path_buf()),
            ..SkillWriterAutomationOptions::default()
        },
        trigger: AutomationTrigger::HostReceipt,
        ..CombinedReviewAutomationOptions::default()
    };
    let admission = super::combined_effect::prepare_combined_effects(
        engine,
        cg,
        run_control,
        project_path,
        &dashboard_root,
        Some(&host_run_id),
        configuration.configuration_digest.clone(),
        &combined_options,
    )
    .await?;
    let mut first_error = None;
    let outcome = super::combined_effect::run_combined_scheduler_effect(
        admission,
        engine,
        cg,
        &project_id,
        project_path,
        config,
        &configuration.configuration_revision_id,
        &backend,
        retrieval.as_ref(),
        combined_options,
        &mut first_error,
    )
    .await;
    if let Some(error) = first_error {
        return Err(error);
    }
    if outcome.completed() {
        tracedecay_agent_hosts::automation::host_receipts::mark_consumed(
            &dashboard_root,
            &pending.session_key,
            pending.generation,
        )
        .await?;
    } else if !outcome.handled() {
        log_daemon_event(
            "host_receipt_review",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "deferred".to_string()),
                ("reason", "not_combined".to_string()),
            ],
        );
    }
    Ok(())
}
