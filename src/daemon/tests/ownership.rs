#[cfg(unix)]
use std::process::Command;

use super::*;
use crate::daemon::ProjectServerRequirement;

#[cfg(unix)]
#[derive(Clone, Copy)]
enum ProjectGitState {
    NonGit,
    Unborn,
    Committed,
}

#[cfg(unix)]
async fn assert_fresh_project_open_owners(label: &str, git_state: ProjectGitState) {
    let temp = TempDir::new().expect("project-open owners fixture");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project-open owners project");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n")
        .expect("project-open owners source");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    if !matches!(git_state, ProjectGitState::NonGit) {
        let initialized = Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&project)
            .status()
            .expect("run git init");
        assert!(initialized.success(), "git init must succeed");
    }
    if matches!(git_state, ProjectGitState::Unborn) {
        let symbolic_head = Command::new("git")
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .current_dir(&project)
            .output()
            .expect("read unborn symbolic HEAD");
        assert!(
            symbolic_head.status.success(),
            "unborn HEAD must be symbolic"
        );
        assert_eq!(
            String::from_utf8(symbolic_head.stdout)
                .expect("unborn symbolic HEAD must be UTF-8")
                .trim(),
            "refs/heads/main",
            "unborn fixture must use its explicit default branch"
        );
        let resolved_head = Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&project)
            .output()
            .expect("verify unborn HEAD");
        assert!(
            !resolved_head.status.success(),
            "unborn fixture must not have a commit"
        );
    }
    if matches!(git_state, ProjectGitState::Committed) {
        let added = Command::new("git")
            .args(["add", "--all"])
            .current_dir(&project)
            .status()
            .expect("run git add");
        assert!(added.success(), "git add must succeed");
        let committed = Command::new("git")
            .args([
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "test: initial",
            ])
            .current_dir(&project)
            .status()
            .expect("run git commit");
        assert!(committed.success(), "git commit must succeed");
    }
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope = enter_test_daemon_database_scope(&profile_root, label);
    let engine = test_daemon_engine_for_profile(&profile_root);
    let server = engine
        .project_server(&handshake)
        .await
        .expect("fresh project-open owners");
    let graph = server.cg().await;
    let canonical_project = graph.project_root().to_path_buf();
    let replay_root = graph.hook_store_layout().data_root.clone();

    assert!(
        engine
            .invocation
            .service
            .lsp_owner(Some(&canonical_project))
            .await
            .is_some(),
        "fresh project open must retain its LSP owner"
    );
    assert_eq!(
        engine
            .invocation
            .service
            .feedback_cycle(Some(&canonical_project))
            .await
            .is_some(),
        matches!(git_state, ProjectGitState::Committed),
        "feedback cycle presence must follow exact committed Git identity"
    );
    assert!(
        crate::daemon::hook_v2_replay::hook_v2_replay_consumer_registered(&replay_root),
        "fresh project open must start Hook V2 replay"
    );
    let graph_weak = Arc::downgrade(&graph);
    drop(graph);
    drop(server);
    engine.shutdown_all().await;
    assert!(
        graph_weak.upgrade().is_none(),
        "daemon shutdown must release the project graph retained by Hook V2 replay"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_committed_project_open_mounts_feedback_before_lsp() {
    assert_fresh_project_open_owners("committed-project-open-owners", ProjectGitState::Committed)
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn non_git_project_open_retains_lsp_and_starts_hook_replay() {
    assert_fresh_project_open_owners("non-git-project-open-owners", ProjectGitState::NonGit).await;
}

#[cfg(unix)]
#[tokio::test]
async fn unborn_git_project_open_retains_lsp_and_starts_hook_replay() {
    assert_fresh_project_open_owners("unborn-project-open-owners", ProjectGitState::Unborn).await;
}

#[cfg(unix)]
async fn maintenance_owner_fixture(
    label: &str,
) -> (
    TempDir,
    crate::db::DaemonDatabaseScope,
    DaemonEngine,
    ProjectServerKey,
    DaemonHandshake,
) {
    let temp = TempDir::new().expect("maintenance fixture");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("maintenance fixture project");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n")
        .expect("maintenance fixture source");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let database_scope = enter_test_daemon_database_scope(&profile_root, label);
    let cg = super::super::open_project_for_handshake(
        &project,
        &handshake,
        &engine.store_administration,
    )
    .await
    .expect("open maintenance fixture through daemon authority");
    let key =
        ProjectServerKey::from_open_project(&cg, &handshake).expect("maintenance fixture owner");
    let server = crate::mcp::McpServer::new_with_global_db(cg, None, None).await;
    engine
        .store_administration
        .project_servers()
        .lock()
        .await
        .insert(key.clone(), server);
    (temp, database_scope, engine, key, handshake)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_cache_hit_skips_open_and_singleflights_first_miss() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    const PARALLEL_CLIENT_IDENTITIES: usize = 16;
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let project_alias = temp.path().join("project-alias");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("project source");
    std::os::unix::fs::symlink(&project, &project_alias).expect("project alias");
    let client_identity = test_client_identity_for(profile_root.clone());
    eprintln!("[cache-test] phase=init start");
    initialize_test_project(&project, &client_identity).await;
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    eprintln!("[cache-test] phase=init done");

    let direct = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let aliased = DaemonHandshake {
        project_path: Some(project_alias),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "project-server-cache-test")
            .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let direct_route = super::super::ProjectRouteKey::from_handshake(&project, &direct).unwrap();
    let alias_route = super::super::ProjectRouteKey::from_handshake(
        &project.canonicalize().expect("canonical project"),
        &aliased,
    )
    .unwrap();
    assert_eq!(
        direct_route, alias_route,
        "aliases must share one route gate"
    );

    eprintln!("[cache-test] phase=concurrent-open start");
    let (direct_server, alias_server) = tokio::time::timeout(
        PHASE_TIMEOUT,
        Box::pin(async {
            tokio::join!(
                engine.project_server(&direct),
                engine.project_server(&aliased)
            )
        }),
    )
    .await
    .expect("cache-test concurrent-open phase timed out");
    eprintln!("[cache-test] phase=concurrent-open done");
    let direct_server = direct_server.expect("direct project server");
    let alias_server = alias_server.expect("aliased project server");
    assert!(std::sync::Arc::ptr_eq(&direct_server, &alias_server));
    tokio::time::timeout(PHASE_TIMEOUT, async {
        while engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial maintenance activation timed out");
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "canonical aliases must singleflight the first project open"
    );
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "concurrent insertion must acquire one repair owner"
    );
    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .is_empty()
                && engine
                    .automation_config_probe_attempts
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial memory-repair pass timed out");
    engine
        .memory_repair_start_attempts
        .store(0, std::sync::atomic::Ordering::Relaxed);
    engine
        .automation_config_probe_attempts
        .store(0, std::sync::atomic::Ordering::Relaxed);

    eprintln!("[cache-test] phase=cached-open start");
    let cached = tokio::time::timeout(PHASE_TIMEOUT, engine.project_server(&direct))
        .await
        .expect("cache-test cached-open phase timed out")
        .expect("cached project server");
    eprintln!("[cache-test] phase=cached-open done");
    assert!(std::sync::Arc::ptr_eq(&direct_server, &cached));
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache hits must return before opening project databases"
    );
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cache hits must not restart completed maintenance"
    );
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cache hits must not re-probe unchanged automation config"
    );
    for client_index in 0..PARALLEL_CLIENT_IDENTITIES {
        let mut client = direct.clone();
        client.client_instance_id = format!("{client_index:032x}");
        let shared = engine
            .project_server(&client)
            .await
            .expect("same-worktree client server");
        assert!(std::sync::Arc::ptr_eq(&direct_server, &shared));
    }
    let retained_servers = engine
        .store_administration
        .project_servers()
        .lock()
        .await
        .servers
        .len();
    assert_eq!(retained_servers, 1);
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    eprintln!(
        "same_worktree_live_engine_proxy clients={PARALLEL_CLIENT_IDENTITIES} open_attempts=1 retained_servers={retained_servers} cache_hits={PARALLEL_CLIENT_IDENTITIES}"
    );
    let released_server = Arc::downgrade(&direct_server);
    drop(cached);
    drop(alias_server);
    drop(direct_server);
    eprintln!("[cache-test] phase=shutdown start");
    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("cache-test shutdown phase timed out");
    assert!(
        released_server.upgrade().is_none(),
        "shutdown must break the server-to-administration ownership cycle"
    );
    eprintln!("[cache-test] phase=shutdown done");
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_post_insert_activation_retains_maintenance_ownership() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "interrupted-activation-test")
            .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&profile_root);

    let (published_tx, published_rx) = tokio::sync::oneshot::channel();
    let request_engine = engine.clone();
    let request_handshake = handshake.clone();
    let request = tokio::spawn(async move {
        let (_, _, server, inserted) = request_engine
            .store_administration
            .with_writer(|| request_engine.open_project_server(&request_handshake))
            .await
            .expect("insert project server");
        assert!(inserted);
        published_tx.send(()).expect("signal cache publication");
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        server
    });
    published_rx.await.expect("cache publication signal");
    request.abort();
    let cancellation = request.await;
    assert!(
        matches!(cancellation, Err(error) if error.is_cancelled()),
        "requesting future must actually be cancelled"
    );
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
            || engine
                .memory_repair_start_attempts
                .load(std::sync::atomic::Ordering::Relaxed)
                == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon-owned maintenance activation timed out");
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache publication must acquire repair ownership before activation can be interrupted"
    );
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache publication must perform its one initial automation reconciliation"
    );

    engine.shutdown_all().await;
}

