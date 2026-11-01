#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tracedecay_daemon_protocol::{
    DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
};

use tempfile::TempDir;
use tokio::sync::mpsc;
use tracedecay_application::primitives::PrimitiveProjectRuntime;
use tracedecay_application::project_open_authorization::project_open_work_grant;
use tracedecay_configuration::config::PinnedRuntimeConfiguration;
use tracedecay_contracts::retrieval::{
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveScope, PrimitiveRequest,
};
use tracedecay_contracts::{CancellationContext, Deadline};
use tracedecay_daemon_service::{
    DaemonWorkProposalRoutingAuthorityV1, daemon_owned_project_source_access_at,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkAuthority, canonical_sha256};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::{
    apply_project_setting_via_surface, enter_test_daemon_database_scope, git,
    initialize_test_project, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::MAX_CACHED_PROJECT_SERVERS;
use crate::daemon::engine::DaemonEngine;
use crate::daemon::invocation_dispatch::execute_daemon_invocation;
use crate::daemon::project_open_admission::ProjectRouteKey;

struct CompilerRunCompleted(mpsc::UnboundedSender<()>);

impl<S> Layer<S> for CompilerRunCompleted
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_close(&self, id: tracing::span::Id, context: Context<'_, S>) {
        if context
            .span(&id)
            .is_some_and(|span| span.name() == "usecases.diagnostics.typescript_producer")
        {
            let _ = self.0.send(());
        }
    }
}

fn install_project_compiler(project: &Path, compiler: &Path, missing: &str) {
    std::fs::create_dir_all(project.join("node_modules/.bin")).unwrap();
    std::os::unix::fs::symlink(compiler, project.join("node_modules/.bin/tsc")).unwrap();
    std::fs::write(
        project.join("node_modules/check.json"),
        format!(r#"{{"extends":"./{missing}.json"}}"#),
    )
    .unwrap();
}

async fn diagnostics(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
) -> DaemonInvocationOutcome {
    let now = tracedecay_contracts::now_micros();
    execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::primitive(
            "request.compiler-status-reopen",
            ApplicationSurfaceOperation::DiagnosticsRead,
            PrimitiveRequest::DiagnosticsRead(DiagnosticsPrimitiveRequest {
                scope: DiagnosticsPrimitiveScope::Workspace,
                maximum_diagnostics: 10,
                cursor: None,
            }),
            now,
            Deadline::new(UtcMicros(now.0 + 30_000_000)).unwrap(),
            CancellationContext::active("cancel.compiler-status-reopen").unwrap(),
        )
        .unwrap(),
    )
    .await
    .outcome
}

async fn completed_compiler_failure(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    completed: &mut mpsc::UnboundedReceiver<()>,
    expected: &str,
) -> DaemonInvocationOutcome {
    tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            let outcome = diagnostics(engine, handshake).await;
            if let DaemonInvocationOutcome::ApplicationProblem { problem } = &outcome
                && problem.diagnostic().is_some_and(|diagnostic| {
                    diagnostic.code == "application.diagnostics.producer-failed"
                        && diagnostic.message.contains(expected)
                })
            {
                return outcome;
            }
            // Siblings can finish checks too. A completed run wakes the read;
            // only this route's expected public outcome completes the wait.
            completed
                .recv()
                .await
                .expect("compiler completion observer");
        }
    })
    .await
    .expect("the production producer must expose its real tsc outcome")
}

fn compiler_failure(outcome: &DaemonInvocationOutcome) -> String {
    let DaemonInvocationOutcome::ApplicationProblem { problem } = outcome else {
        panic!("a failed compiler must answer a public typed problem: {outcome:?}");
    };
    let diagnostic = problem.diagnostic().expect("compiler failure diagnostic");
    assert_eq!(diagnostic.code, "application.diagnostics.producer-failed");
    diagnostic.message.clone()
}

