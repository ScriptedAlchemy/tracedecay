//! Canonical daemon invocation constructors for the Git surface.

use tracedecay_application::{CancellationContext, Deadline};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::surface::GitReadSurfaceRequest;
use tracedecay_application::git::GitHubStackSignalExpandSurfaceRequest;
use tracedecay_application::git::{GitApplySurfaceRequest, GitPreviewSurfaceRequest};

use super::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationPayload,
    DaemonInvocationRequest,
};

impl DaemonInvocationRequest {
    pub fn git_read(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: GitReadSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn git_preview(
        request_id: impl Into<String>,
        request: GitPreviewSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn git_apply(
        request_id: impl Into<String>,
        request: GitApplySurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitApply {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    /// Expands one admitted durable GitHub stack signal with daemon-minted
    /// actor, scope, and capability-grant authority.
    pub fn github_stack_signal_expand(
        request_id: impl Into<String>,
        request: GitHubStackSignalExpandSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitHubStackSignalExpand {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }
}
