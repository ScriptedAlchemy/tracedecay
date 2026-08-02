use super::*;
#[cfg(unix)]
use crate::application::context::CancellationToken;
use crate::daemon::ProductionProjectCompositionHarnessV1;
use crate::daemon::{ProjectServerRequirement, project_server_requirement};
#[cfg(unix)]
use crate::errors::TraceDecayError;
use crate::mcp::JsonRpcResponse;
#[cfg(unix)]
use std::process::Command;

static PRODUCTION_DASHBOARD_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn bootstrap_tool_catalog_uses_project_node_count() {
    let request: super::super::JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    }))
    .expect("tools/list request");
    let response = super::super::daemon_bootstrap_response(&request, None, Some(65_395))
        .expect("bootstrap response")
        .expect("tools/list response");
    let result = response.result.expect("tools/list result");
    let context_description = result["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("tracedecay_context"))
        .and_then(|tool| tool["description"].as_str())
        .expect("context tool description");

    assert!(context_description.contains("5 calls maximum"));
    assert!(context_description.contains("65395 nodes"));
}

fn project_open_test_route(name: &str) -> ProjectRouteKey {
    ProjectRouteKey {
        profile_root: std::path::PathBuf::from(format!("/profiles/{name}")),
        global_db_path: std::path::PathBuf::from(format!("/profiles/{name}/global.db")),
        project_path: std::path::PathBuf::from(format!("/projects/{name}")),
        scope_prefix: None,
    }
}

#[test]
fn hook_runtime_reset_counter_only_requires_core_publication() {
    let reset = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_hook_runtime",
            "arguments": {"action": "reset_counter"}
        }
    })
    .to_string();
    let status = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_status",
            "arguments": {"format": "json"}
        }
    })
    .to_string();

    assert_eq!(
        project_server_requirement(&reset),
        ProjectServerRequirement::Core
    );
    assert_eq!(
        project_server_requirement(&status),
        ProjectServerRequirement::Core
    );
}

#[test]
fn hook_runtime_ingest_waits_for_registered_project_authority_publication() {
    let ingest = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_hook_runtime",
            "arguments": {
                "action": "ingest_transcript",
                "provider": "cursor",
                "user_scope": false,
                "event_json": "{}"
            }
        }
    })
    .to_string();

    assert_eq!(
        project_server_requirement(&ingest),
        ProjectServerRequirement::RegisteredHostIngest
    );
}

#[test]
fn hook_runtime_missing_malformed_or_unknown_action_waits_for_registration() {
    for arguments in [
        json!({}),
        json!({"action": 42}),
        json!({"action": "unknown"}),
    ] {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_hook_runtime",
                "arguments": arguments
            }
        })
        .to_string();

        assert_eq!(
            project_server_requirement(&request),
            ProjectServerRequirement::RegisteredHostIngest
        );
    }
}

#[test]
fn hook_event_waits_for_registered_project_authority_publication() {
    let hook_event = json!({
        "jsonrpc": "2.0",
        "method": crate::daemon::HOOK_EVENT_METHOD,
        "params": {}
    })
    .to_string();

    assert_eq!(
        project_server_requirement(&hook_event),
        ProjectServerRequirement::RegisteredHostIngest
    );
}

#[cfg(unix)]
fn run_git(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
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
}

#[cfg(unix)]
fn assert_missing_enrollment_admission(error: &TraceDecayError) {
    match error {
        TraceDecayError::Config { message } => {
            assert!(
                message.contains("is not enrolled"),
                "expected missing-enrollment admission error, got: {error}"
            );
        }
        // Add the typed MissingEnrollment admission variant here once exposed.
        other => panic!("expected missing-enrollment admission error, got: {other}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unenrolled_ambient_directory_is_rejected_before_project_warmup() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join(".tracedecay");
    let ambient_directory = home.path().join("ambient");
    std::fs::create_dir_all(&ambient_directory).expect("create ambient directory");
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "unenrolled ambient route");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let mut handshake = test_handshake_defaults();
    handshake.project_path = Some(ambient_directory.clone());
    handshake.client_identity = test_client_identity_for(profile_root);

    let error = match engine.begin_project_open(handshake, None).await {
        Ok(_) => panic!("unenrolled ambient directory must not start project warm-up"),
        Err(error) => error,
    };

    assert_missing_enrollment_admission(&error);
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the expensive project-open operation must not start"
    );
    assert!(
        !ambient_directory.join(".tracedecay").exists(),
        "rejection must not manufacture project state"
    );
}

/// Enrolls `project_root` on disk exactly as a previously-initialized project
/// is enrolled — an in-repo enrollment marker plus a materialized profile store
/// — without touching the profile registry. This is the on-disk shape a profile
/// parked at the forward-only migration boundary recovers with: the derived
/// registry is fresh, every project's durable enrollment survives.
#[cfg(unix)]
fn enroll_project_on_disk_only(
    project_root: &std::path::Path,
    profile_root: &std::path::Path,
    project_id: &str,
) -> crate::storage::StoreLayout {
    let marker = crate::storage::EnrollmentMarker {
        project_id: project_id.to_owned(),
        storage_mode: crate::storage::StorageMode::ProfileSharded,
    };
    crate::storage::write_enrollment_marker(project_root, &marker).expect("enrollment marker");
    let layout = crate::storage::profile_sharded_layout(project_root, profile_root, &marker)
        .expect("layout");
    std::fs::create_dir_all(&layout.data_root).expect("profile store root");
    std::fs::write(&layout.graph_db_path, b"existing graph store").expect("graph store");
    layout
}

/// Regression: a post-boundary post-update runs its startup-health probe as an
/// ordinary daemon tool call, which cannot pass `allow_init`. Before this fix
/// the pre-admission guard consulted only the profile registry, so a project
/// whose store was fully intact on disk was refused as "not enrolled", the
/// forward-only post-update treated that as fatal, and the boundary re-parked.
/// Admission must instead honour the same durable enrollment the authoritative
/// layout resolver consults first, so the existing store is mounted.
#[cfg(unix)]
#[tokio::test]
async fn durably_enrolled_project_is_admitted_after_a_registry_reset() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let project = root.join("repository");
    std::fs::create_dir_all(&project).expect("create repository");
    run_git(&project, &["init", "--quiet"]);
    let layout = enroll_project_on_disk_only(&project, &profile_root, "proj_forward_boundary");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "post-boundary registry reset");
    let engine = test_daemon_engine_for_profile(&profile_root);

    // The registry is the fresh one forward recovery brought the daemon up on.
    let registry = engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("profile registry");
    assert!(
        registry
            .project_registry_context_by_alias(&project)
            .await
            .expect("registry lookup")
            .is_none(),
        "fixture must reproduce the post-boundary fresh registry"
    );

    engine
        .ensure_registered_project_route(&project, false)
        .await
        .expect("a durably enrolled project must be admitted without allow_init");

    // Admission must mount the recovered store, never mint a replacement.
    let marker = crate::storage::read_enrollment_marker(&project)
        .expect("read enrollment marker")
        .expect("enrollment marker retained");
    assert_eq!(marker.project_id, "proj_forward_boundary");
    assert!(
        layout.graph_db_path.is_file(),
        "the pre-existing store must be left intact"
    );
}

