//! Daemon-owned execution for the canonical project Observatory read.

use std::sync::Arc;

use super::*;

struct RegisteredObservatoryReadPort {
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    scope_ref: String,
}

impl tracedecay_application::ObservatoryReadPortV1 for RegisteredObservatoryReadPort {
    fn read<'a>(
        &'a self,
        request: tracedecay_application::ObservatoryReadRequestV1,
    ) -> tracedecay_application::ObservatoryReadFuture<'a> {
        Box::pin(async move {
            let since_seconds = request.since_seconds();
            let observatory = tracedecay_usecases::observability::observatory_read_model(
                self.database.as_ref(),
                Some(&self.scope_ref),
                since_seconds,
            )
            .await;
            let costs = tracedecay_usecases::observability::costs_read_model(
                self.database.as_ref(),
                None,
                None,
                Some(&self.scope_ref),
                since_seconds,
            )
            .await;
            Ok(tracedecay_application::ObservatoryReadResultV1 { observatory, costs })
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_observatory_read(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    request: tracedecay_application::ObservatoryReadRequestV1,
    resolved_scope: Option<ResolvedScope>,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let Some(registered) = service
        .project_runtimes
        .get::<RegisteredCallableCodeRuntime>(project_root)
        .await
    else {
        return observatory_unavailable(
            wire_request_id,
            "application.observatory.authority-unavailable",
        );
    };
    if !resolved_scope
        .as_ref()
        .is_none_or(|scope| scope == &registered.scope)
    {
        return concealed_application_problem(wire_request_id);
    }
    let Some(database) = service
        .project_runtimes
        .read::<RegisteredObservabilityProducerV1, _, _>(project_root, |runtime| runtime.database())
        .await
    else {
        return observatory_unavailable(
            wire_request_id,
            "application.observatory.store-unavailable",
        );
    };
    let access = match registered.authorization.current(observed_at).await {
        Ok(access) if access.scope == registered.scope => access,
        Ok(_) | Err(_) => return concealed_application_problem(wire_request_id),
    };
    let operation = match tracedecay_application::observatory_read_operation() {
        Ok(operation) => operation,
        Err(_) => {
            return observatory_unavailable(
                wire_request_id,
                "application.observatory.operation-unavailable",
            );
        }
    };
    let context = match callable_code_request_context(
        &registered.scope,
        &access,
        &wire_request_id,
        &operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let authorization = registered.authorization.authorize(access);
    let admission = match authorization.admit(&context, &operation, observed_at).await {
        Ok(admission) => admission,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let cancellation_signal = match tracedecay_application::CancellationSignal::active(
        context.cancellation().token_id.as_str(),
    ) {
        Ok(signal) => signal,
        Err(_) => return application_problem(wire_request_id, invalid_observatory_request()),
    };
    if let CancellationState::Cancelled { requested_at } = &context.cancellation().state {
        cancellation_signal.cancel(*requested_at);
    }
    let observatory_service =
        tracedecay_application::ObservatoryReadServiceV1::new(RegisteredObservatoryReadPort {
            database,
            scope_ref: registered.scope.project_id.as_str().to_owned(),
        });
    let read = observatory_service.read(request);
    tokio::pin!(read);
    let result = tokio::select! {
        result = &mut read => result,
        () = request_cancellation.cancelled() => {
            cancellation_signal.cancel(now_micros());
            read.await
        }
    };
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            return observatory_unavailable(
                wire_request_id,
                "application.observatory.read-unavailable",
            );
        }
    };
    let finished_at = current_micros();
    let authority = match authorization
        .recheck_publication(&context, &operation, &admission, finished_at)
        .await
    {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let result = match observatory_evidence(result, authority, observed_at, deadline) {
        Ok(result) => result,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    DaemonInvocationResponse::with_outcome(
        wire_request_id,
        DaemonInvocationOutcome::ObservatoryRead {
            scope: context.scope().clone(),
            result,
        },
    )
}

fn observatory_evidence(
    result: tracedecay_application::ObservatoryReadResultV1,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<DaemonFeedbackResult, ApplicationProblem> {
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid_observatory_request())?;
    let coverage = EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
        .map_err(|_| invalid_observatory_request())?;
    let page = PageState::first_page(
        SortContractId::new("sort.observatory.read.v1")
            .map_err(|_| invalid_observatory_request())?,
        1,
        Some(1),
        1,
    )
    .map_err(|_| invalid_observatory_request())?;
    let payload = serde_json::to_value(result).map_err(|_| invalid_observatory_request())?;
    Ok(DaemonFeedbackResult::from_application(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page,
        execution,
        payload: Some(payload),
    }))
}

fn invalid_observatory_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.observatory.invalid-request".to_owned(),
            message: "The Observatory read request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn observatory_unavailable(request_id: String, code: &str) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: code.to_owned(),
            message: "The project Observatory authority is unavailable".to_owned(),
        }),
    )
}
