//! Configuration registrar coverage.

use super::*;

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
    );
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
        assert!(
            authority
                .issue_direct(
                    &format!("request.configuration.exact.{index}"),
                    &mutation,
                    expected_revision.clone(),
                    UtcMicros(1),
                )
                .is_ok()
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
                &foreign,
                expected_revision.clone(),
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