/// The guard still refuses a project whose enrollment marker points at a store
/// that is not on disk, so the widened admission cannot resurrect a route with
/// nothing behind it.
#[cfg(unix)]
#[tokio::test]
async fn enrollment_marker_without_a_store_is_still_rejected() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let project = root.join("repository");
    std::fs::create_dir_all(&project).expect("create repository");
    run_git(&project, &["init", "--quiet"]);
    let layout = enroll_project_on_disk_only(&project, &profile_root, "proj_store_absent");
    std::fs::remove_dir_all(&layout.data_root).expect("remove profile store");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "enrollment marker without store");
    let engine = test_daemon_engine_for_profile(&profile_root);

    let error = match engine
        .ensure_registered_project_route(&project, false)
        .await
    {
        Ok(()) => panic!("a marker with no store on disk must not be admitted"),
        Err(error) => error,
    };
    assert_missing_enrollment_admission(&error);
}

#[cfg(unix)]
#[tokio::test]
async fn unenrolled_leaf_is_rejected_from_cache_and_direct_open() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join(".tracedecay");
    let repository = home.path().join("repository");
    std::fs::create_dir_all(&repository).expect("create repository");
    run_git(&repository, &["init", "--quiet"]);
    let leaf_directory = repository.join("leaf");
    std::fs::create_dir_all(&leaf_directory).expect("create leaf directory");
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "direct unenrolled leaf route");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let mut handshake = test_handshake_defaults();
    handshake.project_path = Some(leaf_directory.clone());
    handshake.client_identity = test_client_identity_for(profile_root);
    handshake.allow_init = true;

    let cache_error = match engine.cached_project_server(&handshake).await {
        Ok(_) => panic!("cache lookup must enforce enrollment admission"),
        Err(error) => error,
    };
    assert_missing_enrollment_admission(&cache_error);

    let cancellation = CancellationToken::new();
    let open_error = match engine
        .open_project_server_until_cancelled(&handshake, &cancellation)
        .await
    {
        Ok(_) => panic!("direct project open must enforce enrollment admission"),
        Err(error) => error,
    };
    assert_missing_enrollment_admission(&open_error);
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the expensive project-open operation must not start"
    );
    assert!(
        !leaf_directory.join(".tracedecay").exists(),
        "rejection must not manufacture project state"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn linked_worktree_root_is_not_admitted_as_first_touch_project() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("create primary repository");
    run_git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::write(primary.join("README.md"), "first touch authority\n").expect("fixture");
    run_git(&primary, &["add", "."]);
    run_git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "feature/first-touch",
            linked.to_str().expect("utf-8 linked path"),
            "HEAD",
        ],
    );

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "linked first-touch rejection");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity: test_client_identity_for(profile_root),
        allow_init: true,
        ..test_handshake_defaults()
    };

    let error = match engine.project_server(&handshake).await {
        Ok(_) => panic!("linked worktree must not claim first-touch project authority"),
        Err(error) => error,
    };

    assert_missing_enrollment_admission(&error);
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "linked first-touch rejection must precede project opening"
    );
    assert!(
        crate::storage::read_enrollment_marker(&linked)
            .expect("read linked marker")
            .is_none(),
        "rejection must not write a linked-worktree enrollment marker"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn same_identity_worktree_and_primary_register_one_project_authority() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("create primary repository");
    run_git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::write(primary.join("README.md"), "shared authority\n").expect("fixture");
    run_git(&primary, &["add", "."]);
    run_git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--force",
            linked.to_str().expect("utf-8 linked path"),
            "main",
        ],
    );

    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&primary, &client_identity).await;
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "shared worktree authority");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let primary_handshake = DaemonHandshake {
        project_path: Some(primary.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let linked_handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

    let primary_server = engine
        .project_server(&primary_handshake)
        .await
        .expect("primary project must open");
    let linked_server = engine
        .project_server(&linked_handshake)
        .await
        .expect("linked worktree must reuse the primary authority");

    assert!(
        Arc::ptr_eq(&primary_server, &linked_server),
        "both routes must resolve one retained project server"
    );
    let servers = engine.store_administration.project_servers().lock().await;
    assert_eq!(servers.servers.len(), 1, "one physical project server key");
    assert_eq!(servers.aliases.len(), 2, "primary and linked route aliases");
    drop(servers);
    assert!(
        crate::storage::read_enrollment_marker(&linked)
            .expect("read linked marker")
            .is_none(),
        "linked route must not acquire a second enrollment marker"
    );
    engine.shutdown_all().await;
}

#[tokio::test]
async fn repeated_bootstrap_requests_share_one_bounded_invariant_open_failure() {
    let tasks = super::super::ProjectOpenTasks::default();
    let route = project_open_test_route("rejected");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first_attempts = Arc::clone(&attempts);
    let first = tasks
        .start(route.clone(), async move {
            first_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(crate::errors::TraceDecayError::Database {
                message: "session temporal receipts or cursor keys are mutable".to_string(),
                operation: "ensure global database authority invariants".to_string(),
            })
        })
        .await;
    let first = match first {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => {
            panic!("first route request must start one open task")
        }
        super::super::ProjectOpenTaskClaim::Saturated => {
            panic!("first route request must fit the bounded task registry")
        }
    };

    for _ in 0..32 {
        let repeated_attempts = Arc::clone(&attempts);
        let claim = tokio::time::timeout(
            tokio::time::Duration::from_millis(50),
            tasks.start(route.clone(), async move {
                repeated_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }),
        )
        .await
        .expect("repeated initialize/tools-list routing must return promptly");
        assert!(
            matches!(
                claim,
                super::super::ProjectOpenTaskClaim::InFlight(_)
                    | super::super::ProjectOpenTaskClaim::Failed(_)
            ),
            "same route must reuse its one opening or cached failure"
        );
        assert!(
            tasks.tracked_task_count().await <= 1,
            "same route must never accumulate detached open tasks"
        );
    }

    let error = super::super::ProjectOpenTasks::wait_for_completion(first)
        .await
        .expect_err("the injected invariant rejection must surface");
    assert!(
        error
            .to_string()
            .contains(super::super::PROJECT_OPEN_FAILURE_RETRY_HINT),
        "cached route failure must carry the stable backoff marker: {error}"
    );
    let repeated_attempts = Arc::clone(&attempts);
    let repeated_error = match tokio::time::timeout(
        tokio::time::Duration::from_millis(50),
        tasks.start(route, async move {
            repeated_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }),
    )
    .await
    .expect("cached invariant failure must return promptly")
    {
        super::super::ProjectOpenTaskClaim::Failed(failure) => failure.to_error(),
        super::super::ProjectOpenTaskClaim::InFlight(_) => {
            panic!("cached invariant failure must not re-open the route")
        }
        super::super::ProjectOpenTaskClaim::Saturated => {
            panic!("cached invariant failure must not be replaced by global saturation")
        }
    };
    assert!(
        repeated_error
            .to_string()
            .contains(super::super::PROJECT_OPEN_FAILURE_RETRY_HINT),
        "repeated routing must return the typed backoff failure"
    );
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "repeated bootstrap requests must use one bounded open attempt"
    );
    assert_eq!(
        tasks.tracked_route_count().await,
        1,
        "the rejected route must retain one backoff entry"
    );

    let response = super::super::project_open_error_response(serde_json::json!(17), &error);
    let data = response
        .error
        .expect("typed route rejection")
        .data
        .expect("route rejection data");
    assert_eq!(
        data["kind"],
        serde_json::json!("project_route_open_backoff")
    );
    assert_eq!(data["retryable"], serde_json::json!(true));
}

