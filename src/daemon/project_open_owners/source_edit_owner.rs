use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::{
    ApplicationOperation, CancellationContext, Deadline, RequestContext, RequestId, now_micros,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};

use super::{POLICY_REVISION_V1, ProjectOpenSourceEditAuthorizationV1};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::McpServer;
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

pub(super) fn source_edit_request_context(
    access: &ProjectSourceAccessSnapshot,
    request_id: RequestId,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext> {
    if cancellation.is_cancelled() || deadline.is_elapsed_at(observed_at) {
        return Err(source_edit_authority_error());
    }
    let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
    if expires_at.0 <= observed_at.0 {
        return Err(source_edit_authority_error());
    }
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.source-edit-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        operation.capability_id(),
        operation.use_case_id(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("source edit route grant unavailable: {error}"),
    })?;
    let grant = tracedecay_application::CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!(
            "grant.daemon.source-edit.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(source_edit_contract_error)?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        expires_at,
        access.scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        tracedecay_application::DisclosureClass::Sensitive,
    )
    .map_err(source_edit_contract_error)?;
    RequestContext::new(
        access.requester.clone(),
        access.scope.clone(),
        grant,
        request_id,
        Deadline::new(expires_at).map_err(source_edit_contract_error)?,
        cancellation,
    )
    .map_err(source_edit_contract_error)
}

pub(super) fn source_edit_contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("source edit invocation contract is invalid: {error}"),
    }
}

pub(super) fn source_edit_authority_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit was not found or is not authorized".to_owned(),
    }
}

pub(super) fn source_edit_surface_result(
    result: tracedecay_usecases::edit::SourceEditApplicationResult,
) -> Result<tracedecay_application::source_edit::SourceEditSurfaceResultV1> {
    let replayed = result.replayed;
    let mut value = result.value();
    let object = value
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "source edit result did not serialize to its canonical object contract"
                .to_owned(),
        })?;
    object.insert("replayed".to_owned(), serde_json::Value::Bool(replayed));
    serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
        message: format!("source edit result violated its canonical surface contract: {error}"),
    })
}

pub(super) async fn invoke_project_open_source_edit_rollback(
    graph: Arc<crate::tracedecay::TraceDecay>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    invocation: crate::mcp::server::SourceEditRollbackInvocationV1,
) -> Result<tracedecay_application::source_edit::SourceEditSurfaceResultV1> {
    let observed_at = now_micros();
    let effect_control = tracedecay_usecases::edit::SourceEditEffectControlV1::new(
        invocation.deadline.clone(),
        invocation.cancellation.clone(),
    );
    let operation = tracedecay_application::source_edit_rollback_operation()
        .map_err(source_edit_contract_error)?;
    let access = authorization
        .current_access(observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let context = source_edit_request_context(
        &access,
        invocation.request_id,
        &operation,
        observed_at,
        invocation.deadline,
        invocation.cancellation.context(),
    )?;
    let current = authorization
        .current_authority(&context, &operation, observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let request = tracedecay_application::SourceEditRollbackRequestV1 {
        context,
        authority: current.receipt.clone(),
        effect_id: invocation.effect_id,
        original_idempotency_key: invocation.original_idempotency_key,
        idempotency_key: invocation.idempotency_key,
        original_input_digest: invocation.original_input_digest,
        expected_state: invocation.expected_state,
        proof: current.proof,
        observed_at,
    };
    tracedecay_usecases::edit::execute_source_edit_rollback_with_control(
        &*graph,
        &operation,
        request,
        &authorization,
        &effect_control,
    )
    .await
    .and_then(source_edit_surface_result)
}

pub(super) fn install_project_open_source_edit_rollback_owner(
    server: &McpServer,
    graph: Arc<crate::tracedecay::TraceDecay>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    mutation: Arc<super::SourceEditMutationGate>,
) -> Result<()> {
    server
        .install_source_edit_rollback_executor(Arc::new(move |request| {
            let graph = Arc::clone(&graph);
            let authorization = authorization.clone();
            let mutation = Arc::clone(&mutation);
            Box::pin(async move {
                mutation.authorize_mutation("rollback")?;
                invoke_project_open_source_edit_rollback(graph, authorization, request).await
            })
        }))
        .map_err(|_| TraceDecayError::Config {
            message: "project-open source edit rollback authority was already installed".to_owned(),
        })
}
