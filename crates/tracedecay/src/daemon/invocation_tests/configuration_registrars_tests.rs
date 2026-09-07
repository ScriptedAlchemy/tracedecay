//! Configuration registrar coverage.

use std::collections::BTreeMap;

use super::*;
use tracedecay_configuration::{DirectConfigurationMutation, configuration_layer_scope_digest};
use tracedecay_daemon_service::*;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationRevisionId,
};

#[tokio::test]
async fn read_only_project_configuration_requires_the_bootstrap_profile_plan() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let (graph, runtime) =
        crate::tracedecay::TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.configuration.read-only-worker-plan",
        )
        .await
        .expect("registered graph");
    graph.close();
    let read_only = runtime
        .open_project_graph_read_only_for_test(
            project.path(),
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(
                    tracedecay_runtime_core::storage::default_profile_root()
                        .expect("default profile root"),
                ),
                global_db_path: None,
            },
        )
        .await
        .expect("read-only project graph");
    assert!(!read_only.db().is_writable());

    let invocation = crate::daemon::invocation_state::DaemonInvocationState::default();
    let profile_root =
        tracedecay_runtime_core::storage::default_profile_root().expect("default profile root");
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("profile identity");
    let profile_sessions = runtime
        .session_registry_for_test()
        .profile_sessions()
        .await
        .expect("profile sessions authority");
    invocation
        .install_profile_worker_plan(profile_sessions, profile_identity.profile_id())
        .await
        .expect("daemon bootstrap worker plan");
    invocation
        .configuration_runtime_registrar()
        .ensure_worker_plan()
        .expect("read-only project worker-plan boundary");

    assert!(tracedecay_code_index::parallelism::installed_worker_status().is_some());
}

#[test]
fn direct_configuration_grants_reject_foreign_caller_selected_layers() {
    let exact_project = tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
        project_id: ProjectId::new("project.configuration.exact").expect("project"),
    };
    let exact_profile = tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
        profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.exact")
            .expect("profile"),
    };
    let exact_collection = tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
        collection_id: tracedecay_domain::QueryCollectionId::new("collection.configuration.exact")
            .expect("collection"),
    };
    let authority = DaemonConfigurationGrantAuthority::for_test(
        [
            exact_project.clone(),
            exact_profile.clone(),
            exact_collection.clone(),
        ],
        UtcMicros(100),
    )
    .expect("test configuration grant authority");
    let expected_revision =
        ConfigurationRevisionId::new("configuration.revision.exact").expect("revision");

    for (index, layer) in [exact_project, exact_profile, exact_collection]
        .into_iter()
        .enumerate()
    {
        let mutation = DirectConfigurationMutation::Unset {
            layer,
            key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                .expect("setting"),
        };
        let grant = authority
            .issue_direct(
                &format!("request.configuration.exact.{index}"),
                ConfigurationIdempotencyKey::new(format!(
                    "configuration.idempotency.exact-{index}"
                ))
                .expect("idempotency key"),
                &mutation,
                expected_revision.clone(),
                UtcMicros(50),
                UtcMicros(1),
            )
            .expect("exact layer grant");
        assert_eq!(
            grant.receipt.expires_at,
            UtcMicros(50),
            "the accepted execution deadline must cap the registrar lifetime"
        );
    }

    for (index, layer) in [
        tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
            project_id: ProjectId::new("project.configuration.foreign").expect("project"),
        },
        tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
            profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.foreign")
                .expect("profile"),
        },
        tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
            collection_id: tracedecay_domain::QueryCollectionId::new(
                "collection.configuration.foreign",
            )
            .expect("collection"),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let foreign = DirectConfigurationMutation::Unset {
            layer,
            key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                .expect("setting"),
        };
        assert!(matches!(
            authority.issue_direct(
                &format!("request.configuration.foreign.{index}"),
                ConfigurationIdempotencyKey::new(format!(
                    "configuration.idempotency.foreign-{index}"
                ))
                .expect("idempotency key"),
                &foreign,
                expected_revision.clone(),
                UtcMicros(50),
                UtcMicros(1),
            ),
            Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
        ));
    }
}

#[test]
fn mounted_configuration_layers_exclude_stale_collection_provenance() {
    use tracedecay_domain::configuration::{
        CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationSnapshotV1,
        ConfigurationValueV1,
    };

    let project_id = ProjectId::new("project.configuration.mounted").expect("project");
    let profile_id =
        tracedecay_domain::UserProfileId::new("profile.configuration.mounted").expect("profile");
    let winning = tracedecay_domain::QueryCollectionId::new("collection.configuration.winning")
        .expect("collection");
    let overridden =
        tracedecay_domain::QueryCollectionId::new("collection.configuration.overridden")
            .expect("collection");
    let rejected = tracedecay_domain::QueryCollectionId::new("collection.configuration.rejected")
        .expect("collection");
    let key =
        tracedecay_domain::configuration::SettingKey::new("sync.auto_watch").expect("setting");
    let revision =
        ConfigurationRevisionId::new("configuration.revision.mounted").expect("revision");
    let candidate = |collection_id, disposition| ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Collection { collection_id },
        revision_id: revision.clone(),
        disposition,
        safe_reason: None,
    };
    let snapshot = ConfigurationSnapshotV1::new(
        BTreeMap::from([(key.clone(), ConfigurationValueV1::Boolean(true))]),
        BTreeMap::from([(
            key,
            vec![
                candidate(winning.clone(), CandidateDispositionV1::Winning),
                candidate(overridden.clone(), CandidateDispositionV1::Overridden),
                candidate(rejected.clone(), CandidateDispositionV1::Rejected),
            ],
        )]),
    )
    .expect("snapshot");

    let mounted =
        mounted_configuration_layers(&project_id, &profile_id, &snapshot).expect("layers");
    let contains = |layer: ConfigurationLayerIdV1| {
        let digest = configuration_layer_scope_digest(&layer).expect("digest");
        mounted.get(&digest) == Some(&layer)
    };
    assert!(contains(ConfigurationLayerIdV1::Collection {
        collection_id: winning,
    }));
    assert!(!contains(ConfigurationLayerIdV1::Collection {
        collection_id: overridden,
    }));
    assert!(!contains(ConfigurationLayerIdV1::Collection {
        collection_id: rejected,
    }));
}