#[tokio::test]
async fn project_open_task_registry_caps_distinct_inflight_routes() {
    let tasks = super::super::ProjectOpenTasks::default();
    for index in 0..super::super::MAX_TRACKED_PROJECT_OPEN_TASKS {
        let claim = tasks
            .start(
                project_open_test_route(&format!("bounded-{index}")),
                std::future::pending::<crate::errors::Result<()>>(),
            )
            .await;
        assert!(
            matches!(claim, super::super::ProjectOpenTaskClaim::InFlight(_)),
            "each route inside the configured bound must get one task"
        );
    }
    assert_eq!(
        tasks.tracked_task_count().await,
        super::super::MAX_TRACKED_PROJECT_OPEN_TASKS
    );

    let overflow = tasks
        .start(project_open_test_route("overflow"), async { Ok(()) })
        .await;
    assert!(
        matches!(overflow, super::super::ProjectOpenTaskClaim::Saturated),
        "a new route must not create an unbounded detached task"
    );
    let response = super::super::project_open_error_response(
        serde_json::Value::Null,
        &super::super::project_open_task_capacity_error(),
    );
    let data = response
        .error
        .expect("typed task capacity rejection")
        .data
        .expect("task capacity rejection data");
    assert_eq!(data["kind"], "project_open_task_capacity_reached");
    assert_eq!(data["retryable"], true);
    assert_eq!(
        data["capacity"],
        super::super::MAX_TRACKED_PROJECT_OPEN_TASKS
    );

    tasks.shutdown().await;
    assert_eq!(tasks.tracked_task_count().await, 0);
    assert_eq!(tasks.tracked_route_count().await, 0);
}

#[tokio::test]
async fn cached_project_open_failures_do_not_consume_inflight_capacity() {
    let tasks = super::super::ProjectOpenTasks::default();
    for index in 0..super::super::MAX_TRACKED_PROJECT_OPEN_TASKS {
        let state = match tasks
            .start(
                project_open_test_route(&format!("cached-failure-{index}")),
                async {
                    Err(authority_invariant_error(
                        "invalid committed observation authority JSON",
                    ))
                },
            )
            .await
        {
            super::super::ProjectOpenTaskClaim::InFlight(state) => state,
            super::super::ProjectOpenTaskClaim::Failed(_) => {
                panic!("first route attempt must start")
            }
            super::super::ProjectOpenTaskClaim::Saturated => {
                panic!("completed failures must not consume active-task capacity")
            }
        };
        super::super::ProjectOpenTasks::wait_for_completion(state)
            .await
            .expect_err("injected authority failure must surface");
    }

    let healthy = tasks
        .start(project_open_test_route("healthy-after-failures"), async {
            Ok(())
        })
        .await;
    let state = match healthy {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => {
            panic!("independent healthy route must not reuse a failure")
        }
        super::super::ProjectOpenTaskClaim::Saturated => {
            panic!("cached failures must not block an unrelated open")
        }
    };
    super::super::ProjectOpenTasks::wait_for_completion(state)
        .await
        .expect("independent healthy route must open");
}

#[tokio::test]
async fn project_open_failure_cache_is_bounded_separately() {
    let tasks = super::super::ProjectOpenTasks::default();
    for index in 0..(super::super::MAX_CACHED_PROJECT_OPEN_FAILURES + 8) {
        let state = match tasks
            .start(
                project_open_test_route(&format!("bounded-failure-{index}")),
                async {
                    Err(authority_invariant_error(
                        "invalid committed observation authority JSON",
                    ))
                },
            )
            .await
        {
            super::super::ProjectOpenTaskClaim::InFlight(state) => state,
            super::super::ProjectOpenTaskClaim::Failed(_) => {
                panic!("each distinct route must start once")
            }
            super::super::ProjectOpenTaskClaim::Saturated => {
                panic!("cached failures must not consume task capacity")
            }
        };
        super::super::ProjectOpenTasks::wait_for_completion(state)
            .await
            .expect_err("injected authority failure must surface");
    }
    assert!(
        tasks.tracked_route_count().await <= super::super::MAX_CACHED_PROJECT_OPEN_FAILURES,
        "failure cache must stay independently bounded"
    );
}

fn authority_invariant_error(message: &str) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        message: message.to_string(),
        operation: "ensure global database authority invariants".to_string(),
    }
}

#[test]
fn deterministic_code_authority_conflicts_do_not_spin_project_warmup() {
    let error = crate::errors::TraceDecayError::Database {
        message: "DuplicateCodeAuthority { shard_id: fixture }".to_string(),
        operation: "register code-shard authority".to_string(),
    };

    assert_eq!(
        super::super::project_open_retry_backoff(&error),
        Some(super::super::PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF)
    );
}

#[test]
fn exhausted_code_runtime_capacity_retries_at_resource_cadence() {
    let error = crate::errors::TraceDecayError::Database {
        message: "ProjectCodeBudgetExhausted { limit: 4 }".to_string(),
        operation: "open registered session runtime".to_string(),
    };

    assert_eq!(
        super::super::project_open_retry_backoff(&error),
        Some(super::super::PROJECT_OPEN_RESOURCE_RETRY_BACKOFF)
    );
    assert!(
        super::super::PROJECT_OPEN_RESOURCE_RETRY_BACKOFF
            > super::super::PROJECT_OPEN_FAILURE_RETRY_BACKOFF
    );
}

#[test]
fn undecodable_authority_row_backs_off_beyond_the_transient_debounce() {
    // The identity material of a committed observation stopped matching the
    // derivation the running binary applies, so every reopen re-runs the whole
    // authority audit and fails on the same row.
    let backoff = super::super::project_open_retry_backoff(&authority_invariant_error(
        "invalid committed observation authority JSON: serialized observation identity \
         does not match its source evidence",
    ));

    assert_eq!(
        backoff,
        Some(super::super::PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF),
        "an undecodable persisted row must not reopen at the transient debounce cadence"
    );
    assert!(
        super::super::PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF
            > super::super::PROJECT_OPEN_FAILURE_RETRY_BACKOFF
    );
}

#[test]
fn mutable_cursor_key_rejection_keeps_the_transient_debounce() {
    assert_eq!(
        super::super::project_open_retry_backoff(&authority_invariant_error(
            "session temporal receipts or cursor keys are mutable",
        )),
        Some(super::super::PROJECT_OPEN_FAILURE_RETRY_BACKOFF)
    );
}

