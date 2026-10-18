//! Canonical daemon invocation constructors for source-edit mutations.

use tracedecay_contracts::{
    CancellationContext, Deadline, SourceEditInvocationV1, SourceEditReconciliationInvocationV1,
    SourceEditRollbackInvocationV1,
};
use tracedecay_domain::UtcMicros;

use crate::contract::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationPayload,
    DaemonInvocationRequest,
};

impl DaemonInvocationRequest {
    pub fn source_edit(
        request_id: impl Into<String>,
        request: SourceEditInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SourceEdit {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn source_edit_reconcile(
        request_id: impl Into<String>,
        request: SourceEditReconciliationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SourceEditReconcile {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn source_edit_rollback(
        request_id: impl Into<String>,
        request: SourceEditRollbackInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SourceEditRollback {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }
}
