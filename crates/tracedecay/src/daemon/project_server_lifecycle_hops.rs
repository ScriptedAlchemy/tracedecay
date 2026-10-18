//! The `project_server_lifecycle` entry points that native code awaits:
//! bootstrap, the connection handshake and retained profile reads. That
//! module still polls Tokio timers, so each call here runs with the daemon's
//! Tokio runtime entered, in the caller's own task: the connection admission
//! scope stays in reach, and dropping the caller drops the call at once. Its
//! other entry points are awaited only inside Tokio tasks (project open,
//! retirements and the shutdown runner).

use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_daemon_service::ProfileHostAdmissionBootstrapStatus;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::native_runtime;

use super::{StoreAdministration, project_server_lifecycle};

/// A miss is counted and logged by `tokio_handle`; history is left retained.
pub(super) async fn cancel_retained_session_history(administration: &StoreAdministration) {
    if let Some(cancel) = native_runtime::on_tokio(
        "daemon.lifecycle.cancel_retained_session_history",
        project_server_lifecycle::cancel_retained_session_history(administration),
    ) {
        cancel.await;
    }
}

/// `None` when no Tokio runtime ran the scheduling; that miss is counted.
pub(super) async fn schedule_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Option<ProfileHostAdmissionBootstrapStatus> {
    native_runtime::on_tokio(
        "daemon.lifecycle.schedule_profile_host_admission_replay",
        project_server_lifecycle::schedule_user_profile_host_admission_replay_for_identity(
            administration,
            identity,
        ),
    )?
    .await
}

pub(super) async fn await_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Result<()> {
    native_runtime::on_tokio(
        "daemon.lifecycle.await_profile_host_admission_replay",
        project_server_lifecycle::await_user_profile_host_admission_replay_for_identity(
            administration,
            identity,
        ),
    )
    .ok_or_else(|| {
        TraceDecayError::project_route(
            "tokio_runtime_unavailable",
            false,
            "the daemon has no Tokio runtime for the profile host admission replay",
        )
    })?
    .await
}