/// Every authority verdict that is a property of the stored rows must back off,
/// including messages nobody has enumerated yet.
///
/// The column-versus-JSON disagreement below was unclassified and spun warm-up
/// at the debounce cadence, burning roughly three quarters of a core. It is a
/// deterministic judgement of persisted data, so it cannot self-clear, and
/// neither can the rest of this family.
#[test]
fn deterministic_authority_verdicts_all_back_off() {
    for message in [
        "committed observation authority columns disagree with observation JSON",
        "sanitization receipt authority columns disagree with receipt JSON",
        "summary publication receipt authority columns disagree with receipt JSON",
        "source cursor authority keys disagree with cursor JSON",
        "committed source cursor disagrees with observation source evidence",
        "committed observation references a missing receipt",
        "projection provenance disagrees with deterministic output",
        "an invariant message that does not exist yet",
    ] {
        assert_eq!(
            super::super::project_open_retry_backoff(&authority_invariant_error(message)),
            Some(super::super::PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF),
            "{message} cannot clear on its own and must not reopen at the debounce cadence"
        );
    }
}

#[test]
fn transient_authority_failures_stay_immediately_retryable() {
    // A locked or unreadable database surfaces under the same operation but
    // clears without operator repair, so it must not be backed off.
    assert_eq!(
        super::super::project_open_retry_backoff(&authority_invariant_error("database is locked",)),
        None
    );
    assert_eq!(
        super::super::project_open_retry_backoff(&crate::errors::TraceDecayError::Database {
            message: "invalid committed observation authority JSON: trailing characters"
                .to_string(),
            operation: "read observation".to_string(),
        }),
        None,
        "only the authority-invariant operation classifies these messages"
    );
}

#[tokio::test]
async fn route_open_backoff_retries_after_deadline_without_cross_route_blocking() {
    let tasks = super::super::ProjectOpenTasks::default();
    let rejected = project_open_test_route("rejected");
    let healthy = project_open_test_route("healthy");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let rejected_attempts = Arc::clone(&attempts);
    let rejected_state = match tasks
        .start(rejected.clone(), async move {
            rejected_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(crate::errors::TraceDecayError::Config {
                message: "identity cutover conflict: strict route invariant".to_string(),
            })
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => {
            panic!("first rejected route attempt must start")
        }
        super::super::ProjectOpenTaskClaim::Saturated => panic!("first rejected route must fit"),
    };
    super::super::ProjectOpenTasks::wait_for_completion(rejected_state)
        .await
        .expect_err("rejected route must fail");

    let healthy_attempts = Arc::clone(&attempts);
    let healthy_state = match tasks
        .start(healthy, async move {
            healthy_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => {
            panic!("a rejected route must not poison another route")
        }
        super::super::ProjectOpenTaskClaim::Saturated => {
            panic!("independent route must fit the bounded task registry")
        }
    };
    super::super::ProjectOpenTasks::wait_for_completion(healthy_state)
        .await
        .expect("independent route must open while another is backed off");

    tokio::time::sleep(
        super::super::PROJECT_OPEN_FAILURE_RETRY_BACKOFF + tokio::time::Duration::from_millis(25),
    )
    .await;
    let retry_attempts = Arc::clone(&attempts);
    let retry_state = match tasks
        .start(rejected, async move {
            retry_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => {
            panic!("backoff expiry must allow a new route attempt")
        }
        super::super::ProjectOpenTaskClaim::Saturated => {
            panic!("retry must fit after its completed failure is pruned")
        }
    };
    super::super::ProjectOpenTasks::wait_for_completion(retry_state)
        .await
        .expect("retry after route-specific backoff must open");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "one rejected route retry and one independent route must be the only opens"
    );
}

#[tokio::test]
async fn project_open_task_shutdown_cancels_and_clears_route_registry() {
    let tasks = super::super::ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown");
    let started = Arc::new(tokio::sync::Notify::new());
    let task_started = Arc::clone(&started);
    let state = match tasks
        .start_cancellable(route, move |cancellation| async move {
            task_started.notify_one();
            cancellation.cancelled().await;
            Err(crate::errors::TraceDecayError::Config {
                message: "project open cancelled".to_string(),
            })
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => panic!("pending task must start"),
        super::super::ProjectOpenTaskClaim::Saturated => panic!("pending task must fit"),
    };
    started.notified().await;
    assert_eq!(tasks.tracked_task_count().await, 1);

    tasks.shutdown().await;

    assert_eq!(tasks.tracked_task_count().await, 0);
    assert_eq!(tasks.tracked_route_count().await, 0);
    assert!(
        super::super::ProjectOpenTasks::wait_for_completion(state)
            .await
            .is_err(),
        "cancelled open tasks must wake waiters instead of retaining them"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_open_shutdown_waits_for_safe_unit_then_joins() {
    let tasks = super::super::ProjectOpenTasks::default();
    let route = project_open_test_route("cooperative-shutdown");
    let lifecycle = DaemonLifecycle::default();
    let store_administration = StoreAdministration::default();
    let (cancellation_tx, cancellation_rx) = tokio::sync::oneshot::channel();
    let (unit_started_tx, unit_started_rx) = tokio::sync::oneshot::channel();
    let (unit_release_tx, unit_release_rx) = tokio::sync::oneshot::channel();
    let (unit_finished_tx, unit_finished_rx) = tokio::sync::oneshot::channel();

    let task_lifecycle = lifecycle.clone();
    let task_administration = store_administration.clone();
    let state = match tasks
        .start_cancellable(route, move |cancellation| async move {
            let _activity = task_lifecycle
                .try_enter()
                .expect("project open lifecycle activity");
            let published_cancellation = cancellation.clone();
            task_administration
                .with_writer_until_cancelled(&cancellation, move || async move {
                    cancellation_tx
                        .send(published_cancellation)
                        .expect("publish project-open cancellation");
                    unit_started_tx.send(()).expect("publish safe unit start");
                    unit_release_rx.await.expect("release safe unit");
                    unit_finished_tx
                        .send(())
                        .expect("publish safe unit completion");
                })
                .await
                .expect("safe unit acquired writer administration");
            cancellation.cancelled().await;
            Err(crate::errors::TraceDecayError::Config {
                message: "project open cancelled after safe unit".to_string(),
            })
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(state) => state,
        super::super::ProjectOpenTaskClaim::Failed(_) => panic!("project open must start"),
        super::super::ProjectOpenTaskClaim::Saturated => panic!("project open must fit"),
    };
    let cancellation = cancellation_rx
        .await
        .expect("project-open cancellation token");
    unit_started_rx.await.expect("safe unit started");

    let shutdown_tasks = tasks.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_tasks.shutdown().await });
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        cancellation.cancelled(),
    )
    .await
    .expect("shutdown must request cooperative cancellation");
    assert!(
        !shutdown.is_finished(),
        "shutdown must not abort a transactionally safe unit in progress"
    );

    unit_release_tx.send(()).expect("release safe unit");
    unit_finished_rx.await.expect("safe unit completed");
    let cooperative = tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut shutdown)
        .await
        .expect("cooperative project-open shutdown timed out")
        .expect("project-open shutdown task");
    assert!(
        cooperative,
        "normal warm-up cancellation must not reach its timeout guard"
    );
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        lifecycle.wait_for_idle(),
    )
    .await
    .expect("client-drain lifecycle activity must be released");
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        store_administration.with_writer(|| async {}),
    )
    .await
    .expect("server shutdown must reacquire writer administration");
    assert_eq!(tasks.tracked_route_count().await, 0);
    super::super::ProjectOpenTasks::wait_for_completion(state)
        .await
        .expect_err("cancelled project open must report a terminal failure");
}

