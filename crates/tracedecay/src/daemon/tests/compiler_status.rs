#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::mpsc;
use tracedecay_application::primitives::PrimitiveProjectRuntime;
use tracedecay_contracts::retrieval::{
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveScope, PrimitiveRequest,
};
use tracedecay_contracts::{CancellationContext, Deadline};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::{
    apply_project_setting_via_surface, enter_test_daemon_database_scope, git,
    initialize_test_project, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::{
    DaemonEngine, DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
    MAX_CACHED_PROJECT_SERVERS, ProjectRouteKey, execute_daemon_invocation,
};

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

async fn compiler_completed(completed: &mut mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(Duration::from_mins(1), completed.recv())
        .await
        .expect("the production producer must finish its real tsc check")
        .expect("compiler completion observer");
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

fn compiler_failure(outcome: &DaemonInvocationOutcome) -> String {
    let DaemonInvocationOutcome::ApplicationProblem { problem } = outcome else {
        panic!("a failed compiler must answer a public typed problem: {outcome:?}");
    };
    let diagnostic = problem.diagnostic().expect("compiler failure diagnostic");
    assert_eq!(diagnostic.code, "application.diagnostics.producer-failed");
    diagnostic.message.clone()
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
    compiler_completed(&mut completions).await;
    let before = diagnostics(&engine, &handshake).await;
    assert!(compiler_failure(&before).contains("predecessor-missing.json"));
    let retained_dispatch = engine
        .invocation
        .service
        .project_runtimes
        .read::<PrimitiveProjectRuntime, _, _>(&primary, PrimitiveProjectRuntime::dispatch)
        .await
        .unwrap();
    drop(primary_server);

    let mut leased_siblings = Vec::new();
    let mut sibling_handshake = None;
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
        if index == 0 {
            compiler_completed(&mut completions).await;
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
    compiler_completed(&mut completions).await;
    let after = diagnostics(&engine, &handshake).await;
    let sibling = diagnostics(&engine, sibling_handshake.as_ref().unwrap()).await;
    drop((retained_dispatch, still_retained, reopened, leased_siblings));
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
    assert!(shutdown.background.unfinished().is_empty(), "{shutdown:?}");
    assert!(engine.invocation.service.project_runtimes.is_empty());
    assert!(
        compiler_failure(&after).contains("reopened-missing.json"),
        "reopen must expose the completed new check, not the retained predecessor: {after:?}"
    );
    assert!(compiler_failure(&sibling).contains("sibling-missing.json"));
    println!("public diagnostics observed predecessor, reopened, and isolated sibling failures");
}
