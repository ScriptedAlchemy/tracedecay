use std::fs;
use std::process::Command;
use tempfile::TempDir;
use tracedecay_configuration::{TraceDecayConfig, get_config_path, save_config_to_path};
use tracedecay_semantic_contracts::DEFAULT_FASTEMBED_MODEL_ID;

#[test]
fn semantic_defaults_cover_the_cataloged_fastembed_model() {
    let config = TraceDecayConfig::default();
    let catalog = tracedecay_semantic::production_fastembed_catalog();
    let model = catalog
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .expect("default semantic model is cataloged");
    let model_bytes = model.members.get("model").expect("model member").length;
    assert!(config.semantic.resources.max_model_bytes >= model_bytes);
    // The shipped configuration pins no resident ceiling: composition derives
    // it from the host's admitted process memory.
    assert_eq!(config.semantic.resources.max_resident_bytes, None);
    assert_eq!(
        config.semantic.resources.max_concurrent_sessions,
        tracedecay_semantic::embedding_parallelism::default_max_concurrent_sessions(),
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_does_not_open_registry_only_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();

    let gdb =
        crate::test_support::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();

    let project_id = "proj_identity_only";
    gdb.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(tracedecay_global_db::StoreInstanceUpsert {
        store_id: "store_identity_only".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();

    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    assert!(
        super::discover_project_root(&project_root).is_none(),
        "sync discover_project_root must not see a global-only store"
    );

    assert!(
        super::discover_project_root_with_identity(&project_root)
            .await
            .is_none(),
        "process-local discovery must leave registry-only aliases to the daemon"
    );
    let nested = project_root.join("crates/inner");
    fs::create_dir_all(&nested).unwrap();
    assert!(
        super::discover_project_root_with_identity(&nested)
            .await
            .is_none(),
        "nested discovery must not open the global registry"
    );

    let bare = TempDir::new().unwrap();
    let bare_root = bare.path().canonicalize().unwrap();
    assert!(
        super::discover_project_root_with_identity(&bare_root)
            .await
            .is_none(),
        "a directory with no store must not resolve"
    );
}

#[tokio::test]
async fn config_path_with_identity_does_not_open_registry_without_enrollment() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
    let gdb =
        crate::test_support::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();
    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    let project_id = "proj_config_identity";
    let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(&project_root);
    gdb.upsert_code_project(
        project_id,
        &project_root,
        git_common_dir.as_deref(),
        None,
        None,
    )
    .await
    .unwrap();
    gdb.upsert_store_instance(tracedecay_global_db::StoreInstanceUpsert {
        store_id: "store_config_identity".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    let identity_layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    save_config_to_path(
        &identity_layout.config_path,
        &TraceDecayConfig {
            root_dir: "identity-config".to_string(),
            ..TraceDecayConfig::default()
        },
    )
    .unwrap();

    assert_eq!(
        super::get_config_path_with_identity(&project_root).await,
        get_config_path(&project_root)
    );
    assert_eq!(
        super::load_config_with_identity(&project_root)
            .await
            .unwrap()
            .root_dir,
        project_root.to_string_lossy()
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_does_not_bind_non_git_child_to_parent_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
    let gdb =
        crate::test_support::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let parent_dir = TempDir::new().unwrap();
    let parent_root = parent_dir.path().canonicalize().unwrap();
    let project_id = "proj_parent_identity_only";
    gdb.upsert_code_project(project_id, &parent_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(tracedecay_global_db::StoreInstanceUpsert {
        store_id: "store_parent_identity_only".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &parent_root,
        &profile_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let child = parent_root.join("scratch/deep");
    fs::create_dir_all(&child).unwrap();

    assert_eq!(
        super::discover_project_root_with_identity(&child).await,
        None,
        "non-git scratch directories must not inherit initialized parent stores"
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_preserves_sync_fast_path() {
    let _profile = super::PinnedUserDataDir::new();
    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();

    let db_dir = super::get_tracedecay_dir(&project_root);
    fs::create_dir_all(&db_dir).unwrap();
    fs::write(super::get_project_db_path(&project_root), b"").unwrap();

    let sync = super::discover_project_root(&project_root);
    assert!(sync.is_some(), "sync resolver must see a repo-local db");
    assert_eq!(
        super::discover_project_root_with_identity(&project_root).await,
        sync,
        "identity wrapper fast path must equal the sync result"
    );
}

mod runtime_configuration_cutover {
    #[cfg(unix)]
    use std::process::Command;

    use std::collections::BTreeMap;

    use tempfile::TempDir;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationGrantId, ConfigurationGrantReceiptId,
        ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationEffectV1,
        ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
        ConfigurationMutationSinkV1, ConfigurationRevisionId, ConfigurationValueV1,
        DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
        SOURCE_BINDINGS_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY, ScopeSourceBinding, SettingKey,
        SourceBindingId,
    };
    use tracedecay_domain::{AccessPolicyDigest, ActorId, ProjectId, UtcMicros};

    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::{ConfigurationLayerV1, resolve_configuration};
    use crate::config::{
        DaemonRuntimeConfiguration, RuntimeConfigurationCache, RuntimeConfigurationTarget,
        cached_runtime_configuration, cached_sync_config, cached_telemetry_config,
        install_pinned_runtime_configuration, runtime_configuration_for_layout,
    };
    use crate::test_support::host_admission::HostAdmissionTestRuntimeV1;
    use tracedecay_configuration::ProjectConfigurationRuntime;
    use tracedecay_configuration::TraceDecayConfig;
    use tracedecay_global_db::configuration::contracts::{
        ConfigurationControlStore, ConfigurationMutationAuthority, DirectConfigurationMutation,
    };

    fn project_id(value: &str) -> ProjectId {
        ProjectId::new(value.to_owned()).expect("fixture project id is canonical")
    }

    fn revision_id(value: &str) -> ConfigurationRevisionId {
        ConfigurationRevisionId::new(value).expect("fixture revision id is canonical")
    }

    #[test]
    fn cached_runtime_reads_ignore_legacy_input_after_publication() {
        let project_id = project_id("project.runtime-cache-only");
        let root = TempDir::new().expect("temporary project root");
        // Publish with an explicit auto-watch=true settings layer so the
        // published-snapshot-wins assertion below stays valid regardless of
        // the registry default's polarity.
        let snapshot = resolve_configuration(
            &ConfigurationRegistry::core().expect("registry is available"),
            &[ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: project_id.clone(),
                },
                revision_id: revision_id("revision.runtime-cache-only.settings"),
                entries: BTreeMap::from([(
                    SettingKey::new(SYNC_AUTO_WATCH_SETTING_KEY).expect("auto-watch setting key"),
                    ConfigurationValueV1::Boolean(true),
                )]),
            }],
        )
        .expect("explicit settings layer resolves")
        .snapshot;
        let pinned = DaemonRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id,
                project_root: root.path().to_path_buf(),
            },
            revision_id("revision.runtime-cache-only"),
            snapshot,
        )
        .expect("default snapshot materializes");
        install_pinned_runtime_configuration(pinned);

        let legacy_dir = root.path().join(".tracedecay");
        std::fs::create_dir_all(&legacy_dir).expect("create legacy fixture directory");
        std::fs::write(
            legacy_dir.join("config.json"),
            r#"{"root_dir":"/legacy","telemetry":{"timings":false},"sync":{"auto_watch":false}}"#,
        )
        .expect("write conflicting legacy input");

        assert!(
            cached_telemetry_config(root.path())
                .expect("cache lookup")
                .timings,
            "hook-safe telemetry lookup must use the published snapshot"
        );
        assert!(
            cached_sync_config(root.path())
                .expect("cache lookup")
                .auto_watch,
            "hook-safe sync lookup must use the published snapshot"
        );
        assert_eq!(
            cached_runtime_configuration(root.path())
                .expect("cache lookup")
                .config
                .root_dir,
            root.path().to_string_lossy().to_string(),
            "root metadata comes from the non-authoritative published route"
        );
    }

    #[test]
    fn runtime_cache_retargets_legacy_root_metadata_per_cached_root() {
        let project_id = project_id("project.runtime-cache-retarget");
        let root = TempDir::new().expect("temporary project root");
        let first_root = root.path().join("first-worktree");
        let second_root = root.path().join("second-worktree");
        std::fs::create_dir_all(&first_root).expect("create first root");
        std::fs::create_dir_all(&second_root).expect("create second root");
        let snapshot = resolve_configuration(
            &ConfigurationRegistry::core().expect("registry is available"),
            &[],
        )
        .expect("defaults resolve")
        .snapshot;
        let revision_id = revision_id("revision.runtime-cache-retarget");
        let cache = RuntimeConfigurationCache::default();
        cache.insert(
            DaemonRuntimeConfiguration::new(
                RuntimeConfigurationTarget {
                    project_id: project_id.clone(),
                    project_root: first_root.clone(),
                },
                revision_id.clone(),
                snapshot.clone(),
            )
            .expect("first snapshot materializes"),
        );
        cache.insert(
            DaemonRuntimeConfiguration::new(
                RuntimeConfigurationTarget {
                    project_id: project_id.clone(),
                    project_root: second_root.clone(),
                },
                revision_id,
                snapshot,
            )
            .expect("second snapshot materializes"),
        );

        let first = cache.for_root(&first_root).expect("first root lookup");
        let second = cache.for_root(&second_root).expect("second root lookup");
        assert_eq!(first.target().project_id, project_id);
        assert_eq!(second.target().project_id, project_id);
        assert_eq!(first.target().project_root, first_root);
        assert_eq!(second.target().project_root, second_root);
        assert_ne!(first.config.root_dir, second.config.root_dir);
    }

    #[tokio::test]
    async fn runtime_current_reads_the_store_after_startup_snapshot_drifts() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        let project_id = project_id("project.configuration-runtime-drift");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let host_runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");
        let database = host_runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("bind registered project database");
        crate::config::install_usecase_runtime_configuration_authority()
            .expect("install the root runtime configuration read ports");
        let (_, opened) = crate::config::open_runtime_configuration_for_registered_database(
            root.path(),
            &layout,
            database,
        )
        .await
        .expect("open runtime configuration")
        .into_parts();
        let (runtime, startup) =
            ProjectConfigurationRuntime::open(opened).expect("open project configuration runtime");
        let mutation = DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            key: SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap(),
            value: Box::new(ConfigurationValueV1::Boolean(true)),
        };
        let authority = ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                ConfigurationGrantReceiptId::new("configuration.grant-receipt.drift").unwrap(),
                ConfigurationGrantId::new("configuration.grant.drift").unwrap(),
                ActorId::new("actor.configuration-runtime-drift").unwrap(),
                ConfigurationMutationOperationV1::DirectMutation,
                mutation.target_scope_digest().unwrap(),
                startup.revision_id().clone(),
                1,
                AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                Some(
                    ConfigurationIdempotencyKey::new("configuration.idempotency.runtime-drift")
                        .unwrap(),
                ),
                UtcMicros(1),
                UtcMicros(100),
            )
            .unwrap(),
        };
        let store = runtime.configuration_store();
        let receipt = ConfigurationControlStore::commit_direct(
            &store,
            &authority,
            &mutation,
            startup.revision_id(),
        )
        .await
        .unwrap();

        let current = runtime.client().current().await.unwrap();
        assert_eq!(current.revision_id(), &receipt.result_revision_id);
        assert_ne!(current.revision_id(), startup.revision_id());
        assert!(!startup.config().diagnostics_prewarm);
        assert!(current.config().diagnostics_prewarm);
        assert_eq!(runtime.configuration_target(), current.target());
    }

    /// One real journey over the production read surfaces: open (as the
    /// lifecycle does), cached reads through the root cache, the lower cache
    /// port, and the dashboard read port, a committed configuration change
    /// published the way daemon settlement publishes it, and the same cached
    /// reads afterwards. The three surfaces must hand out the same revision
    /// and the same shared settings, while the daemon-only settings keep
    /// their exact values.
    #[tokio::test]
    async fn open_cached_read_and_configuration_change_share_one_runtime_pin() {
        #[cfg(feature = "hotpath")]
        let _hotpath = hotpath::HotpathGuardBuilder::new("configuration-runtime-pin-journey")
            .sections(vec![hotpath::Section::FunctionsTiming])
            .build();
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        let project_id = project_id("project.configuration-shared-pin-journey");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let host_runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");
        let database = host_runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("bind registered project database");
        crate::config::install_usecase_runtime_configuration_authority()
            .expect("install the root runtime configuration read ports");

        let (config, opened) = crate::config::open_runtime_configuration_for_registered_database(
            root.path(),
            &layout,
            database,
        )
        .await
        .expect("open runtime configuration")
        .into_parts();
        let (runtime, startup) =
            ProjectConfigurationRuntime::open(opened).expect("open project configuration runtime");
        assert!(!config.diagnostics_prewarm);
        assert_eq!(config.max_file_size, startup.config().max_file_size);
        assert_eq!(config.semantic, startup.config().semantic);

        let cached_reads = || {
            let root_pin = cached_runtime_configuration(root.path()).expect("root cached read");
            let lower_pin =
                tracedecay_configuration::config::cached_pinned_runtime_configuration(root.path())
                    .expect("lower cached read");
            let dashboard_pin =
                tracedecay_dashboard_api::config::cached_runtime_configuration(root.path())
                    .expect("dashboard cached read");
            (root_pin, lower_pin, dashboard_pin)
        };
        let (root_pin, lower_pin, dashboard_pin) = cached_reads();
        for pin in [&lower_pin, &dashboard_pin] {
            assert_eq!(pin.revision_id(), startup.revision_id());
            assert_eq!(pin.snapshot().snapshot_id, startup.snapshot().snapshot_id);
            assert_eq!(pin.config(), startup.config());
        }
        assert_eq!(root_pin.revision_id(), startup.revision_id());
        assert!(!root_pin.config().diagnostics_prewarm);
        assert_eq!(
            root_pin.config().sync.auto_watch,
            TraceDecayConfig::default().sync.auto_watch,
            "daemon-only settings materialize from the same snapshot"
        );

        let mutation = DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            key: SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap(),
            value: Box::new(ConfigurationValueV1::Boolean(true)),
        };
        let authority = ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                ConfigurationGrantReceiptId::new("configuration.grant-receipt.shared-pin").unwrap(),
                ConfigurationGrantId::new("configuration.grant.shared-pin").unwrap(),
                ActorId::new("actor.configuration-shared-pin").unwrap(),
                ConfigurationMutationOperationV1::DirectMutation,
                mutation.target_scope_digest().unwrap(),
                startup.revision_id().clone(),
                1,
                AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                Some(
                    ConfigurationIdempotencyKey::new("configuration.idempotency.shared-pin")
                        .unwrap(),
                ),
                UtcMicros(1),
                UtcMicros(100),
            )
            .unwrap(),
        };
        let store = runtime.configuration_store();
        let receipt = ConfigurationControlStore::commit_direct(
            &store,
            &authority,
            &mutation,
            startup.revision_id(),
        )
        .await
        .expect("commit the configuration change");
        let current = runtime
            .client()
            .current()
            .await
            .expect("read the committed revision");
        assert_eq!(current.revision_id(), &receipt.result_revision_id);
        assert!(current.config().diagnostics_prewarm);
        tracedecay_configuration::config::publish_pinned_runtime_configuration(current)
            .expect("publish the committed revision to the runtime cache");

        let (root_pin, lower_pin, dashboard_pin) = cached_reads();
        for pin in [&lower_pin, &dashboard_pin] {
            assert_eq!(pin.revision_id(), &receipt.result_revision_id);
            assert!(pin.config().diagnostics_prewarm);
        }
        assert_eq!(root_pin.revision_id(), &receipt.result_revision_id);
        assert!(root_pin.config().diagnostics_prewarm);
        assert_eq!(
            root_pin.config().sync.auto_watch,
            TraceDecayConfig::default().sync.auto_watch,
            "an unrelated change must not disturb daemon-only settings"
        );
        assert_eq!(
            root_pin.config().sync.retention,
            tracedecay_configuration::RetentionConfig::default()
        );
    }

    #[tokio::test]
    async fn ensure_runtime_configuration_persists_initial_resolution_when_cache_is_empty() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            "proj_ensure_runtime_bootstrap",
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        // Write the opposite of the typed registry default so the stale input
        // stays distinguishable from the canonical resolution regardless of
        // the default's polarity.
        let stale_auto_watch = !TraceDecayConfig::default().sync.auto_watch;
        std::fs::write(
            &layout.config_path,
            format!(r#"{{"sync":{{"auto_watch":{stale_auto_watch}}},"max_file_size":7}}"#),
        )
        .expect("write stale config.json input");

        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_err(),
            "fail-closed lookup must reject an unpublished project"
        );

        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_ensure_runtime_bootstrap"),
        )
        .await
        .expect("open retained project runtime");
        let pinned = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("cold open persists and publishes a resolved revision");
        assert_eq!(
            pinned.target().project_id.as_str(),
            "proj_ensure_runtime_bootstrap"
        );
        assert_eq!(
            pinned.revision_id().as_str(),
            "configuration.initial.canonical.v1",
            "fresh stores publish the sole canonical initial revision"
        );
        assert_eq!(
            pinned.config.sync.auto_watch,
            TraceDecayConfig::default().sync.auto_watch,
            "stale config.json input must not enter the final configuration authority"
        );
        assert_eq!(
            pinned.config.max_file_size,
            TraceDecayConfig::default().max_file_size,
            "fresh initialization uses the typed registry, not config.json"
        );
        assert!(
            layout.sessions_db_path.is_file(),
            "initial resolution must be committed to the retained project store"
        );

        let reopened = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("reopen loads the durable current revision");
        assert_eq!(reopened.revision_id(), pinned.revision_id());
        assert_eq!(reopened.snapshot(), pinned.snapshot());
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_ok(),
            "after ensure, fail-closed lookup must see the published pin"
        );
    }

    #[tokio::test]
    async fn existing_snapshot_converges_new_native_graph_default_before_materialization() {
        use tracedecay_runtime_core::db::engine::params;

        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        let project_id = project_id("proj_configuration_native_graph_default_upgrade");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id,
        )
        .await
        .expect("open retained project runtime");
        let initial = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("seed canonical configuration");
        let database = runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("bind registered project database");
        let setting = SettingKey::new(INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY)
            .expect("native graph setting key");
        let mut values = initial.snapshot().effective_values.clone();
        let mut provenance = initial.snapshot().provenance.clone();
        values.remove(&setting);
        provenance.remove(&setting);
        let pre_key_snapshot =
            tracedecay_domain::configuration::ConfigurationSnapshotV1::new(values, provenance)
                .expect("pre-key snapshot remains internally canonical");

        let transaction = database
            .begin_write_transaction()
            .await
            .expect("open fixture transaction");
        transaction
            .execute("DROP TRIGGER configuration_entries_immutable_delete", ())
            .await
            .expect("open immutable entry fixture seam");
        transaction
            .execute("DROP TRIGGER configuration_revisions_immutable_update", ())
            .await
            .expect("open immutable revision fixture seam");
        transaction
            .execute(
                "DELETE FROM configuration_entries WHERE revision_id = ?1 AND key = ?2",
                params![initial.revision_id().as_str(), setting.as_str()],
            )
            .await
            .expect("remove post-snapshot setting from fixture");
        transaction
            .execute(
                "UPDATE configuration_revisions
                 SET snapshot_id = ?2,
                     effective_behavior_digest = ?3,
                     resolution_provenance_digest = ?4
                 WHERE revision_id = ?1",
                params![
                    initial.revision_id().as_str(),
                    pre_key_snapshot.snapshot_id.as_str(),
                    pre_key_snapshot.effective_behavior_digest.as_str(),
                    pre_key_snapshot.resolution_provenance_digest.as_str(),
                ],
            )
            .await
            .expect("bind fixture revision to pre-key snapshot identity");
        transaction
            .execute(
                "CREATE TRIGGER configuration_entries_immutable_delete
                 BEFORE DELETE ON configuration_entries
                 BEGIN SELECT RAISE(ABORT, 'configuration entries are immutable'); END",
                (),
            )
            .await
            .expect("restore immutable entry trigger");
        transaction
            .execute(
                "CREATE TRIGGER configuration_revisions_immutable_update
                 BEFORE UPDATE ON configuration_revisions
                 BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END",
                (),
            )
            .await
            .expect("restore immutable revision trigger");
        transaction.commit().await.expect("commit pre-key fixture");

        let converged = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("registered default must converge before runtime materialization");
        assert_ne!(converged.revision_id(), initial.revision_id());
        assert!(converged.config.native_graph_activation);
        assert_eq!(
            converged.snapshot().effective_values.get(&setting),
            Some(&ConfigurationValueV1::Boolean(true))
        );
        let reopened = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("converged revision reopens without another migration");
        assert_eq!(reopened.revision_id(), converged.revision_id());
    }

    #[tokio::test]
    async fn ensure_runtime_configuration_rejects_a_revision_without_the_registered_binding() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        let project_id = project_id("proj_runtime_binding_required");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id,
        )
        .await
        .expect("open retained project runtime");
        let database = runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("bind registered project database");
        let store =
            tracedecay_global_db::configuration::GlobalDbConfigurationControlStore::new_registered(
                database.as_ref(),
            );
        let revision_id = revision_id("configuration.invalid.without-binding");
        let resolution = resolve_configuration(
            &ConfigurationRegistry::core().expect("configuration registry"),
            &[],
        )
        .expect("resolve registry defaults without a project binding");
        store
            .initialize_canonical(&revision_id, &resolution, UtcMicros(1))
            .await
            .expect("seed exact final schema with incompatible configuration data");

        let error = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect_err("missing registered source binding must not be repaired");
        assert!(matches!(
            error,
            tracedecay_domain::errors::TraceDecayError::ResetRequired { ref authority, .. }
                if authority == "configuration"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_runtime_configuration_keeps_binding_revision_across_linked_worktrees() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary root");
        let primary = root.path().join("primary");
        let linked = root.path().join("linked");
        std::fs::create_dir_all(&primary).expect("create primary root");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "TraceDecay Test")
                .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
                .env("GIT_COMMITTER_NAME", "TraceDecay Test")
                .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&primary, &["init", "-b", "main", "--quiet"]);
        std::fs::write(primary.join("README.md"), "primary\n").expect("fixture");
        git(&primary, &["add", "README.md"]);
        git(&primary, &["commit", "-m", "fixture", "--quiet"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "feature/linked",
                linked.to_str().expect("linked path"),
                "HEAD",
            ],
        );

        let project_id = project_id("proj_runtime_linked_binding");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &primary,
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout = tracedecay_runtime_core::storage::resolve_layout_for_current_profile(&primary)
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            &primary,
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");

        let primary_configuration = runtime
            .ensure_runtime_configuration_for_test(&primary, &layout)
            .await
            .expect("open primary configuration");
        let linked_configuration = runtime
            .ensure_runtime_configuration_for_test(&linked, &layout)
            .await
            .expect("open linked configuration");
        let reopened_primary = runtime
            .ensure_runtime_configuration_for_test(&primary, &layout)
            .await
            .expect("reopen primary configuration");

        assert_eq!(
            linked_configuration.revision_id(),
            primary_configuration.revision_id(),
            "linked open must not rebind the shared repository authority"
        );
        assert_eq!(
            reopened_primary.revision_id(),
            primary_configuration.revision_id(),
            "returning to the primary must not repair linked-worktree churn"
        );
        assert_eq!(
            tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
                &project_id,
                &primary,
            )
            .expect("primary binding"),
            tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
                &project_id,
                &linked,
            )
            .expect("linked binding"),
        );
    }

    /// Moving or renaming a checkout changes only the path-derived locator
    /// digest; the registry still resolves the same registered project. The
    /// open path must republish the daemon binding with the new digest as a
    /// durable revision instead of demanding a reset.
    #[tokio::test]
    async fn ensure_runtime_configuration_rebinds_locator_digest_for_a_renamed_checkout() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary root");
        let original = root.path().join("checkout");
        let renamed = root.path().join("checkout-renamed");
        std::fs::create_dir_all(&original).expect("create original checkout");
        let project_id = project_id("proj_runtime_rebind_rename");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &original,
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(&original)
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            &original,
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");
        let initial = runtime
            .ensure_runtime_configuration_for_test(&original, &layout)
            .await
            .expect("cold open publishes the canonical initial revision");

        // The whole checkout moves on disk: same registered project and
        // store, new canonical root, therefore a new derived locator digest.
        std::fs::rename(&original, &renamed).expect("rename checkout");

        let healed = runtime
            .ensure_runtime_configuration_for_test(&renamed, &layout)
            .await
            .expect("registry-verified rename must rebind the locator digest, not reset");
        assert_ne!(
            healed.revision_id(),
            initial.revision_id(),
            "the rebind must republish a new durable revision"
        );
        let reopened = runtime
            .ensure_runtime_configuration_for_test(&renamed, &layout)
            .await
            .expect("reopen after the rebind");
        assert_eq!(
            reopened.revision_id(),
            healed.revision_id(),
            "a rebound binding must be stable across reopens"
        );
    }

    /// Locator drift may heal only through the exact daemon-owned binding.
    /// A store whose single authority-matching binding carries a foreign
    /// binding id (and a store whose bindings belong to another project)
    /// must stay a typed reset, never a silent rebind.
    #[tokio::test]
    async fn ensure_runtime_configuration_rejects_locator_drift_without_the_daemon_binding() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary root");
        let checkout = root.path().join("checkout");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&checkout).expect("create checkout");
        std::fs::create_dir_all(&elsewhere).expect("create foreign locator root");
        let other_project = project_id("proj_runtime_rebind_other");
        let project_id = project_id("proj_runtime_rebind_denied");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &checkout,
            project_id.as_str(),
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(&checkout)
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            &checkout,
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");
        let database = runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("bind registered project database");
        let store =
            tracedecay_global_db::configuration::GlobalDbConfigurationControlStore::new_registered(
                database.as_ref(),
            );

        // One binding belongs to a different registered project; the other
        // matches this project's authority but carries a foreign binding id
        // and a drifted locator digest.
        let other_binding =
            tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
                &other_project,
                &checkout,
            )
            .expect("build other-project binding");
        let drifted =
            tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
                &project_id,
                &elsewhere,
            )
            .expect("build drifted daemon binding");
        let foreign = ScopeSourceBinding::new(
            SourceBindingId::new("binding.operator.project-open".to_owned())
                .expect("foreign binding id"),
            drifted.source_kind,
            drifted.source_locator_digest,
            AuthorityRef::Project(project_id.clone()),
        )
        .expect("build foreign-id binding");
        let source_bindings_key =
            SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).expect("source bindings setting key");
        let seeded_revision = revision_id("configuration.seeded.foreign-binding");
        let resolution = resolve_configuration(
            &ConfigurationRegistry::core().expect("configuration registry"),
            &[ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: project_id.clone(),
                },
                revision_id: seeded_revision.clone(),
                entries: BTreeMap::from([(
                    source_bindings_key,
                    ConfigurationValueV1::SourceBindings(vec![foreign, other_binding]),
                )]),
            }],
        )
        .expect("resolve seeded bindings");
        store
            .initialize_canonical(&seeded_revision, &resolution, UtcMicros(1))
            .await
            .expect("seed store with foreign bindings");

        let error = runtime
            .ensure_runtime_configuration_for_test(&checkout, &layout)
            .await
            .expect_err("locator drift without the daemon binding id must stay a reset");
        assert!(matches!(
            error,
            tracedecay_domain::errors::TraceDecayError::ResetRequired { ref authority, .. }
                if authority == "configuration"
        ));
    }

    #[tokio::test]
    async fn resolve_runtime_configuration_pins_registered_project_when_cache_is_cold() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            "proj_resolve_cold_cache",
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");

        // A freshly registered project has no pinned snapshot in this process's
        // cache — exactly the state a daemon is in for a project it has not yet
        // opened, or for any project after a restart. The fail-closed hook-path
        // lookup rejects it.
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_err(),
            "cold cache must fail the fail-closed lookup before an on-demand resolve"
        );

        // The daemon authority path resolves and pins on demand instead of
        // erroring, so branch administration and other daemon operations run.
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_resolve_cold_cache"),
        )
        .await
        .expect("open retained project runtime");
        let pinned = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("daemon resolve pins a registered project on demand");
        assert_eq!(
            pinned.target().project_id.as_str(),
            "proj_resolve_cold_cache"
        );

        // After the resolve, even the fail-closed lookup sees the published pin,
        // so a subsequent daemon operation no longer hits the cold-cache error.
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_ok(),
            "on-demand resolve must publish a pin the fail-closed lookup can read"
        );

        // A second resolve is idempotent and returns the same authority.
        let reresolved = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("second daemon resolve reuses the published pin");
        assert_eq!(reresolved.revision_id(), pinned.revision_id());
        assert_eq!(reresolved.snapshot(), pinned.snapshot());
    }

    #[tokio::test]
    async fn resolve_runtime_configuration_errors_typed_when_authority_is_unresolvable() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            "proj_resolve_unresolvable",
        )
        .expect("write enrollment marker");
        let mut layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");

        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_resolve_unresolvable"),
        )
        .await
        .expect("open retained project runtime");
        // Strip the authoritative project identity: a layout with no project id
        // has no configuration authority to resolve, and on-demand resolution
        // must surface a typed error rather than fabricate one.
        layout.identity.project_id = None;

        let error = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect_err("a layout without project identity has no resolvable authority");
        assert!(
            matches!(
                error,
                tracedecay_domain::errors::TraceDecayError::Config { .. }
            ),
            "genuine unavailability must stay a typed configuration error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn read_only_open_rejects_an_uninitialized_store_without_fabricated_defaults() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            root.path(),
            "proj_read_only_uninitialized",
        )
        .expect("write enrollment marker");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root.path())
                .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        if let Some(parent) = layout.sessions_db_path.parent() {
            std::fs::create_dir_all(parent).expect("create sessions db parent");
        }

        // Materialize the durable store schema without ever seeding a
        // configuration revision — the state a consolidated destination store is
        // left in after a repository move, when its configuration authority was
        // never migrated in.
        let runtime = HostAdmissionTestRuntimeV1::project(
            tracedecay_runtime_core::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_read_only_uninitialized"),
        )
        .await
        .expect("open retained project runtime");
        let error = runtime
            .load_runtime_configuration_read_only_for_test(root.path(), &layout)
            .await
            .expect_err("read-only open must not fabricate a configuration revision");
        assert!(
            matches!(
                error,
                tracedecay_domain::errors::TraceDecayError::ResetRequired { ref authority, .. }
                    if authority == "configuration"
            ),
            "uninitialized durable configuration must remain a typed reset state: {error:?}"
        );
    }
}