async fn register_work_for_configuration(
    engine: &DaemonEngine,
    target_root: &Path,
    server: &crate::mcp::McpServer,
    configuration: &PinnedRuntimeConfiguration,
) -> (ManifestDigest, bool, Result<(), TraceDecayError>) {
    let graph = server.cg_snapshot().await;
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        graph.project_root(),
        &graph
            .configuration_runtime()
            .configuration_target()
            .project_id,
    )
    .unwrap();
    let access = daemon_owned_project_source_access_at(
        &scope,
        graph.project_root(),
        configuration,
        tracedecay_contracts::now_micros(),
    )
    .unwrap();
    let database = graph
        .store_runtime_registry()
        .mount_registered_project_sessions(scope.project_id.clone())
        .await
        .unwrap();
    let grant = project_open_work_grant(&access, tracedecay_contracts::now_micros())
        .unwrap()
        .unwrap();
    let authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        access.requester.clone(),
        grant.digest.clone(),
    )
    .unwrap();
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
    ))
    .unwrap();
    let routing = DaemonWorkProposalRoutingAuthorityV1::mount(
        scope.clone(),
        configuration,
        &access.configuration_digest,
        &grant,
    )
    .unwrap();
    let topology = tracedecay_configuration::config::topology::resolved_work_topology_policy(
        configuration.snapshot(),
    )
    .unwrap()
    .clone();
    drop(graph);
    let registrar = engine.invocation.work_runtime_registrar();
    let matches = registrar
        .authority_matches(
            target_root,
            &authority,
            &access.requester,
            &grant,
            &policy_digest,
            &access.configuration_digest,
        )
        .await;
    let grant_digest = grant.digest.clone();
    let result = registrar
        .register(
            target_root.to_path_buf(),
            database,
            authority,
            access.requester,
            grant,
            policy_digest,
            access.configuration_digest,
            topology,
            routing,
            server
                .work_evidence_retrieval(&scope, engine.invocation.work_federated_query_authority())
                .unwrap(),
        )
        .await;
    (grant_digest, matches, result)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_eviction_reopen_reads_retained_compiler_status_with_leased_sibling() {
    const CHILD: &str = "TRACEDECAY_TEST_COMPILER_STATUS_REOPEN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::route_eviction_reopen_reads_retained_compiler_status_with_leased_sibling",
            module_path!().split_once("::").unwrap().1
        );
        let home = TempDir::new().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("isolated compiler-status daemon journey");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "compiler-status daemon journey failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let compiler = PathBuf::from(
        std::env::var_os("TRACEDECAY_TEST_TYPESCRIPT_COMPILER")
            .expect("set TRACEDECAY_TEST_TYPESCRIPT_COMPILER to the installed pinned tsc"),
    )
    .canonicalize()
    .expect("installed pinned tsc");
    let version = Command::new(&compiler).arg("--version").output().unwrap();
    assert!(
        version.status.success(),
        "installed tsc must run: {version:?}"
    );
    println!(
        "installed project compiler: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    let (completed, mut completions) = mpsc::unbounded_channel();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(CompilerRunCompleted(completed)),
    )
    .unwrap();

    let temp = TempDir::new().unwrap();
    let primary = temp.path().join("primary");
    let profile = temp.path().join("profile");
    std::fs::create_dir_all(primary.join("src")).unwrap();
    std::fs::write(primary.join("src/index.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(primary.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::write(
        primary.join("tsconfig.json"),
        r#"{"extends":"./node_modules/check.json","include":["src/**/*.ts"]}"#,
    )
    .unwrap();
    git(&primary, &["init", "--quiet", "--initial-branch=main"]);
    git(&primary, &["add", "--all"]);
    git(
        &primary,
        &["commit", "--quiet", "-m", "test: compiler ownership"],
    );
    let client_identity = test_client_identity_for(profile.clone());
    initialize_test_project(&primary, &client_identity).await;
    let primary = primary.canonicalize().unwrap();
    let handshake = DaemonHandshake {
        project_path: Some(primary.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope = enter_test_daemon_database_scope(&profile, "compiler-status-reopen");
    let configuration_engine = test_daemon_engine_for_profile(&profile);
    drop(
        apply_project_setting_via_surface(&configuration_engine, &handshake, |_| {
            (
                tracedecay_domain::configuration::SettingKey::new(
                    tracedecay_domain::configuration::SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
                )
                .unwrap(),
                tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
            )
        })
        .await,
    );
    let shutdown = configuration_engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
    assert!(shutdown.background.unfinished().is_empty(), "{shutdown:?}");
    drop(configuration_engine);
    install_project_compiler(&primary, &compiler, "predecessor-missing");
    let engine = test_daemon_engine_for_profile(&profile);
    let (primary_key, _, primary_server, _) = engine.open_project_server(&handshake).await.unwrap();
    let before = completed_compiler_failure(
        &engine,
        &handshake,
        &mut completions,
        "predecessor-missing.json",
    )
    .await;
    assert!(compiler_failure(&before).contains("predecessor-missing.json"));
    let retained_runtime = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(&primary, PrimitiveProjectRuntime::clone)
        .await
        .unwrap();
    let retained_dispatch = retained_runtime.dispatch();
    drop(primary_server);

    let mut leased_siblings = Vec::new();
    let mut sibling_handshake = None;
    let mut last_sibling_handshake = None;
    for index in 0..MAX_CACHED_PROJECT_SERVERS {
        let linked = temp.path().join(format!("linked-{index}"));
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        if index == 0 {
            install_project_compiler(&linked, &compiler, "sibling-missing");
        }
        let linked_handshake = DaemonHandshake {
            project_path: Some(linked),
            ..handshake.clone()
        };
        let (key, _, server, _) = engine.open_project_server(&linked_handshake).await.unwrap();
        assert_eq!(
            key.owner, primary_key.owner,
            "linked roots share the real store owner"
        );
        last_sibling_handshake = Some(linked_handshake.clone());
        if index == 0 {
            completed_compiler_failure(
                &engine,
                &linked_handshake,
                &mut completions,
                "sibling-missing.json",
            )
            .await;
            sibling_handshake = Some(linked_handshake);
        }
        leased_siblings.push(server);
    }
    let route = ProjectRouteKey::from_handshake(&primary, &handshake).unwrap();
    {
        let mut servers = engine.store_administration.project_servers().lock().await;
        assert_eq!(servers.servers.len(), MAX_CACHED_PROJECT_SERVERS);
        assert!(
            servers.get_route(&route).is_none(),
            "the primary route must actually be evicted"
        );
        assert!(
            servers
                .retire_lru_ready_under_graph_pressure(|server| Arc::strong_count(server) > 1)
                .is_err(),
            "leased siblings must prevent whole-owner retirement"
        );
    }
    tokio::time::timeout(
        Duration::from_mins(1),
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("route eviction cleanup must finish before reopen");
    let still_retained = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(&primary, PrimitiveProjectRuntime::dispatch)
        .await
        .expect("sibling route eviction retains the invocation runtime");
    assert!(Arc::ptr_eq(&retained_dispatch, &still_retained));
    std::fs::write(
        primary.join("node_modules/check.json"),
        r#"{"extends":"./reopened-missing.json"}"#,
    )
    .unwrap();
    assert_eq!(git(&primary, &["status", "--porcelain"]), "");
    drop(leased_siblings.pop());
    let (_, _, reopened, _) = engine.open_project_server(&handshake).await.unwrap();
    let after = completed_compiler_failure(
        &engine,
        &handshake,
        &mut completions,
        "reopened-missing.json",
    )
    .await;
    let sibling = diagnostics(&engine, sibling_handshake.as_ref().unwrap()).await;
    assert!(
        compiler_failure(&after).contains("reopened-missing.json"),
        "reopen must expose the completed new check, not the retained predecessor: {after:?}"
    );
    assert!(compiler_failure(&sibling).contains("sibling-missing.json"));
    let unchanged = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(&primary, PrimitiveProjectRuntime::clone)
        .await
        .unwrap();
    assert!(
        Arc::ptr_eq(&retained_dispatch, &unchanged.dispatch()),
        "same-configuration reopen keeps the installed facade"
    );
    let old_configuration = reopened
        .cg_snapshot()
        .await
        .configuration_runtime()
        .client()
        .current()
        .await
        .unwrap();
    let (old_work_grant, old_work_matches, old_work_registration) =
        register_work_for_configuration(&engine, &primary, &reopened, &old_configuration).await;
    assert!(old_work_matches);
    old_work_registration.expect("same-configuration Work registration remains admitted");
    drop(reopened);

    // Reopen the sibling displaced by the primary. Every other route is
    // leased, so the now-idle primary is evicted while its runtime survives.
    let (_, _, last_sibling, _) = engine
        .open_project_server(last_sibling_handshake.as_ref().unwrap())
        .await
        .unwrap();
    assert!(
        engine
            .store_administration
            .project_servers()
            .lock()
            .await
            .get_route(&route)
            .is_none(),
        "the primary must be evicted before its configuration changes"
    );
    tokio::time::timeout(
        Duration::from_mins(1),
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("second route eviction cleanup");
    drop(
        apply_project_setting_via_surface(&engine, sibling_handshake.as_ref().unwrap(), |_| {
            (
                tracedecay_domain::configuration::SettingKey::new(
                    tracedecay_domain::configuration::TELEMETRY_TIMINGS_SETTING_KEY,
                )
                .unwrap(),
                tracedecay_domain::configuration::ConfigurationValueV1::Boolean(false),
            )
        })
        .await,
    );
    std::fs::write(
        primary.join("node_modules/check.json"),
        r#"{"extends":"./configured-missing.json"}"#,
    )
    .unwrap();
    drop(last_sibling);
    let (_, _, configured, _) = engine.open_project_server(&handshake).await.unwrap();
    let current_configuration = configured
        .cg_snapshot()
        .await
        .configuration_runtime()
        .client()
        .current()
        .await
        .unwrap();
    assert_ne!(
        old_configuration.revision_id(),
        current_configuration.revision_id(),
        "reopen must encounter the configuration committed through its sibling"
    );
    let refreshed = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(&primary, PrimitiveProjectRuntime::clone)
        .await
        .unwrap();
    assert!(
        !Arc::ptr_eq(&retained_dispatch, &refreshed.dispatch()),
        "changed configuration replaces the complete primitive facade"
    );
    let after_configuration = completed_compiler_failure(
        &engine,
        &handshake,
        &mut completions,
        "configured-missing.json",
    )
    .await;
    assert!(compiler_failure(&after_configuration).contains("configured-missing.json"));
    let (current_work_grant, current_work_matches, current_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &current_configuration)
            .await;
    assert_eq!(
        old_work_grant, current_work_grant,
        "telemetry changes preserve durable Work grant identity"
    );
    assert!(
        current_work_matches,
        "reopen must already have refreshed the Work configuration authority"
    );
    current_work_registration.expect("current Work configuration remains admitted");
    let (_, stale_work_matches, stale_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &old_configuration).await;
    assert!(!stale_work_matches);
    assert!(
        stale_work_registration.is_err(),
        "a delayed old configuration cannot replace current Work authority"
    );
    let sibling_server = leased_siblings.first().unwrap();
    let (_, foreign_work_matches, foreign_work_registration) =
        register_work_for_configuration(&engine, &primary, sibling_server, &current_configuration)
            .await;
    assert!(!foreign_work_matches);
    assert!(
        foreign_work_registration.is_err(),
        "a linked sibling cannot replace the primary's Work authority"
    );
    let (_, preserved_work_matches, preserved_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &current_configuration)
            .await;
    assert!(preserved_work_matches);
    preserved_work_registration.expect("rejected candidates leave current Work authority intact");

    // Retain A while a route captures B, then commit C before the route can
    // publish. The configuration writer fence must refuse B without changing A.
    drop(
        apply_project_setting_via_surface(&engine, sibling_handshake.as_ref().unwrap(), |_| {
            (
                tracedecay_domain::configuration::SettingKey::new(
                    tracedecay_domain::configuration::TELEMETRY_TIMINGS_SETTING_KEY,
                )
                .unwrap(),
                tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
            )
        })
        .await,
    );
    let candidate_configuration = configured
        .cg_snapshot()
        .await
        .configuration_runtime()
        .client()
        .current()
        .await
        .unwrap();
    assert_ne!(
        current_configuration.revision_id(),
        candidate_configuration.revision_id()
    );
    let (candidate_captured, allow_refresh) = engine
        .invocation
        .service
        .pause_work_runtime_refresh(primary.clone())
        .await;
    let ((_, raced_matches, raced_registration), latest_configuration) = tokio::join!(
        register_work_for_configuration(&engine, &primary, &configured, &candidate_configuration),
        async {
            candidate_captured.await.expect("Work candidate captured");
            drop(
                apply_project_setting_via_surface(
                    &engine,
                    sibling_handshake.as_ref().unwrap(),
                    |_| {
                        (
                            tracedecay_domain::configuration::SettingKey::new(
                                tracedecay_domain::configuration::TELEMETRY_TIMINGS_SETTING_KEY,
                            )
                            .unwrap(),
                            tracedecay_domain::configuration::ConfigurationValueV1::Boolean(false),
                        )
                    },
                )
                .await,
            );
            let latest = configured
                .cg_snapshot()
                .await
                .configuration_runtime()
                .client()
                .current()
                .await
                .unwrap();
            assert_ne!(candidate_configuration.revision_id(), latest.revision_id());
            allow_refresh.send(()).expect("resume Work refresh");
            latest
        },
    );
    assert!(!raced_matches);
    assert!(
        matches!(raced_registration, Err(TraceDecayError::Config { ref message })
            if message == "Work configuration changed during reopen"),
        "a durable commit before publication must refuse the superseded candidate: {raced_registration:?}"
    );
    let (_, unchanged_work_matches, unchanged_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &current_configuration)
            .await;
    assert!(
        unchanged_work_matches,
        "refusing B must leave the complete previously installed A authority intact"
    );
    unchanged_work_registration.expect("same-pin admission remains valid after a later commit");
    let (_, latest_work_matches, latest_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &latest_configuration)
            .await;
    assert!(!latest_work_matches);
    latest_work_registration.expect("the durable current C authority can be installed");
    let (_, installed_work_matches, installed_work_registration) =
        register_work_for_configuration(&engine, &primary, &configured, &latest_configuration)
            .await;
    assert!(installed_work_matches);
    installed_work_registration.expect("C remains admitted after publication");

    assert!(Arc::ptr_eq(
        &retained_runtime.compiler_status().current().unwrap(),
        &refreshed.compiler_status().current().unwrap(),
    ));

    let mut delayed_reopen = refreshed.clone();
    assert!(
        delayed_reopen
            .reconcile_reopen(&unchanged, &retained_runtime)
            .is_err(),
        "a delayed old-configuration candidate cannot replace the refreshed facade"
    );
    assert!(Arc::ptr_eq(
        &delayed_reopen.dispatch(),
        &refreshed.dispatch()
    ));
    let sibling_runtime = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(
            sibling_handshake
                .as_ref()
                .unwrap()
                .project_path
                .as_ref()
                .unwrap(),
            PrimitiveProjectRuntime::clone,
        )
        .await
        .unwrap();
    assert!(
        delayed_reopen
            .reconcile_reopen(&refreshed, &sibling_runtime)
            .is_err(),
        "a linked sibling's source authority cannot replace the primary's"
    );
    let sibling_after = diagnostics(&engine, sibling_handshake.as_ref().unwrap()).await;
    assert!(compiler_failure(&sibling_after).contains("sibling-missing.json"));
    drop((
        retained_runtime,
        retained_dispatch,
        still_retained,
        unchanged,
        refreshed,
        delayed_reopen,
        sibling_runtime,
        configured,
        leased_siblings,
    ));
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
    assert!(shutdown.background.unfinished().is_empty(), "{shutdown:?}");
    assert!(engine.invocation.service.project_runtimes.is_empty());
    println!("public diagnostics observed retained, reconfigured, and isolated sibling owners");
}
