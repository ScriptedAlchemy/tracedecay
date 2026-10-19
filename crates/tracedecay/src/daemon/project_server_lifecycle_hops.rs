//! Project-server lifecycle entry points shared by native daemon tasks.

use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_daemon_service::ProfileHostAdmissionBootstrapStatus;
use tracedecay_domain::errors::Result;
use super::{StoreAdministration, project_server_lifecycle};

pub(super) async fn cancel_retained_session_history(administration: &StoreAdministration) {
    project_server_lifecycle::cancel_retained_session_history(administration).await;
}

pub(super) async fn schedule_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Option<ProfileHostAdmissionBootstrapStatus> {
    project_server_lifecycle::schedule_user_profile_host_admission_replay_for_identity(
        administration,
        identity,
    )
    .await
}

pub(super) async fn await_user_profile_host_admission_replay_for_identity(
    administration: &StoreAdministration,
    identity: &DaemonClientIdentity,
) -> Result<()> {
    project_server_lifecycle::await_user_profile_host_admission_replay_for_identity(
        administration,
        identity,
    )
    .await
}