#[tokio::test]
async fn project_open_shutdown_backstop_aborts_and_joins_noncooperative_task() {
    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    let tasks = super::super::ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown-backstop");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    match tasks
        .start_cancellable(route, move |_| async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            started_tx.send(()).expect("publish task start");
            std::future::pending::<crate::errors::Result<()>>().await
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(_) => {}
        super::super::ProjectOpenTaskClaim::Failed(_) => panic!("pending task must start"),
        super::super::ProjectOpenTaskClaim::Saturated => panic!("pending task must fit"),
    }
    started_rx.await.expect("noncooperative task started");

    let cooperative = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        tasks.shutdown_with_deadline(
            tokio::time::Duration::ZERO,
            tokio::time::Duration::from_secs(1),
        ),
    )
    .await
    .expect("shutdown backstop must join the aborted task");

    assert!(!cooperative, "noncooperative task must reach the backstop");
    dropped_rx
        .await
        .expect("joined task must drop its owned resources before shutdown returns");
    assert_eq!(tasks.tracked_route_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_open_shutdown_detaches_synchronous_work_after_abort_deadline() {
    let tasks = super::super::ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown-synchronous-backstop");
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_started = Arc::clone(&started);
    let task_release = Arc::clone(&release);
    match tasks
        .start_cancellable(route, move |_| async move {
            task_started.store(true, std::sync::atomic::Ordering::Release);
            while !task_release.load(std::sync::atomic::Ordering::Acquire) {
                std::hint::spin_loop();
            }
            Ok(())
        })
        .await
    {
        super::super::ProjectOpenTaskClaim::InFlight(_) => {}
        super::super::ProjectOpenTaskClaim::Failed(_) => panic!("synchronous task must start"),
        super::super::ProjectOpenTaskClaim::Saturated => panic!("synchronous task must fit"),
    }
    while !started.load(std::sync::atomic::Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    let cooperative = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        tasks.shutdown_with_deadline(
            tokio::time::Duration::ZERO,
            tokio::time::Duration::from_millis(25),
        ),
    )
    .await
    .expect("shutdown must detach synchronous work after its abort deadline");

    assert!(!cooperative, "synchronous work must reach the backstop");
    assert_eq!(tasks.tracked_route_count().await, 0);
    release.store(true, std::sync::atomic::Ordering::Release);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portable_broker_bootstrap_bypasses_project_writer_gate() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);

    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "portable-bootstrap-cache-test")
            .expect("daemon database scope");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let owners = Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default()));
    let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("load test profile identity");
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners))
        .with_profile_identity(profile_identity);
    let gates = Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");

    let blocker_administration = store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        blocker_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    let server_administration = store_administration.clone();
    let server_gates = Arc::clone(&gates);
    let server_attempts = Arc::clone(&attempts);
    let server_lifecycle = lifecycle.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let stream = listener.accept().await.expect("accept client");
            let administration = server_administration.clone();
            let gates = Arc::clone(&server_gates);
            let attempts = Arc::clone(&server_attempts);
            let lifecycle = server_lifecycle.clone();
            clients.spawn(async move {
                Box::pin(super::super::serve_windows_broker_client(
                    stream,
                    TOKEN,
                    &lifecycle,
                    administration,
                    gates,
                    Some(attempts),
                ))
                .await
            });
        }
        while let Some(client) = clients.join_next().await {
            client.expect("client task").expect("serve client");
        }
    });

    let request = |id: u64, method: &'static str| {
        let endpoint = endpoint.clone();
        let handshake = handshake.clone();
        async move {
            let stream = super::super::transport::BrokerStream::connect(&endpoint)
                .await
                .expect("connect client");
            let (reader, mut writer) = stream.into_split();
            let preface = super::super::transport::DaemonAuthPreface::new(TOKEN)
                .to_line()
                .expect("auth preface");
            writer.write_all(preface.as_bytes()).await.expect("preface");
            writer.write_all(b"\n").await.expect("preface newline");
            writer
                .write_all(handshake.to_line().expect("handshake").as_bytes())
                .await
                .expect("handshake");
            writer.write_all(b"\n").await.expect("handshake newline");
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": (method == "initialize").then_some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "portable-bootstrap-test", "version": "1"}
                }))
            });
            writer
                .write_all(request.to_string().as_bytes())
                .await
                .expect("request");
            writer.write_all(b"\n").await.expect("request newline");
            writer.shutdown().await.expect("shutdown request writer");
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let response = lines
                .next_line()
                .await
                .expect("read response")
                .expect("response line");
            serde_json::from_str::<serde_json::Value>(&response).expect("response json")
        }
    };
    let mut initialize_task = tokio::spawn(request(1, "initialize"));
    let mut tools_list_task = tokio::spawn(request(2, "tools/list"));
    let (initialize_within_bound, tools_list_within_bound) = tokio::join!(
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut initialize_task),
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut tools_list_task),
    );

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if initialize_within_bound.is_err() {
        let _ = initialize_task.await;
    }
    if tools_list_within_bound.is_err() {
        let _ = tools_list_task.await;
    }
    server.await.expect("portable broker server");

    let initialize_response = initialize_within_bound
        .expect("portable initialize must not wait for project writer gate")
        .expect("initialize client task");
    assert_eq!(
        initialize_response["result"]["protocolVersion"],
        serde_json::json!("2024-11-05")
    );
    let tools_list_response = tools_list_within_bound
        .expect("portable tools/list must not wait for project writer gate")
        .expect("tools/list client task");
    assert!(
        tools_list_response["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "portable bootstrap tool catalog must not be empty"
    );
    let portable_context_description = tools_list_response["result"]["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool["name"] == serde_json::json!("tracedecay_context"))
        })
        .and_then(|tool| tool["description"].as_str())
        .expect("portable context tool description");
    assert!(portable_context_description.contains("3 calls maximum"));
    assert!(portable_context_description.contains("project graph is warming"));

    lifecycle.begin_draining();
    tokio::time::timeout(PHASE_TIMEOUT, lifecycle.wait_for_idle())
        .await
        .expect("portable warmup lifecycle drain timed out");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "portable initialize warmup must singleflight one project open"
    );
    super::super::shutdown_project_servers(&store_administration).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_warmup_drops_lifecycle_activity_on_draining() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::create_dir_all(&profile_root).expect("profile dir");
    let project = project.canonicalize().expect("canonical project");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "warmup drain enrolled route");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..test_handshake_defaults()
    };
    let initialize_request = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }))
    .expect("initialize request");

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

    Box::pin(engine.schedule_project_server_warmup(handshake, initialize_request))
        .await
        .expect("schedule project warmup");
    engine.lifecycle.begin_draining();
    engine.shutdown_project_open_tasks().await;
    let idle_while_writer_held = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.lifecycle.wait_for_idle(),
    )
    .await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if idle_while_writer_held.is_err() {
        tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            engine.lifecycle.wait_for_idle(),
        )
        .await
        .expect("warmup cleanup after writer release");
    }

    idle_while_writer_held.expect("draining must cancel project warmup before writer release");
    let tasks = super::super::project_open_tasks(&engine.project_open_gates).await;
    assert_eq!(
        tasks.tracked_task_count().await,
        0,
        "daemon shutdown must clear its tracked project-open task"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_activation_drain_wins_when_discovery_is_simultaneously_ready() {
    for _ in 0..32 {
        let lifecycle = DaemonLifecycle::default();
        let discovery_polled = Arc::new(tokio::sync::Notify::new());
        let discovery_polled_by_future = Arc::clone(&discovery_polled);
        let discovery_lifecycle = lifecycle.clone();
        let discovery_won = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let discovery_won_by_future = Arc::clone(&discovery_won);
        super::super::project_open_orchestration::spawn_lifecycle_automation_scheduler_activation(
            lifecycle.clone(),
            async move {
                discovery_polled_by_future.notify_one();
                discovery_lifecycle.wait_for_draining().await;
                discovery_won_by_future.store(true, std::sync::atomic::Ordering::Release);
            },
        );
        discovery_polled.notified().await;

        lifecycle.begin_draining();
        tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            lifecycle.wait_for_idle(),
        )
        .await
        .expect("simultaneous scheduler discovery drain timed out");
        assert!(
            !discovery_won.load(std::sync::atomic::Ordering::Acquire),
            "draining must win when scheduler discovery becomes ready on the same tick"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portable_project_warmup_rejects_after_shutdown_snapshot() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    prepare_test_profile_root(&profile_root);
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..test_handshake_defaults()
    };
    let initialize_request: crate::mcp::JsonRpcRequest =
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("initialize request");
    let owners = Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default()));
    let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("load test profile identity");
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners))
        .with_profile_identity(profile_identity);
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();

    lifecycle.begin_draining();
    let error = Box::pin(super::super::schedule_portable_project_server_warmup(
        lifecycle.clone(),
        store_administration,
        project_open_gates,
        super::super::DaemonInvocationState::default(),
        super::super::http_application::DaemonHttpApplicationRegistry::default(),
        handshake,
        initialize_request,
        Some(Arc::clone(&attempts)),
    ))
    .await
    .expect_err("draining must reject a new portable project warmup");
    assert!(error.to_string().contains("draining"), "{error}");
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        lifecycle.wait_for_idle(),
    )
    .await
    .expect("rejected portable warmup must not retain lifecycle activity");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "draining must reject a portable warmup before opening a project"
    );
    assert!(
        owners.lock().await.values().next().is_none(),
        "draining portable warmup must not insert a server after shutdown snapshot"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_warmup_settles_when_drain_is_simultaneously_ready() {
    let initialize_request: crate::mcp::JsonRpcRequest =
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("initialize request");

    for _ in 0..32 {
        let lifecycle = DaemonLifecycle::default();
        let open_polled = Arc::new(tokio::sync::Notify::new());
        let open_polled_by_future = Arc::clone(&open_polled);
        let open_lifecycle = lifecycle.clone();
        let open_won = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let open_won_by_future = Arc::clone(&open_won);
        let tasks = super::super::ProjectOpenTasks::default();
        let claim = Box::pin(
            super::super::project_open_orchestration::start_lifecycle_project_open(
                &tasks,
                lifecycle.clone(),
                project_open_test_route("simultaneous-drain"),
                std::path::PathBuf::from("/projects/simultaneous-drain"),
                Some(initialize_request.clone()),
                move |_| async move {
                    open_polled_by_future.notify_one();
                    open_lifecycle.wait_for_draining().await;
                    open_won_by_future.store(true, std::sync::atomic::Ordering::Release);
                    Err(crate::errors::TraceDecayError::Config {
                        message: "simultaneous warmup completion".to_string(),
                    })
                },
            ),
        )
        .await;
        let state = match claim {
            super::super::ProjectOpenTaskClaim::InFlight(state) => state,
            super::super::ProjectOpenTaskClaim::Failed(_) => {
                panic!("production warmup task must start before draining")
            }
            super::super::ProjectOpenTaskClaim::Saturated => {
                panic!("production warmup task must fit")
            }
        };
        open_polled.notified().await;

        lifecycle.begin_draining();
        tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            lifecycle.wait_for_idle(),
        )
        .await
        .expect("simultaneous warmup drain timed out");
        assert!(
            open_won.load(std::sync::atomic::Ordering::Acquire),
            "an admitted open must settle before draining releases its lifecycle activity"
        );
        super::super::ProjectOpenTasks::wait_for_completion(state)
            .await
            .expect_err("draining production warmup must report cancellation");
        tasks.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_bootstrap_catalog_bypasses_project_writer_gate() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let project = project.canonicalize().expect("canonical project");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "mcp-bootstrap-cache-test")
            .expect("daemon database scope");
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        allow_initialize_root_routing: true,
        ..test_handshake_defaults()
    };

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

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bootstrap-cache-test", "version": "1"},
            "roots": [{"uri": project, "name": "registered-project"}]
        }
    });
    let tools_list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let initialize_engine = engine.clone();
    let initialize_handshake = handshake.clone();
    let mut initialize_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(initialize_engine, &initialize_handshake, initialize)
            .await
    });
    let tools_list_engine = engine.clone();
    let tools_list_handshake = handshake.clone();
    let mut tools_list_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(tools_list_engine, &tools_list_handshake, tools_list)
            .await
    });
    let (initialize_within_bound, tools_list_within_bound) = tokio::join!(
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut initialize_task),
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut tools_list_task),
    );

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if initialize_within_bound.is_err() {
        let _ = initialize_task.await;
    }
    if tools_list_within_bound.is_err() {
        let _ = tools_list_task.await;
    }

    let initialize_responses = initialize_within_bound
        .expect("initialize must not wait for project writer gate")
        .expect("initialize client task");
    let initialize_response = initialize_responses
        .iter()
        .find(|response| response["id"] == json!(1))
        .expect("initialize response");
    assert_eq!(
        initialize_response["result"]["protocolVersion"],
        json!("2024-11-05")
    );
    assert_eq!(
        initialize_response["result"]["serverInfo"]["name"],
        json!("tracedecay")
    );
    assert_eq!(
        initialize_response["result"]["_meta"]["tracedecayInitializeRoute"],
        json!({
            "projectPath": handshake.project_path,
            "allowInit": false,
        })
    );

    let tools_list_responses = tools_list_within_bound
        .expect("tools/list must not wait for project writer gate")
        .expect("tools/list client task");
    let tools = tools_list_responses
        .iter()
        .find(|response| response["id"] == json!(2))
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list result catalog");
    assert!(
        !tools.is_empty(),
        "bootstrap tool catalog must not be empty"
    );
    let context_description = tools
        .iter()
        .find(|tool| tool["name"] == json!("tracedecay_context"))
        .and_then(|tool| tool["description"].as_str())
        .expect("context tool description");
    assert!(context_description.contains("3 calls maximum"));
    assert!(context_description.contains("project graph is warming"));

    tokio::time::timeout(PHASE_TIMEOUT, async {
        while engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initialize warmup did not start after the writer gate was released");
    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("bootstrap-cache shutdown timed out");
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "initialize warmup must singleflight one project open"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_tool_cache_miss_returns_warming_while_project_opens_in_background() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let project = project.canonicalize().expect("canonical project");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "direct-warmup-test")
            .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

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

    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_status",
            "arguments": {"format": "json"}
        }
    });
    let request_engine = engine.clone();
    let request_handshake = handshake.clone();
    let mut request_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(request_engine, &request_handshake, request).await
    });
    let response_within_bound =
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut request_task).await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if response_within_bound.is_err() {
        let _ = request_task.await;
    }

    let responses = response_within_bound
        .expect("direct tool cache miss must return a bounded warming response")
        .expect("direct tool client task");
    let response = responses
        .iter()
        .find(|response| response["id"] == json!(3))
        .expect("direct tool response");
    let message = response["error"]["message"]
        .as_str()
        .expect("warming error message");
    assert!(message.contains("warming in the background"), "{message}");
    assert!(message.contains("retry"), "{message}");

    let route =
        super::super::ProjectRouteKey::from_handshake(&project, &handshake).expect("project route");
    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            let warmed = engine
                .store_administration
                .project_servers()
                .lock()
                .await
                .get_route(&route)
                .is_some();
            if warmed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached project warmup timed out");
    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("direct warmup shutdown timed out");
}

