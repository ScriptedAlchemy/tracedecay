//! Registered project-open activation for durable automatic-effect recovery.

use std::sync::Arc;

use tracedecay_application::CancellationSignal;

use crate::daemon::log_daemon_event;
use crate::tracedecay::TraceDecay;

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
    match crate::daemon::automation_effect::reconcile_reserved_automation_effects_for_project(
        project.as_ref(),
        &dashboard_root,
        &cancellation,
    )
    .await
    {
        Ok(report) => log_daemon_event(
            "automation_effect_recovery",
            &[
                (
                    "outcome",
                    if report.deferred == 0 {
                        "completed"
                    } else {
                        "deferred"
                    }
                    .to_owned(),
                ),
                ("inspected", report.inspected.to_string()),
                ("partial_effects", report.partial_effects.to_string()),
                ("reset_required", report.reset_required.to_string()),
                ("already_terminal", report.already_terminal.to_string()),
                ("deferred", report.deferred.to_string()),
            ],
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
