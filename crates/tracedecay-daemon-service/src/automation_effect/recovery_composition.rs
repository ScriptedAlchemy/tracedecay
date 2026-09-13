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

use tracedecay_project::project::TraceDecay;

pub async fn reconcile_reserved_automation_effects_for_project(
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
    // External effects and durable terminals do not require project memory.
    let read_receipts = |run_id, read_control| {
        let owner = owner.clone();
        async move {
            let database = project.open_project_store_db()?;
            let memory = MemoryApplication::new(owner, DatabaseFactStore::new(&database)).map_err(
                |error| {
                    tracedecay_automation_runtime::automation::effect_runtime::contract_error(
                        format!("automation recovery memory authority is invalid: {error}"),
                    )
                },
            )?;
            memory
                .project_memory_automation_run_receipts(run_id, &read_control)
                .await
                .map_err(|error| {
                    tracedecay_automation_runtime::automation::effect_runtime::contract_error(
                        format!("canonical memory automation receipt recovery failed: {error}"),
                    )
                })
        }
    };
    reconcile_prepared_automation_effects_for_project(
        preparation,
        read_receipts,
        &owner,
        cancellation,
        &scope,
    )
    .await
}
