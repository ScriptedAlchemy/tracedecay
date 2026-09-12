//! Registered project-open activation for durable automatic-effect recovery.

use std::sync::Arc;

use tracedecay_automation_runtime::automation::effect_recovery::recovery_report_fields;
use tracedecay_contracts::CancellationSignal;

use crate::project::TraceDecay;
use tracedecay_runtime_core::logging::log_daemon_event;

#[hotpath::measure(label = "daemon.project.automation_recovery", future = true)]
pub(crate) async fn reconcile_project_open_automation_effects(project: Arc<TraceDecay>) {
    let cancellation = match CancellationSignal::active(format!(
        "cancellation.project-open.automation-effect-recovery.{}",
        project
            .hook_store_layout()
            .identity
            .project_id
            .as_deref()
            .unwrap_or("unregistered")
    )) {
        Ok(cancellation) => cancellation,
        Err(error) => {
            log_daemon_event(
                "automation_effect_recovery",
                &[
                    ("outcome", "invalid_cancellation".to_owned()),
                    ("error", error.to_string()),
                ],
            );
            return;
        }
    };
    let dashboard_root = project.hook_store_layout().dashboard_root.clone();
    match crate::daemon::automation_effect::recovery_composition::reconcile_reserved_automation_effects_for_project(
        project.as_ref(),
        &dashboard_root,
        &cancellation,
    )
    .await
    {
        Ok(report) => log_daemon_event(
            "automation_effect_recovery",
            &recovery_report_fields(&report),
        ),
        Err(error) => log_daemon_event(
            "automation_effect_recovery",
            &[
                ("outcome", "error".to_owned()),
                ("error", error.to_string()),
            ],
        ),
    }
}
