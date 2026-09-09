//! Project authority composition for runtime-owned automation recovery.
//!
//! The root orders runtime preparation before opening project memory, then
//! supplies the admitted project identity, scope, and memory authority.

use std::path::Path;

use tracedecay_automation_runtime::automation::effect_runtime::{
    AutomationEffectRecoveryPreparation, AutomationEffectRecoveryReport,
    prepare_reserved_automation_effect_recovery, reconcile_prepared_automation_effects_for_project,
};
use tracedecay_contracts::CancellationSignal;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::Result;
use tracedecay_session_memory::fact_store::DatabaseFactStore;
use tracedecay_session_memory::memory::MemoryApplication;

use crate::tracedecay::TraceDecay;

pub(crate) async fn reconcile_reserved_automation_effects_for_project(
    project: &TraceDecay,
    dashboard_root: &Path,
    cancellation: &CancellationSignal,
) -> Result<AutomationEffectRecoveryReport> {
    let preparation =
        prepare_reserved_automation_effect_recovery(dashboard_root, cancellation).await?;
    let preparation = match preparation {
        AutomationEffectRecoveryPreparation::Complete(report) => return Ok(report),
        AutomationEffectRecoveryPreparation::Pending(preparation) => preparation,
    };
    let context = project.automation_project_context()?;
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        project.project_root(),
        context.project_id(),
    )
    .map_err(|error| {
        tracedecay_automation_runtime::automation::effect_runtime::contract_error(format!(
            "automation recovery scope is invalid: {error:?}"
        ))
    })?;
    let memory = MemoryApplication::new(
        FactOwnerV1::Project {
            project_id: context.project_id().clone(),
        },
        DatabaseFactStore::new(&context.project_memory_database),
    )
    .map_err(|error| {
        tracedecay_automation_runtime::automation::effect_runtime::contract_error(format!(
            "automation recovery memory authority is invalid: {error}"
        ))
    })?;
    reconcile_prepared_automation_effects_for_project(preparation, &memory, cancellation, &scope)
        .await
}
