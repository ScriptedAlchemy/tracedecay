use tempfile::TempDir;
use tracedecay_runtime_core::path_safety::{canonical_existing_identity, canonical_root_identity};

use super::journey_test_support::git;
use super::*;
use crate::daemon::project_composition::ProductionProjectComposition;
use tracedecay_code_index_runtime::code_index_scheduler::{
    LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1,
};

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
        moved_store_adoption: tracedecay_project::project::MovedStoreAdoption::Never,
    };
    let (canonical_project_path, _) =
        project_route_for_handshake(&handshake, resources.store_administration.owner_home()?)?;
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
                    startup_catch_up: false,
                },
                &CancellationToken::new(),
                None,
            )
            .await
        })
        .await
}

/// Open one project route and return the generation level it actually serves.
///
/// Symbol-level evidence exists only while the sealed generation is seated. A
/// reopen whose retained revision-7 head recovered serves through the text
/// projection and never replays its partitions to seat a second copy, so the
/// probe is asserted here on the publishing open and the caller proves the
/// reopen serves that same generation by its id.
async fn open_project(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    instance: &str,
    probe: &str,
) -> Result<(ProductionProjectComposition, LatestCodeTextGenerationV1)> {
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
    super::wait_for_production_composition_code_index(
        &resources.invocation,
        &composition.canonical_project_path,
        &code_search_scope,
    )
    .await?;
    let schedulers = &resources.invocation.code_index_schedulers;
    if let Some(latest) = schedulers
        .latest_complete_ready_for_scope(&code_search_scope)
        .await
    {
        assert_generation_contains_probe(&latest, probe);
        return Ok((composition, latest.text_generation_handle()));
    }
    let serving = schedulers
        .latest_text_serving_for_scope(&code_search_scope)
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "capacity-journey project '{}' has extractable sources but published no generation",
                composition.canonical_project_path.display()
            ),
        })?;
    Ok((composition, serving))
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
    tracedecay_global_db::register_registered_schema_installer();
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
    let first_root = canonical_existing_identity(&projects[0]).expect("canonical first project");
    // Keep the oldest owner leased while sequential clients exceed capacity.
    // Retirement must choose another idle owner, preserving this live client.
    let initial_client = harness
        .resources
        .as_mut()
        .expect("production harness resources")
        .servers
        .remove(&first_root)
        .expect("harness retains its initial client handle");

    let mut replay_roots = Vec::new();
    let mut seeded_generations = Vec::new();
    for (ordinal, project) in projects.iter().enumerate() {
        let probe = format!("project_{ordinal}_probe");
        let (opened, serving) =
            open_project(&harness, project, &format!("initial-{ordinal}"), &probe)
                .await
                .expect("a settled sequential client must release capacity for the next project");
        seeded_generations.push(serving.metadata().manifest().generation_id.clone());
        let graph = opened.server.cg().await;
        let replay_root = graph.hook_store_layout().data_root.clone();
        assert!(
            crate::daemon::hook_v2_replay_consumer::hook_v2_replay_consumer_registered(
                &replay_root
            ),
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
        // Server keys carry the verbatim path the routing layer stores,
        // while this test's roots use the plain canonical convention;
        // spell both sides the same before comparing membership.
        servers
            .servers
            .keys()
            .map(|key| canonical_root_identity(&key.project_root))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let initial_cached_owner_count = initial_cached_projects.len();
    assert!(
        initial_cached_projects.contains(&first_root),
        "the oldest owner's live client must survive capacity retirement"
    );
    assert!(
        (2..=MAX_CACHED_PROJECT_SERVERS).contains(&initial_cached_owner_count),
        "graph pressure must preserve a useful multi-project cache: {initial_cached_owner_count}"
    );
    for (project, replay_root) in &replay_roots {
        assert_eq!(
            crate::daemon::hook_v2_replay_consumer::hook_v2_replay_consumer_registered(replay_root),
            initial_cached_projects.contains(project),
            "Hook V2 replay liveness must match exact project-server retention for {}",
            project.display()
        );
    }

    let (still_live, latest) =
        open_project(&harness, &projects[0], "retained-client", "project_0_probe")
            .await
            .expect("an active owner's cached route remains available under pressure");
    assert!(Arc::ptr_eq(&initial_client, &still_live.server));
    assert_eq!(
        latest.metadata().manifest().generation_id,
        seeded_generations[0],
        "the retained client must still serve its original project's generation"
    );
    // Release the generation reader with this client before the revisit loop
    // retires that owner.
    drop(latest);
    drop(still_live);
    drop(initial_client);

    for (ordinal, project) in projects.iter().enumerate() {
        let probe = format!("project_{ordinal}_probe");
        let (opened, serving) =
            open_project(&harness, project, &format!("reopen-{ordinal}"), &probe)
                .await
                .expect("retired project must reopen through production composition");
        let (cached, cached_serving) =
            open_project(&harness, project, &format!("cached-{ordinal}"), &probe)
                .await
                .expect("immediate reopen must reuse the cached project");
        assert!(
            Arc::ptr_eq(&opened.server, &cached.server),
            "a route-local reopen must reuse the cached server"
        );
        // A reopen that recovered its retained revision-7 head serves through
        // the text projection with the sealed slot deliberately unseated, so
        // the probe symbol is not decodable here. Identity is the property
        // this journey guards: the route must serve the very generation whose
        // symbols carried this project's unique probe, never a neighbour's
        // and never an empty one.
        assert_eq!(
            serving.metadata().manifest().generation_id,
            seeded_generations[ordinal],
            "reopening project {ordinal} must serve its own seeded generation"
        );
        assert_eq!(
            cached_serving.metadata().manifest().generation_id,
            seeded_generations[ordinal],
            "the cached reopen of project {ordinal} must serve its own seeded generation"
        );
        let graph = opened.server.cg().await;
        assert!(
            crate::daemon::hook_v2_replay_consumer::hook_v2_replay_consumer_registered(
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
            let canonical =
                canonical_existing_identity(project).expect("canonical uncached project");
            assert!(
                servers
                    .servers
                    .keys()
                    .all(|key| canonical_root_identity(&key.project_root) != canonical),
                "the concurrent admission fixture must start with an uncached route"
            );
        }
    }
    let (left, right) = tokio::join!(
        open_project(&harness, &projects[0], "concurrent-left", "project_0_probe"),
        open_project(
            &harness,
            &projects[1],
            "concurrent-right",
            "project_1_probe"
        ),
    );
    let (_left, left_serving) = left.expect("first concurrent uncached project admission");
    let (_right, right_serving) = right.expect("second concurrent uncached project admission");
    assert_eq!(
        left_serving.metadata().manifest().generation_id,
        seeded_generations[0],
        "the first concurrent admission must serve its own seeded generation"
    );
    assert_eq!(
        right_serving.metadata().manifest().generation_id,
        seeded_generations[1],
        "the second concurrent admission must serve its own seeded generation"
    );
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

    let leased_servers = {
        let servers = harness
            .resources
            .as_ref()
            .expect("production harness resources")
            .store_administration
            .project_servers()
            .lock()
            .await;
        servers
            .servers
            .iter()
            .map(|(key, entry)| {
                (
                    canonical_root_identity(&key.project_root),
                    Arc::clone(&entry.server),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let uncached_project = projects
        .iter()
        .find(|project| {
            !leased_servers.contains_key(&canonical_existing_identity(project).unwrap())
        })
        .expect("the journey exceeds the route cache");
    let refused = open_project_composition(&harness, uncached_project, "all-owners-leased").await;
    assert!(
        matches!(refused, Err(ref error)
            if error.to_string() == project_server_capacity_error().to_string()),
        "an open must fail with capacity denial while every retained owner is leased: {:?}",
        refused.as_ref().err()
    );
    drop(leased_servers);

    harness.shutdown().await;
}

/// Ingest through the session stores until the full server has mounted them,
/// returning the first settled answer, or `None` once the bound expires.
async fn settled_session_ingest(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Option<serde_json::Value> {
    settled_server_session_ingest(&harness.server(project).expect("mounted project")).await
}

async fn settled_server_session_ingest(
    server: &crate::mcp::McpServer,
) -> Option<serde_json::Value> {
    let request = serde_json::from_value::<JsonRpcRequest>(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_hook_runtime",
            "arguments": {
                "action": "ingest_transcript",
                "provider": "codex",
                "user_scope": false,
            },
        },
    }))
    .expect("hook runtime request");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let response = server
            .handle_request(&request)
            .await
            .expect("hook runtime response");
        assert!(
            response.error.is_none(),
            "hook runtime failed: {response:?}"
        );
        let answer = response.result.expect("hook runtime result");
        if !answer.to_string().contains("application.runtime.mounting") {
            return Some(answer);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shut_down_composition_releases_its_session_stores_for_an_immediate_reopen() {
    let isolation = TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source root");
    std::fs::write(project.join("src/lib.rs"), "pub fn reopen_probe() {}\n")
        .expect("project source");
    git(&project, &["init", "-q"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "seed project"]);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("first production composition");
    let first = settled_session_ingest(&harness, &project).await;
    assert!(
        first.is_some(),
        "the first composition mounts its session stores"
    );
    let project_data_root = harness
        .project_data_root(&project)
        .await
        .expect("project data root");
    let stores = [
        harness.profile_root().join("global.db"),
        harness.profile_root().join("user-sessions.db"),
        project_data_root.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME),
        project_data_root.join("tracedecay.db"),
    ];
    harness.shutdown().await;
    let wal_bytes = stores
        .iter()
        .map(|store| {
            let mut wal = store.as_os_str().to_owned();
            wal.push("-wal");
            let bytes = std::fs::metadata(std::path::PathBuf::from(wal))
                .map_or(0, |metadata| metadata.len());
            (store.file_name().expect("store name").to_owned(), bytes)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        wal_bytes,
        [
            ("global.db".into(), 0),
            ("user-sessions.db".into(), 0),
            ("sessions.db".into(), 0),
            ("tracedecay.db".into(), 0),
        ],
        "shutdown releases every store lease, so each writer truncates its WAL"
    );

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("reopened production composition");
    let reopened = settled_session_ingest(&harness, &project).await;
    assert_eq!(
        reopened
            .as_ref()
            .map(|answer| answer.to_string().contains("accepted_for_replay")),
        Some(true),
        "the reopened composition must remount the session stores: {reopened:?}"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capacity_retirement_releases_session_stores_its_owner_used_on_the_first_attempt() {
    let isolation = TempDir::new().expect("production harness isolation");
    let mut projects = Vec::new();
    for ordinal in 0..12 {
        let project = isolation.path().join(format!("project-{ordinal}"));
        std::fs::create_dir_all(project.join("src")).expect("project source root");
        std::fs::write(
            project.join("src/lib.rs"),
            format!("pub fn sessions_{ordinal}_probe() -> usize {{ {ordinal} }}\n"),
        )
        .expect("project source");
        git(&project, &["init", "-q"]);
        git(&project, &["add", "."]);
        git(&project, &["commit", "-qm", "seed project"]);
        projects.push(project);
    }
    let mut harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        std::iter::once(projects[0].clone()),
    )
    .await
    .expect("production harness authority");
    let first_root = canonical_existing_identity(&projects[0]).expect("canonical first project");
    drop(
        harness
            .resources
            .as_mut()
            .expect("production harness resources")
            .servers
            .remove(&first_root)
            .expect("harness retains its initial client handle"),
    );

    let mut opened_projects = 0_usize;
    for (ordinal, project) in projects.iter().enumerate() {
        let opened = open_project_composition(&harness, project, &format!("sessions-{ordinal}"))
            .await
            .unwrap_or_else(|error| {
                panic!("project {ordinal} must open over released capacity: {error}")
            });
        assert!(
            settled_server_session_ingest(&opened.server)
                .await
                .is_some(),
            "project {ordinal} mounts its session stores"
        );
        drop(opened);
        opened_projects += 1;
    }
    assert_eq!(opened_projects, 12);
    harness.shutdown().await;
}

fn assert_retirement_blocked(error: &TraceDecayError, project_id: &str) {
    let Some((reason_code, retryable, detail)) = error.project_route_context() else {
        panic!("a blocked retirement must be a typed capacity refusal: {error:?}");
    };
    assert_eq!(reason_code, PROJECT_SERVER_CAPACITY_REASON_CODE);
    assert!(retryable, "the next open retires another idle owner");
    assert!(
        detail.contains(&format!("retiring idle project '{project_id}' is blocked"))
            && detail.contains("ClientLeases"),
        "the refusal must name the retired project and its store blocker: {detail}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capacity_retirement_blocked_by_a_foreign_store_lease_is_typed_and_not_replayed() {
    let isolation = TempDir::new().expect("production harness isolation");
    let mut projects = Vec::new();
    for ordinal in 0..14 {
        let project = isolation.path().join(format!("project-{ordinal}"));
        std::fs::create_dir_all(project.join("src")).expect("project source root");
        std::fs::write(
            project.join("src/lib.rs"),
            format!("pub fn leased_{ordinal}_probe() -> usize {{ {ordinal} }}\n"),
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
    let first_root = canonical_existing_identity(&projects[0]).expect("canonical first project");
    drop(
        harness
            .resources
            .as_mut()
            .expect("production harness resources")
            .servers
            .remove(&first_root)
            .expect("harness retains its initial client handle"),
    );

    // Lease the oldest project's session store from outside its owner, then
    // leave the project idle so capacity picks it to retire.
    let leased = open_project_composition(&harness, &projects[0], "leased")
        .await
        .expect("the leased project opens");
    let project_id = leased
        .server
        .cg()
        .await
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let registry = harness
        .resources
        .as_ref()
        .expect("production harness resources")
        .store_administration
        .session_runtime_registry()
        .await
        .expect("session runtime registry");
    let session_lease = registry
        .mounted_project_sessions(&project_id)
        .await
        .expect("the open project mounts its session store");
    drop(leased);

    let mut refusal = None;
    for (ordinal, project) in projects.iter().enumerate().skip(1) {
        match open_project_composition(&harness, project, &format!("fill-{ordinal}")).await {
            Ok(opened) => drop(opened),
            Err(error) => {
                refusal = Some((ordinal, error));
                break;
            }
        }
    }
    let (blocked, refused) = refusal.expect("capacity must retire the leased idle project");
    assert_retirement_blocked(&refused, project_id.as_str());

    // The refused owner's servers are already gone and its refusal was
    // reported once: every later open retires another idle owner and serves
    // while the foreign lease is still held.
    for (ordinal, project) in projects.iter().enumerate().skip(blocked) {
        let opened = open_project_composition(&harness, project, &format!("after-{ordinal}"))
            .await
            .unwrap_or_else(|error| {
                panic!("project {ordinal} must not replay the reported refusal: {error}")
            });
        drop(opened);
    }

    drop(session_lease);
    let reopened = open_project_composition(&harness, &projects[0], "retired-reopen")
        .await
        .expect("the retired project reopens over its retained stores");
    assert_eq!(reopened.canonical_project_path, first_root);
    drop(reopened);
    harness.shutdown().await;
}
