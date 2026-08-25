use tempfile::TempDir;

use super::journey_test_support::git;
use super::*;
use crate::daemon::code_index_scheduler::LatestCompleteCodeIndexV1;
use crate::daemon::project_composition::ProductionProjectComposition;

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
    let handshake = DaemonHandshake {
        client_version: binary_version().to_owned(),
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
    let composition = resources
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
        .await?;
    let code_search_scope = {
        let graph = composition.server.cg().await;
        let target = graph.configuration_runtime().configuration_target();
        project_open_owners::resolved_scope_for_project(graph.project_root(), &target.project_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("capacity-journey code-index scope is invalid: {error:?}"),
            })?
    };
    let latest = super::wait_for_production_composition_code_index(
        &resources.invocation,
        &composition.canonical_project_path,
        &code_search_scope,
    )
    .await?;
    Ok((composition, latest))
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