#[cfg(unix)]
#[test]
fn store_owner_key_collapses_profile_and_store_aliases() {
    let temp = TempDir::new().expect("temp dir");
    let profile = temp.path().join("profile");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&profile).expect("profile dir");
    std::fs::create_dir_all(&store).expect("store dir");
    let profile_alias = temp.path().join("profile-alias");
    let store_alias = temp.path().join("store-alias");
    std::os::unix::fs::symlink(&profile, &profile_alias).expect("profile alias");
    std::os::unix::fs::symlink(&store, &store_alias).expect("store alias");

    let direct = StoreOwnerKey::from_paths(
        &profile,
        &profile.join("global.db"),
        Some("project-id".to_string()),
        &store,
        &store.join("graph.db"),
    )
    .expect("direct owner");
    let aliased = StoreOwnerKey::from_paths(
        &profile_alias,
        &profile_alias.join("global.db"),
        Some("project-id".to_string()),
        &store_alias,
        &store_alias.join("graph.db"),
    )
    .expect("aliased owner");

    assert_eq!(direct, aliased);
}

#[cfg(unix)]
#[test]
fn parallel_client_instances_share_one_engine_but_scope_and_profile_do_not() {
    const REPRESENTATIVE_CLIENTS: usize = 32;

    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile = temp.path().join("profile");
    let other_profile = temp.path().join("other-profile");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::create_dir_all(&profile).expect("profile dir");
    std::fs::create_dir_all(&other_profile).expect("other profile dir");
    std::fs::create_dir_all(&store).expect("store dir");

    let owner = StoreOwnerKey::from_paths(
        &profile,
        &profile.join("global.db"),
        Some("project-id".to_string()),
        &store,
        &store.join("graph.db"),
    )
    .expect("owner");
    let shared_key = ProjectServerKey {
        owner: owner.clone(),
        project_root: project.clone(),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();

    for client_index in 0..REPRESENTATIVE_CLIENTS {
        let mut handshake = test_handshake_defaults();
        handshake.project_path = Some(project.clone());
        handshake.client_identity = test_client_identity_for(profile.clone());
        handshake.client_instance_id = format!("{client_index:032x}");
        let route = ProjectRouteKey::from_handshake(&project, &handshake).expect("route");
        let (_, inserted) =
            registry.bind_or_insert_route(route, shared_key.clone(), Arc::new(client_index as u8));
        assert_eq!(inserted, client_index == 0);
    }

    assert_eq!(
        registry.servers.len(),
        1,
        "transient client identities must not multiply heavy engines"
    );
    eprintln!(
        "same_worktree_retained_engine_proxy unshared_baseline_servers={} shared_servers={} reduction_percent={:.3}",
        REPRESENTATIVE_CLIENTS,
        registry.servers.len(),
        100.0 * (REPRESENTATIVE_CLIENTS - registry.servers.len()) as f64
            / REPRESENTATIVE_CLIENTS as f64
    );

    let scoped_key = ProjectServerKey {
        owner: owner.clone(),
        project_root: project.clone(),
        scope_prefix: Some("private".to_string()),
    };
    let mut scoped = test_handshake_defaults();
    scoped.project_path = Some(project.clone());
    scoped.scope_prefix = Some("private".to_string());
    scoped.client_identity = test_client_identity_for(profile.clone());
    let scoped_route = ProjectRouteKey::from_handshake(&project, &scoped).expect("scoped route");
    let (_, inserted) = registry.bind_or_insert_route(scoped_route, scoped_key, Arc::new(u8::MAX));
    assert!(inserted, "distinct scope must retain an isolated engine");

    let other_owner = StoreOwnerKey::from_paths(
        &other_profile,
        &other_profile.join("global.db"),
        Some("project-id".to_string()),
        &store,
        &store.join("graph.db"),
    )
    .expect("other owner");
    let other_key = ProjectServerKey {
        owner: other_owner,
        project_root: project.clone(),
        scope_prefix: None,
    };
    let mut other = test_handshake_defaults();
    other.project_path = Some(project.clone());
    other.client_identity = test_client_identity_for(other_profile);
    let other_route = ProjectRouteKey::from_handshake(&project, &other).expect("other route");
    let (_, inserted) =
        registry.bind_or_insert_route(other_route, other_key, Arc::new(u8::MAX - 1));
    assert!(inserted, "distinct profile authority must never share");
    assert_eq!(registry.servers.len(), 3);
}

#[cfg(unix)]
#[test]
fn database_owner_registry_rekeys_and_evicts_stale_routes() {
    let owner = StoreOwnerKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_id: Some("project".to_string()),
        store_root: PathBuf::from("/store"),
        graph_db_path: PathBuf::from("/store/main.db"),
    };
    let old = ProjectServerKey {
        owner: owner.clone(),
        project_root: PathBuf::from("/project"),
        scope_prefix: Some("src".to_string()),
    };
    let mut feature_owner = owner;
    feature_owner.graph_db_path = PathBuf::from("/store/feature.db");
    let new = ProjectServerKey {
        owner: feature_owner,
        project_root: PathBuf::from("/project"),
        scope_prefix: Some("src".to_string()),
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project"),
        scope_prefix: Some("src".to_string()),
    };
    let mut registry = DatabaseOwnerRegistry::<u8>::default();
    registry.insert(old.clone(), 7);
    registry.bind_route(route.clone(), old.clone());

    assert!(registry.rekey(&old, &new));

    assert!(registry.get(&old).is_none());
    assert_eq!(registry.get(&new), Some(&7));
    assert_eq!(registry.get_route(&route), Some((&new, &7)));

    let mut collision = DatabaseOwnerRegistry::<u8>::default();
    collision.insert(old.clone(), 7);
    collision.insert(new.clone(), 9);
    collision.bind_route(route.clone(), old.clone());
    assert!(!collision.rekey(&old, &new));
    assert!(collision.get(&old).is_none());
    assert_eq!(collision.get(&new), Some(&9));
    assert!(collision.get_route(&route).is_none());
}