#[tokio::test(start_paused = true)]
async fn foreground_project_open_wait_is_bounded_and_accepts_quick_publication() {
    let project_path = std::path::PathBuf::from("/projects/uncontended");
    let published = super::super::project_open_orchestration::wait_for_project_open_publication(
        &project_path,
        async { Ok::<(), crate::errors::TraceDecayError>(()) },
    )
    .await;
    assert!(
        published.is_ok(),
        "publication inside the deadline must succeed"
    );

    let warming = super::super::project_open_orchestration::wait_for_project_open_publication(
        &project_path,
        std::future::pending::<crate::errors::Result<()>>(),
    )
    .await
    .expect_err("an uncontended warm-up must not pin the foreground request");
    assert!(
        warming.to_string().contains("warming in the background"),
        "{warming}"
    );
}

fn production_composition_tool_text(response: &JsonRpcResponse) -> &str {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool returned an error: {result}");
    result["content"][0]["text"].as_str().expect("tool text")
}

fn commit_production_composition_project(project: &std::path::Path) {
    let run_git = |arguments: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(project)
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    };
    run_git(&["init", "--quiet"]);
    run_git(&["add", "."]);
    run_git(&[
        "-c",
        "user.name=TraceDecay Test",
        "-c",
        "user.email=tracedecay@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "test: seed production composition",
    ]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_harness_wires_query_search_authority() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn production_composition_probe() -> bool { true }\n\
         pub fn production_composition_caller() -> bool { production_composition_probe() }\n",
    )
    .expect("source file");
    commit_production_composition_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");
    let payload = tokio::time::timeout(tokio::time::Duration::from_secs(20), async {
        loop {
            let response = harness
                .call_tool(
                    &project,
                    "tracedecay_search",
                    json!({
                        "query": "production_composition_probe",
                        "limit": 10,
                        "format": "json"
                    }),
                )
                .await
                .expect("production search");
            let payload: serde_json::Value =
                serde_json::from_str(production_composition_tool_text(&response))
                    .expect("search json");
            if payload["code_generation"].as_str().is_some() {
                break payload;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production query authority did not become ready");
    let candidate = payload["results"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find(|candidate| {
                candidate["display"]["name"] == json!("production_composition_probe")
            })
        })
        .unwrap_or_else(|| {
            panic!("production query search authority did not return the indexed symbol: {payload}")
        });
    let node_id = candidate["node_id"]
        .as_str()
        .unwrap_or_else(|| panic!("search result must address graph follow-up tools: {candidate}"));
    let impact = harness
        .call_tool(
            &project,
            "tracedecay_impact",
            json!({"node_id": node_id, "format": "json"}),
        )
        .await
        .expect("production impact");
    let impact_payload: serde_json::Value =
        serde_json::from_str(production_composition_tool_text(&impact)).expect("impact json");
    assert!(
        impact_payload["node_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "impact must consume the graph identity returned by production search: {impact_payload}"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_harness_wires_cross_project_resolver() {
    let temp = TempDir::new().expect("temp dir");
    let first_project = temp.path().join("first");
    let second_project = temp.path().join("second");
    for project in [&first_project, &second_project] {
        std::fs::create_dir_all(project.join("src")).expect("source dir");
    }
    std::fs::write(
        first_project.join("src/lib.rs"),
        "pub fn first_project_probe() {}\n",
    )
    .expect("first source file");
    std::fs::write(
        second_project.join("src/lib.rs"),
        "pub fn second_project_probe() {}\n",
    )
    .expect("second source file");
    for project in [&first_project, &second_project] {
        commit_production_composition_project(project);
    }

    let harness = ProductionProjectCompositionHarnessV1::open(
        temp.path(),
        vec![first_project.clone(), second_project.clone()],
    )
    .await
    .expect("production composition");
    let canonical_second = second_project
        .canonicalize()
        .expect("canonical second project");
    let response = harness
        .call_tool(
            &first_project,
            "tracedecay_grep",
            json!({
                "pattern": "second_project_probe",
                "project_path": canonical_second,
                "format": "json"
            }),
        )
        .await
        .expect("cross-project grep");
    let payload = production_composition_tool_text(&response);
    assert!(
        payload.contains("second_project_probe") && !payload.contains("first_project_probe"),
        "production retained-project resolver must route search to the mounted project: {payload}"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_harness_dispatches_application_invocations_in_process() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(project.join("src/lib.rs"), "pub fn storage_probe() {}\n").expect("source file");
    commit_production_composition_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");
    assert!(
        !harness.semantic_auto_download_enabled(),
        "the effective semantic startup policy used by production composition must disable auto-download"
    );
    let response = harness
        .call_tool(
            &project,
            "tracedecay_storage_status",
            serde_json::json!({"format": "json"}),
        )
        .await
        .expect("in-process application invocation");
    let payload: serde_json::Value =
        serde_json::from_str(production_composition_tool_text(&response))
            .expect("storage-status application envelope");
    assert!(
        payload["contract"]["schema_id"]
            == serde_json::json!("schema.application.primitive.storage-status.result"),
        "production invocation must preserve the application result contract: {payload}"
    );
    assert!(
        payload["problem"].is_null() && !payload["outcome"].is_null(),
        "production composition must dispatch through retained daemon state: {payload}"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_dashboard_persists_project_settings_over_http() {
    let _dashboard_guard = PRODUCTION_DASHBOARD_TEST_LOCK.lock().await;
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn dashboard_settings_probe() {}\n",
    )
    .expect("source file");
    commit_production_composition_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");
    let dashboard = harness
        .call_tool(
            &project,
            "tracedecay_dashboard",
            json!({
                "action": "start",
                "host": "127.0.0.1",
                "port": 0,
                "format": "json"
            }),
        )
        .await
        .expect("start production dashboard");
    let dashboard_payload: serde_json::Value =
        serde_json::from_str(production_composition_tool_text(&dashboard))
            .expect("dashboard start payload");
    let base_url = dashboard_payload["url"]
        .as_str()
        .expect("dashboard URL")
        .trim_end_matches('/')
        .to_owned();

    tokio::task::spawn_blocking(move || {
        fn response_json(
            mut response: ureq::http::Response<ureq::Body>,
        ) -> (u16, serde_json::Value) {
            let status = response.status().as_u16();
            let body = response
                .body_mut()
                .read_to_string()
                .expect("read dashboard response");
            let payload = serde_json::from_str(&body)
                .unwrap_or_else(|error| panic!("decode dashboard response `{body}`: {error}"));
            (status, payload)
        }

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(4)))
            .build()
            .into();
        let settings_url = format!("{base_url}/api/settings");
        let project_settings_url = format!("{settings_url}/project");

        let (status, initial_envelope) = response_json(
            agent
                .get(&settings_url)
                .call()
                .expect("GET project settings"),
        );
        assert_eq!(status, 200, "GET settings failed: {initial_envelope}");
        let initial = &initial_envelope["payload"];
        let initial_revision = initial["project"]["configuration_revision_id"]
            .as_str()
            .expect("initial configuration revision")
            .to_owned();
        let initial_snapshot = initial["project"]["configuration_snapshot_id"]
            .as_str()
            .expect("initial configuration snapshot")
            .to_owned();

        let (status, patched_envelope) = response_json(
            agent
                .patch(&project_settings_url)
                .send_json(json!({
                    "expected_revision_id": initial_revision,
                    "max_file_size": 2048
                }))
                .expect("PATCH project settings"),
        );
        assert_eq!(
            status, 200,
            "production dashboard settings patch failed: {patched_envelope}"
        );
        let patched = &patched_envelope["payload"];
        let patched_revision = patched["project"]["configuration_revision_id"]
            .as_str()
            .expect("patched configuration revision")
            .to_owned();
        assert_ne!(
            patched_revision, initial_revision,
            "configuration mutation must advance its content-addressed revision"
        );
        assert_ne!(
            patched["project"]["configuration_snapshot_id"], initial_snapshot,
            "response must carry the daemon's updated configuration snapshot"
        );
        assert_eq!(patched["project"]["config"]["max_file_size"], 2048);
        assert_eq!(
            patched["resync_recommended"], true,
            "index-affecting settings must truthfully recommend resync"
        );

        let (status, persisted_envelope) = response_json(
            agent
                .get(&settings_url)
                .call()
                .expect("GET persisted settings"),
        );
        assert_eq!(
            status, 200,
            "persisted settings GET failed: {persisted_envelope}"
        );
        let persisted = &persisted_envelope["payload"];
        assert_eq!(
            persisted["project"]["configuration_revision_id"],
            patched_revision
        );
        assert_eq!(persisted["project"]["config"]["max_file_size"], 2048);
        assert_eq!(persisted["project"]["config"]["track_call_sites"], true);

        let (status, stale) = response_json(
            agent
                .patch(&project_settings_url)
                .send_json(json!({
                    "expected_revision_id": initial_revision,
                    "track_call_sites": false
                }))
                .expect("PATCH stale project settings"),
        );
        assert_eq!(status, 409, "stale project settings must conflict: {stale}");

        let (status, unchanged_envelope) = response_json(
            agent
                .get(&settings_url)
                .call()
                .expect("GET unchanged settings"),
        );
        assert_eq!(
            status, 200,
            "unchanged settings GET failed: {unchanged_envelope}"
        );
        let unchanged = &unchanged_envelope["payload"];
        assert_eq!(
            unchanged["project"]["configuration_revision_id"],
            patched_revision
        );
        assert_eq!(unchanged["project"]["config"]["max_file_size"], 2048);
        assert_eq!(unchanged["project"]["config"]["track_call_sites"], true);
    })
    .await
    .expect("dashboard HTTP assertions");

    harness
        .call_tool(
            &project,
            "tracedecay_dashboard",
            json!({"action": "stop", "format": "json"}),
        )
        .await
        .expect("stop production dashboard");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_harness_reads_retained_profile_analytics_authority() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(project.join("src/lib.rs"), "pub fn analytics_probe() {}\n")
        .expect("source file");
    commit_production_composition_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");
    harness
        .call_tool(
            &project,
            "tracedecay_search",
            json!({"query": "analytics_probe", "format": "json"}),
        )
        .await
        .expect("production search");
    harness
        .server(&project)
        .expect("production server")
        .ledger_writes_settled()
        .await;

    let second_owner = crate::application::host_admission::HostAdmissionTestRuntimeV1::profile(
        harness.profile_root(),
    )
    .await;
    let error = match second_owner {
        Ok(_) => panic!("parallel profile authority must remain rejected"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("different daemon election already owns database scope"),
        "parallel authority must fail at daemon election: {error}"
    );

    let events = harness
        .read_profile_analytics_events(&crate::global_db::AnalyticsEventQuery {
            provider: Some("mcp".to_owned()),
            project_id: None,
            session_id: None,
            event_kind: Some("mcp_tool_call".to_owned()),
            since: None,
            until: None,
            before_id: None,
            limit: 100,
        })
        .await
        .expect("read retained profile analytics");
    assert!(
        events
            .iter()
            .any(|event| event.tool_name.as_deref() == Some("tracedecay_search")),
        "retained production authority must expose the exercised tool event: {events:?}"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_composition_harness_shutdown_allows_immediate_profile_reopen() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(project.join("src/lib.rs"), "pub fn reopen_probe() {}\n").expect("source file");
    commit_production_composition_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");
    let profile_root = harness.profile_root().to_path_buf();
    harness.shutdown().await;

    let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("reload isolated profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 100, "production-composition-reopen")
            .expect("fresh daemon election");
    let registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            profile_identity,
        )
        .await
        .expect("immediately reopen profile runtime");
    registry
        .profile_database()
        .await
        .expect("immediately reopen profile authority database");
    registry
        .profile_sessions()
        .await
        .expect("immediately reopen profile session database");
    let project_id = crate::storage::read_repository_identity_marker(&project)
        .expect("read project identity")
        .expect("project identity marker");
    registry
        .project_sessions(
            tracedecay_store::ProjectId::new(project_id.project_id)
                .expect("valid project identity"),
            [project],
        )
        .await
        .expect("immediately reopen project session database");
}

#[tokio::test]
async fn production_composition_harness_rejects_live_profile_overlap() {
    let temp = TempDir::new().expect("temp dir");
    let live_profile = temp.path().join("live-profile");
    let overlapping_isolation = live_profile.join("test-isolation");
    std::fs::create_dir_all(&live_profile).expect("live profile");

    let error = match ProductionProjectCompositionHarnessV1::open_with_live_profile_root_for_test(
        &overlapping_isolation,
        Vec::new(),
        live_profile.clone(),
    )
    .await
    {
        Ok(_) => panic!("live-profile overlap must be rejected before any project or store opens"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("overlaps live profile")
            && message.contains(&live_profile.display().to_string()),
        "overlap rejection must identify the protected live profile: {message}"
    );
    assert!(
        !overlapping_isolation.join("profile").exists(),
        "rejection must fire before the harness creates an isolated profile"
    );
}
