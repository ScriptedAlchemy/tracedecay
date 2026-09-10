//! Root ordering for durable automation-effect admission.
//!
//! The composition root admits the retained request through daemon-service
//! and resolves the project-memory owner. Settlement, retries, and journal
//! recovery live in `tracedecay-automation-runtime`.

use std::path::Path;

use tracedecay_automation_runtime::automation::effect_runtime::contract_error;
use tracedecay_automation_runtime::automation::effect_runtime::settlement::{
    AdmittedAutomationEffectRequest, AutomationEffectAdmission, AutomationEffectAuthority,
    observe_admission_decision,
};
use tracedecay_contracts::retained_surfaces::{
    AutomationRunRequestV1, RetainedSurfaceOperation, retained_surface_application_operation,
};
use tracedecay_contracts::{ApplicationProblemEnvelope, CancellationSignal, Deadline, RequestId};
use tracedecay_daemon_service::{DaemonInvocationService, RegisteredRetainedRequestContextError};
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::Result;

use crate::tracedecay::TraceDecay;

pub(crate) mod recovery_composition;

#[cfg(test)]
#[path = "automation_effect/journal/tests.rs"]
mod journal_tests;

/// Admits the retained request, resolves the memory owner, and hands those
/// authorities to the runtime settlement kernel.
#[allow(clippy::too_many_arguments)]
#[hotpath::skip]
pub(crate) async fn prepare(
    invocation: &DaemonInvocationService,
    memory: &TraceDecay,
    project_root: &Path,
    dashboard_root: &Path,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: &CancellationSignal,
    observed_at: UtcMicros,
    configuration_digest: ManifestDigest,
    request: AutomationRunRequestV1,
) -> Result<AutomationEffectAdmission> {
    if !request.validate() {
        return Err(contract_error("automation run identity is empty"));
    }
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .map_err(contract_error)?;
    let context = match invocation
        .registered_retained_request_context(
            project_root,
            request_id.clone(),
            deadline,
            cancellation.context(),
            observed_at,
            &operation,
        )
        .await
    {
        Ok(context) => context,
        Err(RegisteredRetainedRequestContextError::Application(problem)) => {
            let envelope = ApplicationProblemEnvelope::new(
                operation.result_contract().clone(),
                request_id,
                problem,
            )
            .map_err(contract_error)?;
            let admission = AutomationEffectAdmission::PreAdmissionProblem(envelope);
            observe_admission_decision(&admission);
            return Ok(admission);
        }
        Err(RegisteredRetainedRequestContextError::Runtime(error)) => return Err(error),
    };
    // Runtime prepare retains journal and authority state; keep that frame
    // out of scheduler and dashboard callers.
    Box::pin(AutomationEffectAuthority::prepare(
        AdmittedAutomationEffectRequest {
            context,
            cancellation: cancellation.clone(),
            observed_at,
            configuration_digest,
            request,
            dashboard_root: dashboard_root.to_path_buf(),
        },
        || memory.project_memory_owner(),
        |run_id, read_control| async move {
            memory
                .project_memory_application()
                .map_err(|error| {
                    contract_error(format!(
                        "canonical memory automation receipt recovery failed: {error}"
                    ))
                })?
                .project_memory_automation_run_receipts(run_id, &read_control)
                .await
                .map_err(|error| {
                    contract_error(format!(
                        "canonical memory automation receipt recovery failed: {error}"
                    ))
                })
        },
    ))
    .await
}
