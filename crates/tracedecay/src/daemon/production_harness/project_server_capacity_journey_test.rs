use tempfile::TempDir;

use super::journey_test_support::git;
use super::*;
use crate::daemon::project_composition::ProductionProjectComposition;
use tracedecay_code_index_runtime::code_index_scheduler::LatestCompleteCodeIndexV1;

async fn open_project_composition(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    instance: &str,
) -> Result<ProductionProjectComposition> {
    let resources = harness
        .resources
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "production harness is shut down".to_owned(),
        })?;
    let handshake = DaemonHandshake {
        client_version: binary_version()?.to_owned(),
        client_instance_id: instance.to_owned(),
        client_identity: DaemonClientIdentity {
            profile_root: harness.profile_root.clone(),
            global_db_path: harness.profile_root.join("global.db"),
        },
        scope_prefix: None,
        project_path: Some(project.to_path_buf()),
        timings: false,
        allow_init: true,
        allow_initialize_root_routing: false,
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
    };
    let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
    resources
        .store_administration
        .with_writer(|| async {
            production_project_server(
                &resources.store_administration,
                &resources._project_open_gates,
                &resources.invocation,
                &resources.http_application_registry,
                &canonical_project_path,
                &handshake,
                ProductionProjectCompositionRuntime::Portable {
                    semantic_auto_download: false,
                    startup_catch_up: false,
                },
                &CancellationToken::new(),
                None,
            )
            .await
        })
        .await
}

async fn open_project(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    instance: &str,
) -> Result<(ProductionProjectComposition, LatestCompleteCodeIndexV1)> {
    let resources = harness
        .resources
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "production harness is shut down".to_owned(),
        })?;
    let composition = open_project_composition(harness, project, instance).await?;
    let code_search_scope = {
        let graph = composition.server.cg().await;
        let target = graph.configuration_runtime().configuration_target();
        tracedecay_code_index_runtime::resolved_scope_for_project(
            graph.project_root(),
            &target.project_id,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("capacity-journey code-index scope is invalid: {error:?}"),
        })?
    };
    let latest = super::wait_for_production_composition_code_index(
        &resources.invocation,
        &composition.canonical_project_path,
        &code_search_scope,
    )
    .await?
    .ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "capacity-journey project '{}' has extractable sources but published no generation",
            composition.canonical_project_path.display()
        ),
    })?;
    Ok((composition, latest))
}

async fn seed_project_sessions_pending_convergence(
    profile_root: &Path,
    project_root: &Path,
    project_id: &tracedecay_domain::ProjectId,
) {
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(profile_root)
        .expect("durable harness profile identity");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        project_root,
        project_id.as_str(),
    )
    .expect("target project enrollment");
    let sessions_path = tracedecay_runtime_core::storage::profile_sharded_data_root(
        profile_root,
        project_id.as_str(),
    )
    .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
    std::fs::create_dir_all(sessions_path.parent().expect("session database parent"))
        .expect("session database directory");
    tracedecay_store_runtime::register_registered_schema_installer();
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &sessions_path,
        "seed production project-open convergence fixture",
    )
    .expect("project sessions fixture database authority");
    let (database, _) = tracedecay_runtime_core::db::Database::publish_registered_test_runtime_for_profile_identity(
        &sessions_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        tracedecay_runtime_core::db::TestRuntimeProfileIdentityV1::new(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
        ),
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await
    .expect("seed complete registered project sessions schema");
    database
        .execute_write_batch(
            "remove production project-open convergence checkpoint",
            "DELETE FROM authority_audit_checkpoints",
        )
        .await
        .expect("remove durable convergence checkpoint");
}

