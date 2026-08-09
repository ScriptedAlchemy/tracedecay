//! Canonical daemon invocation constructors for the Git surface.

use tracedecay_application::{CancellationContext, Deadline};
use tracedecay_domain::UtcMicros;

use crate::application_surface::{
    GitApplySurfaceRequest, GitHubStackSignalExpandSurfaceRequest, GitPreviewSurfaceRequest,
    GitReadSurfaceRequest,
};

use super::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationPayload,
    DaemonInvocationRequest,
};

impl DaemonInvocationRequest {
    pub(crate) fn git_read(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
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

    pub(crate) fn git_preview(
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

    pub(crate) fn git_apply(
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
    pub(crate) fn github_stack_signal_expand(
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
