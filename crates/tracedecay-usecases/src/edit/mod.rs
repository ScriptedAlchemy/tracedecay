use tracedecay_application::{
    ApplicationOperation, SourceEditAuthorizationPort, SourceEditEffectRequestV1,
    SourceEditReconciliationRequestV1, SourceEditRequest,
};
use tracedecay_domain::ManifestDigest;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

const JOURNAL_VERSION: u8 = 1;
const MAX_DURABLE_RECORD_BYTES: usize = 4 * 1024 * 1024;
const SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1: &str = "tracedecay.source-edit-state.v1";
const SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1: &str = "tracedecay.source-edit-recovery.v1";

mod control;
mod digest;
mod dispatch;
mod execute;
mod journal;
mod outcome;
mod reconcile;
mod records;
mod rollback;
mod verify;

#[cfg(test)]
mod test_support;

pub use control::SourceEditEffectControlV1;
pub use outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};

use execute::{execute_source_edit_inner, resolve_source_edit_preview};
use reconcile::reconcile_source_edit_effect_unknown_inner;
use rollback::execute_source_edit_rollback_inner;
use verify::config_error;

/// Capture the exact candidate-file CAS digest returned by a dry-run preview.
/// Apply callers must echo this digest; the executor independently repeats the
/// preview and recaptures state under its edit lock.
pub async fn preview_source_edit_expected_state(
    graph: &TraceDecay,
    code_graph: &dyn crate::graph::CodeGraphProjectionReadPort,
    context: &tracedecay_application::RequestContext,
    observed_at: tracedecay_domain::UtcMicros,
    edit: SourceEditRequest,
) -> Result<ManifestDigest> {
    let preview = resolve_source_edit_preview(
        graph,
        code_graph,
        context,
        observed_at,
        crate::graph::request_graph_cancellation(context),
        edit,
    )
    .await?;
    if !preview.outcome.success() {
        return Err(config_error(preview.outcome.message().to_owned()));
    }
    preview
        .expected_state
        .ok_or_else(|| config_error("source edit preview resolved no expected state"))
}

pub async fn execute_source_edit<A>(
    graph: &TraceDecay,
    code_graph: &dyn crate::graph::CodeGraphProjectionReadPort,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_inner(graph, code_graph, operation, request, authorization, None).await
}

pub async fn execute_source_edit_with_control<A>(
    graph: &TraceDecay,
    code_graph: &dyn crate::graph::CodeGraphProjectionReadPort,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
    control: &SourceEditEffectControlV1,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_inner(
        graph,
        code_graph,
        operation,
        request,
        authorization,
        Some(control),
    )
    .await
}

pub async fn execute_source_edit_rollback<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: tracedecay_application::SourceEditRollbackRequestV1,
    authorization: &A,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_rollback_inner(graph, operation, request, authorization, None).await
}

pub async fn execute_source_edit_rollback_with_control<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: tracedecay_application::SourceEditRollbackRequestV1,
    authorization: &A,
    control: &SourceEditEffectControlV1,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_rollback_inner(graph, operation, request, authorization, Some(control))
        .await
}

/// Resolve one retained `EffectUnknown` only after an authorized inspection
/// explicitly proves either the exact committed state or the exact rollback
/// state. A mismatch retains the journal and its uncertainty.
pub async fn reconcile_source_edit_effect_unknown_with_control<A>(
    graph: &TraceDecay,
    request: SourceEditReconciliationRequestV1,
    authorization: &A,
    control: &SourceEditEffectControlV1,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    reconcile_source_edit_effect_unknown_inner(graph, request, authorization, Some(control)).await
}
