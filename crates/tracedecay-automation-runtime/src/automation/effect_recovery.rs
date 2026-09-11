//! Project-open recovery report fields for reserved automation effects.
//!
//! Reconcile composition (opening project memory) stays in the root.

use crate::automation::effect_runtime::AutomationEffectRecoveryReport;

pub fn recovery_report_fields(
    report: &AutomationEffectRecoveryReport,
) -> Vec<(&'static str, String)> {
    vec![
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
        ("indeterminate", report.indeterminate.to_string()),
        ("already_terminal", report.already_terminal.to_string()),
        ("deferred", report.deferred.to_string()),
    ]
}