#[test]
fn database_owner_registry_race_keeps_first_server_and_binds_route() {
    let owner = StoreOwnerKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_id: Some("project".to_string()),
        store_root: PathBuf::from("/store"),
        graph_db_path: PathBuf::from("/store/main.db"),
    };
    let key = ProjectServerKey {
        owner,
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project-alias"),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<u8>::default();
    registry.insert(key.clone(), 7);

    let (resolved, inserted) = registry.bind_or_insert_route(route.clone(), key.clone(), 9);

    assert_eq!(resolved, 7);
    assert!(!inserted);
    assert_eq!(registry.get_route(&route), Some((&key, &7)));
}

#[test]
fn database_owner_registry_evicts_lru_idle_and_protects_active_leases() {
    fn key(name: &str) -> ProjectServerKey {
        ProjectServerKey {
            owner: StoreOwnerKey {
                profile_root: PathBuf::from("/profile"),
                global_db_path: PathBuf::from("/profile/global.db"),
                project_id: Some(name.to_string()),
                store_root: PathBuf::from(format!("/store/{name}")),
                graph_db_path: PathBuf::from(format!("/store/{name}/graph.db")),
            },
            project_root: PathBuf::from(format!("/project/{name}")),
            scope_prefix: None,
        }
    }

    fn route(name: &str) -> ProjectRouteKey {
        ProjectRouteKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_path: PathBuf::from(format!("/project/{name}")),
            scope_prefix: None,
        }
    }

    let now = std::time::Instant::now();
    let oldest = key("oldest-active");
    let idle = key("idle");
    let inserted = key("inserted");
    let blocked = key("blocked");
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();
    registry.insert_at(
        oldest.clone(),
        Arc::new(1),
        now.checked_sub(std::time::Duration::from_secs(20)).unwrap(),
    );
    registry.bind_route(route("oldest-active"), oldest.clone());
    registry.insert_at(
        idle.clone(),
        Arc::new(2),
        now.checked_sub(std::time::Duration::from_secs(10)).unwrap(),
    );
    registry.bind_route(route("idle"), idle.clone());
    let active_lease = Arc::clone(registry.get(&oldest).expect("oldest server"));

    let (server, was_inserted) = registry
        .bind_or_insert_route_bounded(
            route("inserted"),
            inserted.clone(),
            Arc::new(3),
            2,
            |server| Arc::strong_count(server) > 1,
        )
        .expect("idle entry should be evicted");
    assert!(was_inserted);
    assert_eq!(*server, 3);
    assert!(registry.get(&idle).is_none());
    assert!(registry.get_route(&route("idle")).is_none());
    assert!(registry.get(&oldest).is_some());
    assert!(registry.get(&inserted).is_some());

    let inserted_lease = Arc::clone(registry.get(&inserted).expect("inserted server"));
    assert!(
        registry
            .bind_or_insert_route_bounded(
                route("blocked"),
                blocked.clone(),
                Arc::new(4),
                2,
                |server| Arc::strong_count(server) > 1,
            )
            .is_none(),
        "all leased entries must reject rather than grow or evict"
    );
    assert_eq!(registry.servers.len(), 2);
    assert!(registry.get(&oldest).is_some());
    assert!(registry.get(&inserted).is_some());
    assert!(registry.get(&blocked).is_none());
    assert!(registry.get_route(&route("blocked")).is_none());

    drop(active_lease);
    drop(inserted_lease);
}

