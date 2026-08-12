#[cfg(unix)]
use super::*;

#[cfg(unix)]
use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
};

#[cfg(unix)]
struct AutomationExitBarrierRelease(Arc<AutomationSchedulerExitBarrier>);

#[cfg(unix)]
impl Drop for AutomationExitBarrierRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(unix)]
#[test]
fn automation_scheduler_starts_when_any_task_has_interval() {
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    };

    let mut config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        combine_due_tasks: false,
        tasks: Default::default(),
        ..AutomationConfig::default()
    };
    config.tasks.memory_curator = AutomationTaskConfig {
        enabled: true,
        schedule: Some("every:5m".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };

    assert!(super::super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("manual".to_string());
    assert!(!super::super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("interval".to_string());
    config.tasks.memory_curator.interval_secs = None;
    assert!(!super::super::automation_scheduler_configured(&config));
    config.tasks.memory_curator.interval_secs = Some(300);
    assert!(super::super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.enabled = false;
    config.tasks.session_reflector = AutomationTaskConfig {
        enabled: true,
        schedule: Some("hourly".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };
    assert!(super::super::automation_scheduler_configured(&config));

    config.tasks.session_reflector.enabled = false;
    config.tasks.skill_writer = AutomationTaskConfig {
        enabled: true,
        schedule: Some("daily".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };
    assert!(super::super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("every:5m".to_string());
    config.backend = AutomationBackend::Disabled;
    assert!(!super::super::automation_scheduler_configured(&config));

    config.backend = AutomationBackend::CodexAppServer;
    config.host_mode = AutomationHostMode::DelegatedHost;
    assert!(!super::super::automation_scheduler_configured(&config));

    config.host_mode = AutomationHostMode::Standalone;
    config.enabled = false;
    assert!(!super::super::automation_scheduler_configured(&config));
}

#[cfg(unix)]
#[test]
fn fresh_v2_configuration_is_scheduler_work() {
    assert!(super::super::automation_scheduler_configured(
        &tracedecay_agent_hosts::automation::config::AutomationConfig::default()
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn automation_scheduler_tick_secs_reads_pinned_project_configuration() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-tick-config-test",
    );
    let server = apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            scheduler_tick_secs: Some(17),
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let cg = server.cg().await;

    let tick_secs = Box::pin(super::super::automation_scheduler_tick_secs_for_project(
        &cg, &handshake,
    ))
    .await;

    assert_eq!(tick_secs, 17);
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_v2_project_starts_the_required_automation_scheduler() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    initialize_test_project(&project, &handshake.client_identity).await;
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-fresh-default-test",
    );
    let cg = super::super::open_project_for_handshake(
        &project,
        &handshake,
        &engine.store_administration,
    )
    .await
    .expect("open scheduler fixture through daemon authority");
    let key =
        super::super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(schedulers.contains_key(&key));
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_scheduler_discovery_without_work_does_not_wait_for_writer_gate() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    initialize_test_project(&project, &handshake.client_identity).await;
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-discovery-writer-gate-test",
    );
    let cg = Arc::new(
        super::super::open_project_for_handshake(
            &project,
            &handshake,
            &engine.store_administration,
        )
        .await
        .expect("open scheduler discovery fixture through daemon authority"),
    );
    let key =
        super::super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

    let store_administration = engine.store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        store_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    let discovery = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        engine.activate_automation_scheduler_for_open_project(key, project, handshake, cg),
    )
    .await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    discovery.expect("read-only scheduler discovery must not wait for the writer gate");
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_scheduler_skips_stale_owner_key_after_rekey() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-stale-owner-test",
    );
    let server = apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:5m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let cg = server.cg().await;
    let stale_key = super::super::ProjectServerKey::from_open_project(&cg, &handshake)
        .expect("stale owner key");

    let mut current_key = stale_key.clone();
    current_key.scope_prefix = Some("rekeyed".to_string());
    {
        let mut owners = engine.store_administration.project_servers().lock().await;
        assert!(owners.rekey(&stale_key, &current_key));
    }

    engine
        .activate_automation_scheduler_for_open_project(stale_key.clone(), project, handshake, cg)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(
        !schedulers.contains_key(&stale_key),
        "scheduler discovery must not start under a key that no longer owns the project server"
    );
    assert!(schedulers.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn disabled_finished_scheduler_reenables_with_a_fresh_owner() {
    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-reenable-test",
    );
    let server = save_scheduled_automation(&engine, &handshake, false).await;
    let cg = server.cg().await;
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let finished = tokio::spawn(async {});
    wait_for_finished_task(
        &finished,
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(2),
        "finished scheduler task",
    )
    .await;
    assert!(finished.is_finished());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));

    save_scheduled_automation(&engine, &handshake, true).await;
    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    let owner = schedulers.get(&key).expect("re-enabled scheduler owner");
    assert!(
        !owner
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished(),
        "re-enable must replace a finished scheduler handle"
    );
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_reenable_creates_one_live_scheduler_owner() {
    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&client_identity.profile_root, "concurrent-reenable-test");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let server = save_scheduled_automation(&engine, &handshake, true).await;
    let cg = server.cg().await;
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished = tokio::spawn(async move {
        let _ = finished_tx.send(());
    });
    tokio::time::timeout(tokio::time::Duration::from_secs(2), finished_rx)
        .await
        .expect("finished owner barrier timed out")
        .expect("finished owner barrier sender dropped");
    engine
        .automation_configured_override
        .store(true, std::sync::atomic::Ordering::Relaxed);
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));

    let reenable_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(4);
    let (first_completed_tx, first_completed_rx) = tokio::sync::oneshot::channel();
    let first = {
        let engine = engine.clone();
        let key = key.clone();
        let project = project.clone();
        let handshake = handshake.clone();
        tokio::spawn(async move {
            let outcome = engine
                .ensure_automation_scheduler(key, project, handshake)
                .await;
            let _ = first_completed_tx.send(());
            outcome
        })
    };
    let (second_completed_tx, second_completed_rx) = tokio::sync::oneshot::channel();
    let second = {
        let engine = engine.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let outcome = engine
                .ensure_automation_scheduler(key, project, handshake)
                .await;
            let _ = second_completed_tx.send(());
            outcome
        })
    };

    tokio::time::timeout(
        remaining_test_budget(
            reenable_deadline,
            "timed out waiting for concurrent re-enable completion",
        ),
        async { tokio::try_join!(first_completed_rx, second_completed_rx) },
    )
    .await
    .expect("timed out waiting for concurrent re-enable completion")
    .expect("concurrent re-enable completion sender dropped");
    let (first, second) = tokio::time::timeout(
        remaining_test_budget(
            reenable_deadline,
            "concurrent re-enable tasks did not finish after owner publication",
        ),
        async { tokio::try_join!(first, second) },
    )
    .await
    .expect("concurrent re-enable tasks did not finish after owner publication")
    .expect("concurrent re-enable task panicked");

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert_eq!(schedulers.len(), 1);
    assert!(
        !schedulers
            .get(&key)
            .unwrap()
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished()
    );
    drop(schedulers);
    engine.shutdown_all().await;

    assert!(matches!(
        (first, second),
        (
            crate::dashboard::AutomationSchedulerReconcileOutcome::Started,
            crate::dashboard::AutomationSchedulerReconcileOutcome::RunningNotified
        ) | (
            crate::dashboard::AutomationSchedulerReconcileOutcome::RunningNotified,
            crate::dashboard::AutomationSchedulerReconcileOutcome::Started
        )
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn unavailable_host_admission_spool_does_not_block_project_server_open() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let layout = initialize_test_project(&project, &client_identity).await;
    let session_db_path = layout.sessions_db_path;
    let spool_path = session_db_path.parent().unwrap().join(format!(
        ".{}.host-admission",
        session_db_path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&spool_path, "not a directory").expect("block spool directory open");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "unavailable-host-admission-test",
    );

    engine
        .project_server(&handshake)
        .await
        .expect("read/query server must open when admission spool is unavailable");
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn profile_reconcile_broadcasts_to_cached_projects_without_opening_uncached_projects() {
    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let profile_root = dir.path().join("profile");
    let first_project = dir.path().join("first");
    let second_project = dir.path().join("second");
    let uncached_project = dir.path().join("uncached");
    for project in [&first_project, &second_project, &uncached_project] {
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    }
    let client_identity = test_client_identity_for(profile_root.clone());
    for project in [&first_project, &second_project, &uncached_project] {
        initialize_test_project(project, &client_identity).await;
    }
    let first_handshake = DaemonHandshake {
        project_path: Some(first_project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let second_handshake = DaemonHandshake {
        project_path: Some(second_project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "profile-reconcile-test");
    let first_server = save_scheduled_automation(&engine, &first_handshake, true).await;
    let first_cg = first_server.cg().await;
    let first_key = ProjectServerKey::from_open_project(&first_cg, &first_handshake)
        .expect("first cached owner key");
    let second_server = save_scheduled_automation(&engine, &second_handshake, true).await;
    let second_cg = second_server.cg().await;
    let second_key = ProjectServerKey::from_open_project(&second_cg, &second_handshake)
        .expect("second cached owner key");
    engine
        .activate_automation_scheduler_for_open_project(
            first_key,
            first_project.clone(),
            first_handshake.clone(),
            first_cg,
        )
        .await;
    engine
        .activate_automation_scheduler_for_open_project(
            second_key,
            second_project.clone(),
            second_handshake.clone(),
            second_cg,
        )
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial scheduler probes timed out");
    let opens_before = engine
        .project_open_attempts
        .load(std::sync::atomic::Ordering::Relaxed);

    let params = json!({
        "name": "tracedecay_admin_project",
        "arguments": {
            "action": "automation_reconcile",
            "scope": "profile"
        }
    });
    let response = super::super::projectless_tools_call_response(
        json!(73),
        Some(&params),
        &client_identity,
        &engine.store_administration,
    )
    .await;
    assert!(
        response.error.is_none(),
        "projectless profile reconcile must succeed: {:?}",
        response.error
    );
    let result = response.result.expect("profile reconcile result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("profile reconcile JSON text");
    let report: Value = serde_json::from_str(text).expect("profile reconcile report");
    assert_eq!(report["scope"], "profile");
    assert_eq!(report["cached_owners"], 2);
    assert_eq!(
        report["uncached_projects"],
        "deferred_until_project_startup"
    );
    let outcomes = report["outcomes"]
        .as_array()
        .expect("profile reconcile owner outcomes");
    assert_eq!(outcomes.len(), 2);
    for owner in outcomes {
        assert!(
            owner["project_id"].as_str().is_some(),
            "each profile outcome must carry stable project identity: {owner}"
        );
        assert!(
            owner["store_root"].as_str().is_some(),
            "each profile outcome must carry stable store identity: {owner}"
        );
        assert!(
            owner["graph_db_path"].as_str().is_some(),
            "each profile outcome must identify its physical graph store: {owner}"
        );
        assert!(
            owner["outcome"].as_str().is_some(),
            "each profile owner must carry its typed reconcile outcome: {owner}"
        );
    }
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        opens_before,
        "profile reconcile must not open uncached projects"
    );
    wait_for_automation_scheduler_state(
        &engine,
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(4),
        "profile scheduler broadcast",
        |schedulers| schedulers.len() == 2,
    )
    .await;

    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cached_project_reconciles_cli_enabled_automation_without_cache_probe() {
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
    };

    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "cached-project-reconcile-test",
    );

    let server = engine
        .project_server(&handshake)
        .await
        .expect("cache unconfigured project");
    let cg = server.cg().await;
    let key =
        super::super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial automation config reconciliation timed out");
    let cached = engine
        .project_server(&handshake)
        .await
        .expect("cached project server");
    assert!(Arc::ptr_eq(&server, &cached));
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache hits must not provide config reconciliation"
    );
    assert!(
        !engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .contains_key(&key)
    );

    apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:5m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await;

    let finished = tokio::spawn(async {});
    wait_for_finished_task(
        &finished,
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(2),
        "finished scheduler task",
    )
    .await;
    assert!(finished.is_finished());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));
    let request = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_admin_project",
            "arguments": {
                "action": "automation_reconcile",
                "scope": "project"
            }
        }
    }))
    .expect("automation reconcile request");
    let response = server
        .handle_request(&request)
        .await
        .expect("automation reconcile response");
    assert!(response.error.is_none(), "daemon reconcile must succeed");
    let result = response.result.as_ref().expect("reconcile result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("reconcile JSON text");
    let report: Value = serde_json::from_str(text).expect("reconcile report");
    assert_eq!(
        report["outcome"], "started",
        "a finished handle must not be reported as a successful notification"
    );
    wait_for_automation_scheduler_state(
        &engine,
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(4),
        "automation scheduler reconciliation",
        |schedulers| schedulers.contains_key(&key),
    )
    .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(
        !schedulers
            .get(&key)
            .expect("reconciled scheduler")
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished(),
        "finished handle notification must be replaced, not reported as success"
    );
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn disabled_scheduler_reconcile_cannot_acknowledge_an_owner_that_then_exits() {
    use crate::dashboard::AutomationSchedulerReconcileOutcome;
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
    };
    use tracedecay_agent_hosts::automation::scheduler::{
        AutomationSchedulerControl, save_scheduler_control,
    };

    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let layout = initialize_test_project(&project, &client_identity).await;
    let dashboard_root = layout.dashboard_root;
    let scheduled = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("every:5m".to_string())),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "scheduler-exit-reconcile-test",
    );
    let server =
        apply_project_automation_patch_via_surface(&engine, &handshake, scheduled.clone()).await;
    let cg = server.cg().await;
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .expect("pause scheduler work");
    let barrier = Arc::new(AutomationSchedulerExitBarrier::new());
    let barrier_release = AutomationExitBarrierRelease(Arc::clone(&barrier));
    *engine.automation_scheduler_exit_barrier.lock().await = Some(Arc::clone(&barrier));
    // ensure starts the loop without MCP project_server open (schema-contract
    // reject on session_temporal_generations unique(session_id) vs partial index).
    assert_eq!(
        engine
            .ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone())
            .await,
        AutomationSchedulerReconcileOutcome::Started
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let changed = engine.automation_scheduler_state_changed.notified();
            tokio::pin!(changed);
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .contains_key(&key)
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("scheduler start timed out");

    apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(false),
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let wake = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .get(&key)
        .expect("live scheduler")
        .wake
        .clone();
    wake.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        barrier.wait_until_reached(),
    )
    .await
    .expect("scheduler did not reach disabled-read barrier");

    apply_project_automation_patch_via_surface(&engine, &handshake, scheduled).await;
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        engine.ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone()),
    )
    .await;
    barrier.release();
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        barrier.wait_for_decision(),
    )
    .await;
    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    let owner_count = schedulers.len();
    let owner_live = schedulers
        .get(&key)
        .and_then(|owner| owner.task.as_ref())
        .is_some_and(|task| !task.is_finished());
    drop(schedulers);
    engine.shutdown_all().await;
    drop(barrier_release);

    assert_eq!(
        reconcile,
        Ok(AutomationSchedulerReconcileOutcome::RunningNotified)
    );
    assert_eq!(
        decision,
        Ok(AutomationSchedulerExitBarrier::CONTINUE),
        "the generation acknowledged by reconcile must cancel the pending exit"
    );
    assert_eq!(owner_count, 1, "exactly one scheduler owner must remain");
    assert!(
        owner_live,
        "the acknowledged scheduler owner must still be live"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn automation_scheduler_tick_respects_pause_control_without_backend_call() {
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
    };
    use tracedecay_agent_hosts::automation::run_ledger::load_run_records;
    use tracedecay_agent_hosts::automation::scheduler::{
        AutomationSchedulerControl, save_scheduler_control,
    };

    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let layout = initialize_test_project(&project, &client_identity).await;
    let dashboard_root = layout.dashboard_root;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "paused-scheduler-tick-test",
    );
    let server = apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:1m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let cg = server.cg().await;
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .expect("save paused scheduler control");

    let run_control = tracedecay_agent_hosts::automation::AutomationRunControl::from_interrupted(
        std::sync::Arc::new(|| false),
    );
    Box::pin(super::super::run_automation_scheduler_tick(
        &project,
        &cg,
        &handshake,
        &engine,
        &run_control,
    ))
    .await
    .expect("paused scheduler tick should exit cleanly");

    let records = load_run_records(&dashboard_root, 10)
        .await
        .expect("load run ledger");
    assert!(
        records.is_empty(),
        "paused scheduler tick must not call backends or append run records"
    );
}
