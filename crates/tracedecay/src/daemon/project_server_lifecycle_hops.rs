//! The `project_server_lifecycle` entry points that native code awaits:
//! bootstrap, the connection handshake and retained profile reads. That
//! module still polls Tokio timers, so each call here runs on the daemon's
//! Tokio runtime. Its other entry points are awaited only inside Tokio tasks
//! (project open, retirements and the shutdown runner). Each hop carries the
//! caller's connection admission, so a park inside still hands the slot back.

use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_daemon_service::ProfileHostAdmissionBootstrapStatus;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::native_runtime;

use super::{StoreAdministration, carry_connection_admission, project_server_lifecycle};

pub(super) async fn cancel_retained_session_history(administration: &StoreAdministration) {
    let administration = administration.clone();
    if let Err(error) = native_runtime::on_tokio(
        "daemon.lifecycle.cancel_retained_session_history",
        carry_connection_admission(async move {
            project_server_lifecycle::cancel_retained_session_history(&administration).await;
        }),
    )
    .await
    {
        tracing::error!(%error, "retained session history was not cancelled");
    }
}

/// `None` when no Tokio runtime ran the scheduling; that miss is counted.
pub(super) async fn schedule_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Option<ProfileHostAdmissionBootstrapStatus> {
    let (administration, identity) = (administration.clone(), identity.clone());
    native_runtime::on_tokio(
        "daemon.lifecycle.schedule_profile_host_admission_replay",
        carry_connection_admission(async move {
            project_server_lifecycle::schedule_user_profile_host_admission_replay_for_identity(
                &administration,
                &identity,
            )
            .await
        }),
    )
    .await
    .ok()
    .flatten()
}

pub(super) async fn await_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Result<()> {
    let (administration, identity) = (administration.clone(), identity.clone());
    native_runtime::on_tokio(
        "daemon.lifecycle.await_profile_host_admission_replay",
        carry_connection_admission(async move {
            project_server_lifecycle::await_user_profile_host_admission_replay_for_identity(
                &administration,
                &identity,
            )
            .await
        }),
    )
    .await
    .map_err(|error| TraceDecayError::project_route("tokio_runtime_unavailable", true, error))?
}