#[test]
fn database_owner_registry_hides_bounded_insert_until_core_publication() {
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("pending".to_owned()),
            store_root: PathBuf::from("/store/pending"),
            graph_db_path: PathBuf::from("/store/pending/graph.db"),
        },
        project_root: PathBuf::from("/project/pending"),
        scope_prefix: None,
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project/pending"),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();

    let (_, inserted) = registry
        .bind_or_insert_route_bounded(route.clone(), key.clone(), Arc::new(1), 1, |_| false)
        .expect("pending owner should fit");

    assert!(inserted);
    assert!(
        registry.get_route_and_touch(&route).is_none(),
        "a route must remain hidden until its core server is constructed"
    );
    assert!(registry.mark_ready(&key));
    assert!(
        registry.get_route_and_touch(&route).is_some(),
        "core publication must expose the route while optional owners warm"
    );
}

#[test]
fn database_owner_registry_upgrades_only_the_published_core_and_preserves_aliases() {
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("upgrade".to_owned()),
            store_root: PathBuf::from("/store/upgrade"),
            graph_db_path: PathBuf::from("/store/upgrade/graph.db"),
        },
        project_root: PathBuf::from("/project/upgrade"),
        scope_prefix: None,
    };
    let route = |project_path: &str| ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from(project_path),
        scope_prefix: None,
    };
    let core = Arc::new(1_u8);
    let full = Arc::new(2_u8);
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();
    registry.insert_pending_route(route("/project"), key.clone(), Arc::clone(&core));
    registry.bind_route(route("/project-alias"), key.clone());

    assert!(
        !registry.replace_ready_if(&key, Arc::clone(&full), |current| {
            Arc::ptr_eq(current, &core)
        }),
        "an unpublished core must not be upgraded"
    );
    assert!(registry.mark_ready(&key));
    assert!(
        registry
            .get_route_and_touch_for(
                &route("/project"),
                ProjectServerRequirement::RegisteredHostIngest,
            )
            .is_none(),
        "a graph-only core must not receive host ingest before registered authority publication"
    );
    assert!(
        !registry.replace_ready_if(&key, Arc::clone(&full), |_| false),
        "a stale core comparison must not replace the current server"
    );
    assert!(
        registry.replace_ready_if(&key, Arc::clone(&full), |current| {
            Arc::ptr_eq(current, &core)
        })
    );
    assert!(Arc::ptr_eq(
        registry
            .get_route(&route("/project"))
            .expect("primary route")
            .1,
        &full
    ));
    assert!(Arc::ptr_eq(
        registry
            .get_route(&route("/project-alias"))
            .expect("alias route")
            .1,
        &full
    ));
    assert!(
        registry
            .get_route_and_touch_for(
                &route("/project"),
                ProjectServerRequirement::RegisteredHostIngest,
            )
            .is_some(),
        "the full server must publish registered host-ingest authority"
    );
}

