//! Closed CLI binding for daemon-owned Work application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no work state, scheduling, retry, provider, or persistence logic, the
//! CLI is one more caller of the same daemon invocation the HTTP mount, the
//! dashboard, and the generated SDKs already use.

use serde_json::Value;
use tracedecay_api::WorkOperation;
use tracedecay_contracts::{
    ApplicationResult, ResultContractRef, work_executable_binding_registry,
};
use tracedecay_tool_catalog::OperationId;

use tracedecay_daemon_protocol::DaemonInvocationDelivery;
use tracedecay_daemon_protocol::{WorkApplicationInvocationV1, WorkApplicationOutcomeV1};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::application_cli::{WORK, config_error};

pub(crate) const WORK_CLI_DEADLINE_MILLIS: u64 = 120_000;

/// A Work response plus the authenticated connection on which its terminal
/// delivery acknowledgement must be sent. The handle is intentionally kept
/// alive until the presentation layer has written and flushed stdout.
pub struct WorkCliResponse {
    pub outcome: ApplicationResult<Value>,
    pub(crate) delivery: Option<WorkCliDelivery>,
}

impl WorkCliResponse {
    pub(crate) fn without_delivery(outcome: ApplicationResult<Value>) -> Self {
        Self {
            outcome,
            delivery: None,
        }
    }

    pub fn take_delivery(&mut self) -> Option<WorkCliDelivery> {
        self.delivery.take()
    }
}

/// Authenticated terminal delivery handle for one daemon Work response.
pub struct WorkCliDelivery {
    delivery: DaemonInvocationDelivery,
}

impl WorkCliDelivery {
    pub(crate) fn new(delivery: DaemonInvocationDelivery) -> Self {
        Self { delivery }
    }

    /// Acknowledge only after the caller's output write and flush succeeded.
    pub async fn acknowledge_delivered(self) -> Result<()> {
        self.delivery
            .acknowledge(
                tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
                None,
            )
            .await
    }

    /// Record a terminal disconnected/drop outcome when the caller's output
    /// boundary fails. This must never be converted into Delivered.
    pub async fn acknowledge_dropped(
        self,
        reason: tracedecay_domain::DeliveryDropReasonV1,
    ) -> Result<()> {
        self.delivery
            .acknowledge(
                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                Some(reason),
            )
            .await
    }
}

/// Every Work operation this build accepts by route segment, for typed
/// rejection messages.
pub fn work_operation_segments() -> Vec<&'static str> {
    WorkOperation::ALL
        .iter()
        .map(|operation| operation.route_segment())
        .collect()
}

pub(crate) fn work_result_contract(operation: WorkOperation) -> Result<ResultContractRef> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = work_executable_binding_registry().map_err(config_error)?;
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return Err(TraceDecayError::Config {
            message: format!(
                "Work operation {} is not advertised by this build (valid operations: {})",
                operation_id.as_str(),
                work_operation_segments().join(", ")
            ),
        });
    };
    Ok(ResultContractRef::from_schema(
        binding.result_schema().schema_ref(),
    ))
}

pub(crate) fn decode_work_invocation(
    operation: WorkOperation,
    body: Value,
) -> Result<WorkApplicationInvocationV1> {
    WorkApplicationInvocationV1::decode(operation.operation_key(), body)
        .map_err(|error| WORK.decode_error(error))
}