fn assert_generation_contains_probe(latest: &LatestCompleteCodeIndexV1, probe: &str) {
    let symbols = &latest.generation().symbols().symbols;
    assert!(
        !symbols.is_empty(),
        "the latest-complete generation must contain extracted symbols"
    );
    assert!(
        symbols.iter().any(|symbol| symbol.simple_name == probe),
        "the latest-complete generation must contain the unique project symbol {probe}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_full_publication_precedes_registered_schema_convergence() {
    let isolation = TempDir::new().expect("production harness isolation");
    let bootstrap_project = isolation.path().join("bootstrap-project");
    let target_project = isolation.path().join("target-project");
    for (project, probe) in [
        (&bootstrap_project, "bootstrap_probe"),
        (&target_project, "target_probe"),
    ] {
        std::fs::create_dir_all(project.join("src")).expect("project source root");
        std::fs::write(
            project.join("src/lib.rs"),
            format!("pub fn {probe}() -> usize {{ 1 }}\n"),
        )
        .expect("project source");
        git(project, &["init", "-q"]);
        git(project, &["add", "."]);
        git(project, &["config", "user.name", "TraceDecay Test"]);
        git(
            project,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        git(project, &["commit", "-qm", "seed project"]);
    }

    let harness = ProductionProjectCompositionHarnessV1::open_with_session_maintenance_for_test(
        isolation.path(),
        std::iter::once(bootstrap_project),
    )
    .await
    .expect("production harness authority");
    let target_project_id =
        tracedecay_domain::ProjectId::new("project.schema-convergence-full-publication")
            .expect("typed target project identity");
    seed_project_sessions_pending_convergence(
        harness.profile_root(),
        &target_project,
        &target_project_id,
    )
    .await;

    let resources = harness
        .resources
        .as_ref()
        .expect("production harness resources");
    let registry = resources
        .store_administration
        .session_runtime_registry()
        .await
        .expect("session runtime registry");
    let convergence_gate = registry.block_registered_schema_convergence_for_test();
    let mut project_open = Box::pin(open_project_composition(
        &harness,
        &target_project,
        "foreground-convergence",
    ));
    let composition = tokio::select! {
        result = &mut project_open => result.expect("target project full publication"),
        () = convergence_gate.wait_until_blocked() => {
            panic!("historical schema convergence entered before full project publication")
        }
    };
    drop(project_open);

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        convergence_gate.wait_until_blocked(),
    )
    .await
    .expect("historical convergence starts after full project publication");
    convergence_gate.release();
    drop(composition);
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twelve_project_journey_retires_idle_owners_without_empty_graphs() {
    let isolation = TempDir::new().expect("production harness isolation");
    let mut projects = Vec::new();
    for ordinal in 0..12 {
        let project = isolation.path().join(format!("project-{ordinal}"));
        std::fs::create_dir_all(project.join("src")).expect("project source root");
        std::fs::write(
            project.join("src/lib.rs"),
            format!("pub fn project_{ordinal}_probe() -> usize {{ {ordinal} }}\n"),
        )
        .expect("project source");
        git(&project, &["init", "-q"]);
        git(&project, &["add", "."]);
        git(&project, &["config", "user.name", "TraceDecay Test"]);
        git(
            &project,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        git(&project, &["commit", "-qm", "seed project"]);
        projects.push(project);
    }

    let mut harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        std::iter::once(projects[0].clone()),
    )
    .await
    .expect("production harness authority");
    let first_root = projects[0].canonicalize().expect("canonical first project");
    // The harness retains one convenience handle for every setup project. A
    // sequential CLI connection releases that handle after its response, so
    // remove only this fixture-owned initial client before the real journey.
    let initial_client = harness
        .resources
        .as_mut()
        .expect("production harness resources")
        .servers
        .remove(&first_root)
        .expect("harness retains its initial client handle");
    drop(initial_client);

    let mut replay_roots = Vec::new();
    for (ordinal, project) in projects.iter().enumerate() {
        let probe = format!("project_{ordinal}_probe");
        let (opened, latest) = open_project(&harness, project, &format!("initial-{ordinal}"))
            .await
            .expect("a settled sequential client must release capacity for the next project");
        assert_generation_contains_probe(&latest, &probe);
        let graph = opened.server.cg().await;
        let replay_root = graph.hook_store_layout().data_root.clone();
        assert!(
            crate::daemon::hook_v2_replay::hook_v2_replay_consumer_registered(&replay_root),
            "an open project must retain its Hook V2 replay consumer"
        );
        replay_roots.push((opened.canonical_project_path.clone(), replay_root));
        drop(opened);
    }

    let initial_cached_projects = {
        let resources = harness
            .resources
            .as_ref()
            .expect("production harness resources");
        let servers = resources
            .store_administration
            .project_servers()
            .lock()
            .await;
        servers
            .servers
            .keys()
            .map(|key| key.project_root.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let initial_cached_owner_count = initial_cached_projects.len();
    assert!(
        (2..=MAX_CACHED_PROJECT_SERVERS).contains(&initial_cached_owner_count),
        "graph pressure must preserve a useful multi-project cache: {initial_cached_owner_count}"
    );
    for (project, replay_root) in &replay_roots {
        assert_eq!(
            crate::daemon::hook_v2_replay::hook_v2_replay_consumer_registered(replay_root),
            initial_cached_projects.contains(project),
            "Hook V2 replay liveness must match exact project-server retention for {}",
            project.display()
        );
    }

    for (ordinal, project) in projects.iter().enumerate() {
        let probe = format!("project_{ordinal}_probe");
        let (opened, latest) = open_project(&harness, project, &format!("reopen-{ordinal}"))
            .await
            .expect("retired project must reopen through production composition");
        let (cached, cached_latest) = open_project(&harness, project, &format!("cached-{ordinal}"))
            .await
            .expect("immediate reopen must reuse the cached project");
        assert!(
            Arc::ptr_eq(&opened.server, &cached.server),
            "a route-local reopen must reuse the cached server"
        );
        assert_generation_contains_probe(&latest, &probe);
        assert_generation_contains_probe(&cached_latest, &probe);
        let graph = opened.server.cg().await;
        assert!(
            crate::daemon::hook_v2_replay::hook_v2_replay_consumer_registered(
                &graph.hook_store_layout().data_root,
            ),
            "reopening a retired project must restore its Hook V2 replay consumer"
        );
    }
    {
        let resources = harness
            .resources
            .as_ref()
            .expect("production harness resources");
        let servers = resources
            .store_administration
            .project_servers()
            .lock()
            .await;
        for project in [&projects[0], &projects[1]] {
            let canonical = project.canonicalize().expect("canonical uncached project");
            assert!(
                servers
                    .servers
                    .keys()
                    .all(|key| key.project_root != canonical),
                "the concurrent admission fixture must start with an uncached route"
            );
        }
    }
    let (left, right) = tokio::join!(
        open_project(&harness, &projects[0], "concurrent-left"),
        open_project(&harness, &projects[1], "concurrent-right"),
    );
    let (_left, left_latest) = left.expect("first concurrent uncached project admission");
    let (_right, right_latest) = right.expect("second concurrent uncached project admission");
    assert_generation_contains_probe(&left_latest, "project_0_probe");
    assert_generation_contains_probe(&right_latest, "project_1_probe");
    let cached_owner_count = harness
        .resources
        .as_ref()
        .expect("production harness resources")
        .store_administration
        .project_servers()
        .lock()
        .await
        .servers
        .len();
    assert!(
        (1..=MAX_CACHED_PROJECT_SERVERS).contains(&cached_owner_count),
        "the production registry must remain non-empty and bounded"
    );

    harness.shutdown().await;
}