#[test]
fn database_owner_registry_removal_retires_every_route_alias() {
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("unhealthy".to_owned()),
            store_root: PathBuf::from("/store/unhealthy"),
            graph_db_path: PathBuf::from("/store/unhealthy/graph.db"),
        },
        project_root: PathBuf::from("/project/unhealthy"),
        scope_prefix: None,
    };
    let route = |project_path: &str| ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from(project_path),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();
    registry.insert_route(route("/project"), key.clone(), Arc::new(1));
    registry.bind_route(route("/project-alias"), key.clone());

    assert!(registry.remove(&key).is_some());
    assert!(registry.get_route(&route("/project")).is_none());
    assert!(registry.get_route(&route("/project-alias")).is_none());
}

#[test]
fn failed_core_upgrade_retires_the_rekeyed_owner_without_quarantine() {
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("upgrade-failure".to_owned()),
            store_root: PathBuf::from("/store/upgrade-failure"),
            graph_db_path: PathBuf::from("/store/upgrade-failure/main.db"),
        },
        project_root: PathBuf::from("/project/upgrade-failure"),
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/upgrade-failure/feature.db");
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();
    registry.insert_route(route.clone(), old.clone(), Arc::new(1));

    assert!(registry.rekey(&old, &new));
    assert_eq!(registry.remove_owner(&new.owner).len(), 1);
    assert!(registry.get_route(&route).is_none());
    assert!(!registry.requires_synchronous_health(&new.owner));
}

#[test]
fn failed_optional_upgrade_restores_the_ready_core() {
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("degraded-upgrade".to_owned()),
            store_root: PathBuf::from("/store/degraded-upgrade"),
            graph_db_path: PathBuf::from("/store/degraded-upgrade/main.db"),
        },
        project_root: PathBuf::from("/project/degraded-upgrade"),
        scope_prefix: None,
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let core = Arc::new(1_u8);
    let full = Arc::new(2_u8);
    let mut registry = DatabaseOwnerRegistry::default();
    registry.insert_route(route.clone(), key.clone(), Arc::clone(&core));

    assert!(
        registry.replace_ready_if(&key, Arc::clone(&full), |current| {
            Arc::ptr_eq(current, &core)
        })
    );
    let failed = registry
        .swap_ready_if(&key, Arc::clone(&core), |current| {
            Arc::ptr_eq(current, &full)
        })
        .expect("published full server");

    assert!(Arc::ptr_eq(&failed, &full));
    assert!(
        registry
            .get_route(&route)
            .is_some_and(|(_, current)| Arc::ptr_eq(current, &core))
    );
}

#[test]
fn failed_deferred_health_quarantines_only_the_exact_store_owner() {
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile-a"),
            global_db_path: PathBuf::from("/profile-a/global.db"),
            project_id: Some("unhealthy".to_owned()),
            store_root: PathBuf::from("/store/unhealthy"),
            graph_db_path: PathBuf::from("/store/unhealthy/graph.db"),
        },
        project_root: PathBuf::from("/project/unhealthy"),
        scope_prefix: None,
    };
    let mut other = key.clone();
    other.owner.profile_root = PathBuf::from("/profile-b");
    other.owner.global_db_path = PathBuf::from("/profile-b/global.db");
    let mut sibling_scope = key.clone();
    sibling_scope.scope_prefix = Some("src".to_owned());
    let mut registry = DatabaseOwnerRegistry::<Arc<u8>>::default();
    registry.insert(key.clone(), Arc::new(1));
    registry.insert(sibling_scope.clone(), Arc::new(2));

    assert_eq!(registry.quarantine_and_remove_owner(&key.owner).len(), 2);
    assert!(registry.get(&key).is_none());
    assert!(registry.get(&sibling_scope).is_none());
    assert!(registry.requires_synchronous_health(&key.owner));
    assert!(!registry.requires_synchronous_health(&other.owner));
    registry.clear_synchronous_health(&key.owner);
    assert!(!registry.requires_synchronous_health(&key.owner));
}