/// Whether the daemon holds this Work terminal open for a delivery ACK.
pub(crate) fn work_delivery_is_eligible(
    operation: WorkOperation,
    outcome: &WorkApplicationOutcomeV1,
) -> bool {
    match (operation, outcome) {
        (WorkOperation::StartAttempt, WorkApplicationOutcomeV1::StartAttempt(outcome))
        | (WorkOperation::AttemptStatus, WorkApplicationOutcomeV1::AttemptStatus(outcome))
        | (WorkOperation::CancelAttempt, WorkApplicationOutcomeV1::CancelAttempt(outcome)) => {
            outcome.payload().is_some()
        }
        (WorkOperation::HydrateArtifacts, WorkApplicationOutcomeV1::HydrateArtifacts(outcome)) => {
            outcome.payload().is_some_and(|hydration| {
                matches!(
                    hydration,
                    tracedecay_contracts::WorkArtifactHydrationV1::Hydrated { attempts, .. }
                        if !attempts.is_empty()
                )
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use tracedecay_api::WorkOperation;

    use super::{decode_work_invocation, work_operation_segments, work_result_contract};

    /// Every mounted Work operation must be reachable from the CLI by its route
    /// segment, and must decode to the daemon invocation variant that names it.
    #[test]
    fn every_work_operation_parses_from_its_route_segment_and_dispatches() {
        let mut seen = BTreeSet::new();
        for operation in WorkOperation::ALL {
            let segment = operation.route_segment();
            assert!(
                seen.insert(segment),
                "duplicate Work route segment {segment}"
            );
            let parsed = segment
                .parse::<WorkOperation>()
                .unwrap_or_else(|error| panic!("Work segment {segment} must parse: {error}"));
            assert_eq!(parsed, operation);

            // The CLI resolves a result contract for every operation, so no
            // operation can be advertised by the parser but unmounted here.
            work_result_contract(operation)
                .unwrap_or_else(|error| panic!("Work contract for {segment}: {error}"));

            // Every operation reaches its own daemon invocation variant, and a
            // body that does not match the operation contract is refused before
            // dispatch rather than sent to the daemon.
            let invocation = decode_work_invocation(operation, json!({}));
            let refused = decode_work_invocation(operation, json!({"unexpected": true}))
                .err()
                .map(|error| error.to_string());
            match invocation {
                Ok(invocation) => assert_eq!(
                    invocation.operation_key(),
                    operation.operation_key(),
                    "Work segment {segment} dispatched to the wrong invocation variant"
                ),
                Err(error) => assert!(
                    error.to_string().contains("invalid typed Work request"),
                    "Work segment {segment} must fail with a typed decode problem: {error}"
                ),
            }
            if let Some(refused) = refused {
                assert!(
                    refused.contains("invalid typed Work request"),
                    "Work segment {segment} must reject unknown fields: {refused}"
                );
            }
        }
        assert_eq!(seen.len(), WorkOperation::ALL.len());
    }

    #[test]
    fn an_unknown_work_segment_is_refused_with_the_valid_operations() {
        let error = "not-a-work-operation"
            .parse::<WorkOperation>()
            .expect_err("unknown Work segment must be refused");
        assert!(error.contains("unknown Work operation route segment"));
        for segment in work_operation_segments() {
            assert!(
                error.contains(segment),
                "unknown-operation error must list {segment}: {error}"
            );
        }
    }

    #[test]
    fn proposal_routes_refuse_dispositions_owned_by_the_other_route() {
        for (operation, disposition) in [
            (WorkOperation::ReviewProposal, "accepted"),
            (WorkOperation::AcceptProposal, "rejected"),
        ] {
            let error = decode_work_invocation(operation, json!({ "disposition": disposition }))
                .expect_err("the route must reject the other disposition");
            assert!(
                error.to_string().contains("unknown variant"),
                "{} accepted {disposition}: {error}",
                operation.operation_key()
            );
        }
    }

    #[test]
    fn daemon_work_reset_remains_a_typed_cli_problem() {
        use super::WORK;
        use tracedecay_contracts::ApplicationProblem;
        use tracedecay_daemon_protocol::DaemonInvocationProblem;

        let problem = WORK.daemon_problem(DaemonInvocationProblem::ResetRequired);
        let ApplicationProblem::ResetRequired {
            diagnostic,
            retry,
            legal_actions,
            ..
        } = problem
        else {
            panic!("work reset must remain a typed reset-required problem");
        };
        assert_eq!(diagnostic.code, "work_authority_reset_required");
        assert_eq!(retry, tracedecay_contracts::RetryDirective::Never);
        assert_eq!(
            legal_actions,
            vec![tracedecay_contracts::LegalAction::Reset]
        );
    }
}
