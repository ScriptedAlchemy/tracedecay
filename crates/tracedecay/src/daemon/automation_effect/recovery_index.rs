//! Project composition for runtime-owned automation recovery.

use std::path::Path;

use tracedecay_automation_runtime::automation::effect_runtime::{
    AutomationEffectRecoveryReport, reconcile_reserved_automation_effects_for_project as reconcile,
};
use tracedecay_contracts::CancellationSignal;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::Result;

use crate::tracedecay::TraceDecay;

pub(crate) async fn reconcile_reserved_automation_effects_for_project(
    project: &TraceDecay,
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
) -> Result<AutomationEffectRecoveryReport> {
    let owner = project.project_memory_owner()?;
    let FactOwnerV1::Project { project_id } = &owner else {
        return Err(
            tracedecay_automation_runtime::automation::effect_runtime::contract_error(
                "automation recovery requires a project owner",
            ),
        );
    };
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        project.project_root(),
        project_id,
    )
    .map_err(|error| {
        tracedecay_automation_runtime::automation::effect_runtime::contract_error(format!(
            "automation recovery scope is invalid: {error:?}"
        ))
    })?;
    let memory = project.project_memory_application().await?;
    reconcile(&memory, dashboard_root, cancellation, &scope).await
}
