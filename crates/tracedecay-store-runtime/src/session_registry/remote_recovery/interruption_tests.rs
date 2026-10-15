use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tempfile::TempDir;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::recovery::RecoveryAuthorityExpectationV1;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::{
    AuthorityEpoch, ProjectId, RemotePlacementRevisionV1, RemoteWriterFenceV1, UtcMicros,
};
use tracedecay_rusqlite_runtime::remote::RemoteRecoveryPhysicalEffectErrorV1;
use tracedecay_store::RemoteWriterFenceInstallV1;

use super::support::{RecoveryRuntimeProbeV1, authority_key};
use super::{INTERRUPTION_CANCELLED, INTERRUPTION_DEADLINE, INTERRUPTION_NONE};
use crate::session_registry::DaemonSessionRuntimeRegistryV1;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fence_install_reports_runtime_interruptions_as_typed_recovery_errors() {
    let temp = TempDir::new().expect("fixture root");
    let profile_root = temp.path().join("profile");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        41,
        "remote recovery interruption",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_id = ProjectId::new("project.cancel-test").expect("project id");
    let project_root = temp.path().join("cancel-test");
    std::fs::create_dir_all(&project_root).expect("project root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let _sessions = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("project sessions mount registers the replay target");
    let replay = Arc::clone(&registry.remote_replay_transaction);
    let (target_binding, _) = replay
        .target_descriptor(&project_id)
        .expect("registered replay target");
    let current: RemoteWriterFenceV1 = serde_json::from_value(serde_json::json!({
        "brain_id": "brain.cancel-test",
        "shard_id": "shard.cancel-test",
        "generation_id": "generation.cancel-test",
        "placement_revision": 1,
        "authority_epoch": 1,
        "authority_node_id": "node.cancel-test",
    }))
    .expect("remote writer fence");
    let replacement = RemoteWriterFenceV1 {
        placement_revision: RemotePlacementRevisionV1::new(2).expect("placement revision"),
        authority_epoch: AuthorityEpoch(2),
        ..current.clone()
    };
    let expected = RecoveryAuthorityExpectationV1 {
        brain_id: current.brain_id.as_str().to_owned(),
        shard_id: current.shard_id.as_str().to_owned(),
        generation_id: current.generation_id.as_str().to_owned(),
        authority_node_id: current.authority_node_id.as_str().to_owned(),
        placement_revision: 1,
        authority_epoch: 1,
    };
    let install = RemoteWriterFenceInstallV1 {
        project_id: project_id.clone(),
        target_binding,
        authority_key: authority_key(&expected).expect("authority key"),
        expected: Some(current),
        replacement,
        installed_at: UtcMicros(1),
    };
    let request_id = RequestId::new("request.cancel-test").expect("request id");

    let outcomes = tokio::task::spawn_blocking(move || {
        [
            INTERRUPTION_CANCELLED,
            INTERRUPTION_DEADLINE,
            INTERRUPTION_NONE,
        ]
        .map(|interruption| {
            let probe =
                RecoveryRuntimeProbeV1::new(&request_id, Arc::new(AtomicU8::new(interruption)))
                    .expect("recovery probe");
            replay
                .install_writer_fence(project_id.clone(), install.clone(), Arc::new(probe))
                .err()
        })
    })
    .await
    .expect("fence install thread");

    assert_eq!(
        outcomes,
        [
            Some(RemoteRecoveryPhysicalEffectErrorV1::Cancelled),
            Some(RemoteRecoveryPhysicalEffectErrorV1::TimedOut),
            // No seeded fence row: the uninterrupted compare-and-swap fails,
            // and that failure is not an interruption.
            Some(RemoteRecoveryPhysicalEffectErrorV1::Unavailable),
        ]
    );
}