#[cfg(unix)]
#[tokio::test]
async fn project_rekey_cancels_stale_repair_owner_and_acquires_new_owner_once() {
    let (_fixture, _database_scope, engine, new, handshake) =
        maintenance_owner_fixture("project-rekey-test").await;
    let mut old = new.clone();
    old.owner.graph_db_path = old.owner.graph_db_path.with_extension("retiring.db");
    let task = tokio::spawn(std::future::pending::<()>());
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    engine
        .rekey_project_maintenance(
            &old,
            new.clone(),
            handshake.project_path.clone().expect("project path"),
            handshake,
            true,
        )
        .await;
    assert!(
        stale_task.is_finished(),
        "rekey must await stale repair shutdown before returning"
    );
    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&old));
    assert!(schedulers.contains_key(&new));
    assert_eq!(schedulers.len(), 1);
    drop(schedulers);
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn project_rekey_awaits_stale_automation_owner_before_replacement() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let task = tokio::spawn(std::future::pending::<()>());
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    engine
        .rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        )
        .await;

    assert!(
        stale_task.is_finished(),
        "rekey must await stale automation shutdown before returning"
    );
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_blocked_automation_retirement_reaper_and_is_idempotent() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/automation.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative automation owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));

    let retirement = engine
        .retire_automation_scheduler_locked(&key)
        .await
        .expect("automation owner retirement");
    let shutdown_engine = engine.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_engine.shutdown_all().await;
    });
    wait_for_automation_scheduler_state(
        &engine,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        "shutdown automation ownership drain",
        std::collections::HashMap::is_empty,
    )
    .await;

    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the blocked automation retirement reaper"
    );
    assert!(
        !stale_task.is_finished(),
        "blocked automation owner must still be live before release"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("automation owner completion timed out")
        .expect("automation owner completion sender dropped");
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown did not reap automation retirement")
        .expect("shutdown task panicked");
    retirement.wait().await;

    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0,
        "shutdown must leave no automation reaper ownership record"
    );
    assert!(
        stale_task.is_finished(),
        "shutdown must not leave the retired automation owner orphaned"
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must clear the retired automation tombstone"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), engine.shutdown_all())
        .await
        .expect("repeated shutdown must be idempotent");
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_blocked_repair_retirement_reaper_and_is_idempotent() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/repair.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(
            key.clone(),
            super::super::memory_repair_scheduler::MemoryRepairSchedulerHandle::for_test(task),
        );

    let retirement = engine
        .retire_memory_repair_scheduler_locked(&key)
        .await
        .expect("repair owner retirement");
    let shutdown_engine = engine.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_engine.shutdown_all().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown did not drain repair ownership");

    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the blocked repair retirement reaper"
    );
    assert!(
        !stale_task.is_finished(),
        "blocked repair owner must still be live before release"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("repair owner completion timed out")
        .expect("repair owner completion sender dropped");
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown did not reap repair retirement")
        .expect("shutdown task panicked");
    retirement.wait().await;

    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0,
        "shutdown must leave no repair reaper ownership record"
    );
    assert!(
        stale_task.is_finished(),
        "shutdown must not leave the retired repair owner orphaned"
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must clear the retired repair tombstone"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), engine.shutdown_all())
        .await
        .expect("repeated shutdown must be idempotent");
}

#[cfg(unix)]
#[tokio::test]
async fn automation_retirement_timeout_retains_owner_tombstone_until_join_finishes() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx.await.expect("noncooperative owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    let rekey = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;

    let retained = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == AutomationSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_automation_scheduler(
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
        ),
    )
    .await
    .ok();
    let owner_count = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .len();

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let joined = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new,
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let owners_after_join = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .len();
    let reapers_after_join = engine.store_administration.retirement_reaper_count().await;
    engine.shutdown_all().await;

    assert_eq!(
        rekey,
        Ok(super::super::MaintenanceRekeyOutcome::Retiring),
        "noncooperative retirement must return its bounded timeout outcome"
    );
    assert!(
        retained,
        "retirement timeout must retain a tombstone until the JoinHandle terminates"
    );
    assert_eq!(
        reconcile,
        Some(crate::dashboard::AutomationSchedulerReconcileOutcome::Retiring),
        "replacement must remain unavailable while the old JoinHandle is live"
    );
    assert_eq!(
        owner_count, 1,
        "retirement must retain exactly one ownership record"
    );
    assert!(completed.is_ok(), "noncooperative owner was not released");
    assert_eq!(
        joined,
        Ok(super::super::MaintenanceRekeyOutcome::Completed),
        "released owner must be joined by the next retirement attempt"
    );
    assert!(
        stale_task.is_finished(),
        "stale owner task must be terminated"
    );
    assert_eq!(owners_after_join, 0, "the reaper must clear its tombstone");
    assert_eq!(
        reapers_after_join, 0,
        "normal automation reaper completion must release daemon ownership"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn repair_retirement_timeout_retains_owner_tombstone_until_join_finishes() {
    use super::super::memory_repair_scheduler::{
        MemoryRepairSchedulerLifecycle, MemoryRepairSchedulerReconcileOutcome,
    };

    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    let rekey = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let retained = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == MemoryRepairSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_memory_repair_scheduler(
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
        ),
    )
    .await
    .ok();
    let owner_count = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .len();

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let joined = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new,
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let owners_after_join = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .len();
    let reapers_after_join = engine.store_administration.retirement_reaper_count().await;
    engine.shutdown_all().await;

    assert_eq!(rekey, Ok(super::super::MaintenanceRekeyOutcome::Retiring));
    assert!(retained, "repair timeout must retain a retiring owner");
    assert_eq!(
        reconcile,
        Some(MemoryRepairSchedulerReconcileOutcome::Retiring),
        "repair replacement must remain blocked by the live tombstone"
    );
    assert_eq!(owner_count, 1, "repair retirement must keep one owner");
    assert!(
        completed.is_ok(),
        "noncooperative repair owner was not released"
    );
    assert_eq!(joined, Ok(super::super::MaintenanceRekeyOutcome::Completed));
    assert!(stale_task.is_finished(), "stale repair task must terminate");
    assert_eq!(
        owners_after_join, 0,
        "repair reaper must clear its tombstone"
    );
    assert_eq!(
        reapers_after_join, 0,
        "normal repair reaper completion must release daemon ownership"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn released_repair_tombstone_allows_one_eventual_replacement() {
    use super::super::memory_repair_scheduler::{
        MemoryRepairSchedulerLifecycle, MemoryRepairSchedulerReconcileOutcome,
    };

    let (_fixture, _database_scope, engine, new, handshake) =
        maintenance_owner_fixture("repair-tombstone-test").await;
    let project_path = handshake.project_path.clone().expect("project path");
    let mut old = new.clone();
    old.owner.graph_db_path = old.owner.graph_db_path.with_extension("retiring.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    let timed_out = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            project_path.clone(),
            handshake.clone(),
            true,
        ),
    )
    .await;
    let retained = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == MemoryRepairSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_memory_repair_scheduler(new.clone(), project_path.clone(), handshake.clone()),
    )
    .await
    .ok();
    let no_overlap = {
        let schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        schedulers.len() == 1 && schedulers.contains_key(&old) && !schedulers.contains_key(&new)
    };

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let replaced = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(&old, new.clone(), project_path, handshake, true),
    )
    .await;
    let (owner_count, owns_new, live_replacement) = {
        let schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        (
            schedulers.len(),
            schedulers.contains_key(&new),
            schedulers
                .get(&new)
                .and_then(|owner| owner.task.as_ref())
                .is_some_and(|task| !task.is_finished()),
        )
    };
    let start_attempts = engine
        .memory_repair_start_attempts
        .load(std::sync::atomic::Ordering::Relaxed);
    engine.shutdown_all().await;

    assert_eq!(
        timed_out,
        Ok(super::super::MaintenanceRekeyOutcome::Retiring)
    );
    assert!(
        retained,
        "retirement timeout must retain one Retiring repair tombstone"
    );
    assert_eq!(
        reconcile,
        Some(MemoryRepairSchedulerReconcileOutcome::Retiring),
        "ensure of the logical new key must stay blocked while the tombstone is live"
    );
    assert!(no_overlap, "a retiring repair owner must block replacement");
    assert!(
        completed.is_ok(),
        "noncooperative repair owner was not released"
    );
    assert_eq!(
        replaced,
        Ok(super::super::MaintenanceRekeyOutcome::Completed)
    );
    assert_eq!(owner_count, 1, "exactly one repair owner must remain");
    assert!(owns_new, "the released tombstone must permit replacement");
    assert!(
        live_replacement,
        "the replacement repair owner must be live"
    );
    assert_eq!(
        start_attempts, 1,
        "repair replacement must start exactly once"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn released_automation_tombstone_allows_one_eventual_replacement() {
    use crate::automation::scheduler::{AutomationSchedulerControl, save_scheduler_control};
    use crate::dashboard::AutomationSchedulerReconcileOutcome;

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "automation-tombstone-test",
    );
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let cg = super::super::open_project_for_handshake(
        &project,
        &handshake,
        &engine.store_administration,
    )
    .await
    .expect("open automation fixture through daemon authority");
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    save_scheduled_automation(&dashboard_root, true).await;
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .expect("pause scheduler work");
    let new = ProjectServerKey::from_open_project(&cg, &handshake).expect("new owner key");
    let mut old = new.clone();
    old.owner.graph_db_path = old.owner.graph_db_path.with_extension("retiring.db");
    let server = crate::mcp::McpServer::new_with_global_db(cg, None, None).await;

    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("noncooperative owner start timed out")
        .expect("noncooperative owner start sender dropped");
    engine
        .store_administration
        .project_servers()
        .lock()
        .await
        .insert(new.clone(), server);
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    let retirement_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
    let retirement = {
        let engine = engine.clone();
        let old = old.clone();
        let new = new.clone();
        let project = project.clone();
        let handshake = handshake.clone();
        tokio::spawn(async move {
            engine
                .rekey_project_maintenance(&old, new, project, handshake, true)
                .await
        })
    };
    wait_for_automation_scheduler_state(
        &engine,
        retirement_deadline,
        "retiring scheduler tombstone",
        |schedulers| {
            schedulers
                .get(&old)
                .is_some_and(|owner| owner.lifecycle == AutomationSchedulerLifecycle::Retiring)
        },
    )
    .await;
    let timed_out = tokio::time::timeout(
        remaining_test_budget(
            retirement_deadline,
            "initial scheduler retirement did not finish",
        ),
        retirement,
    )
    .await
    .expect("initial scheduler retirement did not finish")
    .expect("initial scheduler retirement task panicked");
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_automation_scheduler(new.clone(), project.clone(), handshake.clone()),
    )
    .await
    .expect("scheduler reconcile did not observe the retiring tombstone");
    let no_overlap = {
        let schedulers = engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        schedulers.len() == 1 && schedulers.contains_key(&old) && !schedulers.contains_key(&new)
    };

    release.release();
    let release_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    tokio::time::timeout(
        remaining_test_budget(
            release_deadline,
            "noncooperative owner completion timed out",
        ),
        completed_rx,
    )
    .await
    .expect("noncooperative owner completion timed out")
    .expect("noncooperative owner completion sender dropped");
    wait_for_automation_scheduler_state(
        &engine,
        release_deadline,
        "released scheduler tombstone",
        |schedulers| !schedulers.contains_key(&old),
    )
    .await;
    let replacement_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let replacement = {
        let engine = engine.clone();
        let old = old.clone();
        let new = new.clone();
        let project = project.clone();
        let handshake = handshake.clone();
        tokio::spawn(async move {
            engine
                .rekey_project_maintenance(&old, new, project, handshake, true)
                .await
        })
    };
    wait_for_automation_scheduler_state(
        &engine,
        replacement_deadline,
        "one live replacement scheduler",
        |schedulers| {
            schedulers.len() == 1
                && schedulers
                    .get(&new)
                    .and_then(|owner| owner.task.as_ref())
                    .is_some_and(|task| !task.is_finished())
        },
    )
    .await;
    let replaced = tokio::time::timeout(
        remaining_test_budget(
            replacement_deadline,
            "replacement rekey did not finish after owner publication",
        ),
        replacement,
    )
    .await
    .expect("replacement rekey did not finish after owner publication")
    .expect("replacement rekey task panicked");
    let (owner_count, owns_new, live_replacement) = {
        let schedulers = engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        (
            schedulers.len(),
            schedulers.contains_key(&new),
            schedulers
                .get(&new)
                .and_then(|owner| owner.task.as_ref())
                .is_some_and(|task| !task.is_finished()),
        )
    };
    engine.shutdown_all().await;

    assert_eq!(timed_out, super::super::MaintenanceRekeyOutcome::Retiring);
    assert_eq!(reconcile, AutomationSchedulerReconcileOutcome::Retiring);
    assert!(
        no_overlap,
        "replacement must not overlap the retiring owner"
    );
    assert_eq!(replaced, super::super::MaintenanceRekeyOutcome::Completed);
    assert_eq!(owner_count, 1, "exactly one scheduler owner must remain");
    assert!(owns_new, "the released tombstone must permit replacement");
    assert!(
        live_replacement,
        "exactly one live replacement must own the scheduler"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_project_rekeys_are_bounded_and_keep_one_repair_owner() {
    let (_fixture, _database_scope, engine, new, handshake) =
        maintenance_owner_fixture("concurrent-project-rekey-test").await;
    let project_path = handshake.project_path.clone().expect("project path");
    let mut old = new.clone();
    old.owner.graph_db_path = old.owner.graph_db_path.with_extension("retiring.db");
    let first = engine.rekey_project_maintenance(
        &old,
        new.clone(),
        project_path.clone(),
        handshake.clone(),
        true,
    );
    let second = engine.rekey_project_maintenance(&old, new.clone(), project_path, handshake, true);

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(first, second);
    })
    .await
    .expect("concurrent rekey must not deadlock");

    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert_eq!(schedulers.len(), 1);
    assert!(schedulers.contains_key(&new));
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stale_cache_retirement_does_not_duplicate_canonical_repair_owner() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        project_root: PathBuf::from("/project"),
        scope_prefix: None,
    };
    let mut canonical = old.clone();
    canonical.owner.graph_db_path = PathBuf::from("/store/canonical.db");
    let stale_task = tokio::spawn(std::future::pending::<()>());
    let stale_abort = stale_task.abort_handle();
    let canonical_task = tokio::spawn(std::future::pending::<()>());
    let canonical_abort = canonical_task.abort_handle();
    {
        let mut schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        schedulers.insert(
            old.clone(),
            MemoryRepairSchedulerHandle::for_test(stale_task),
        );
        schedulers.insert(
            canonical.clone(),
            MemoryRepairSchedulerHandle::for_test(canonical_task),
        );
    }

    engine
        .rekey_project_maintenance(
            &old,
            canonical.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        )
        .await;
    tokio::task::yield_now().await;

    assert!(stale_abort.is_finished());
    assert!(!canonical_abort.is_finished());
    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&old));
    assert!(schedulers.contains_key(&canonical));
    assert_eq!(schedulers.len(), 1);
    drop(schedulers);
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "retirement must preserve an existing canonical owner"
    );
    engine.shutdown_all().await;
}
