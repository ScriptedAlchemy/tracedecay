#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, CreateWorkCommand, Deadline, MultiRootExecuteRequestV1,
    MultiRootOperationV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasStatusV1,
    MultiRootScopeSetReadRequestV1, RegisteredRootSelectorV1,
};
use tracedecay_domain::{ScopeSetId, TaskId, UtcMicros, WorkCommandId};

use super::{
    enter_test_daemon_database_scope, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};
use crate::daemon::service::invocation::{
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, WorkApplicationInvocationV1, parse_daemon_invocation_request,
};
use crate::daemon::{
    DaemonHandshake, execute_daemon_invocation, execute_portable_daemon_invocation,
};

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("run Git fixture command");
    assert!(status.success(), "git {args:?}");
}

fn repository() -> TempDir {
    let repository = TempDir::new().expect("repository");
    git(repository.path(), &["init", "--quiet"]);
    git(
        repository.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    git(
        repository.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    std::fs::write(
        repository.path().join("lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "base"]);
    repository
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn controls(suffix: &str, observed_at: UtcMicros) -> (Deadline, CancellationContext) {
    (
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline"),
        CancellationContext::active(format!("cancel.multi-root.{suffix}")).expect("cancellation"),
    )
}

fn wire_round_trip(request: &DaemonInvocationRequest) -> DaemonInvocationRequest {
    let wire = serde_json::to_string(request).expect("daemon invocation wire");
    parse_daemon_invocation_request(&wire)
        .expect("daemon invocation protocol")
        .expect("valid daemon invocation envelope")
}

async fn message_search(server: &crate::mcp::McpServer, arguments: Value) -> Value {
    let request = crate::mcp::JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "tracedecay_message_search",
            "arguments": arguments,
        })),
    };
    let response = server
        .handle_request(&request)
        .await
        .expect("message-search response");
    let result = response.result.expect("successful message-search response");
    result["content"]
        .as_array()
        .expect("message-search content")
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("message-search JSON content: {result}"))
}

#[cfg(unix)]
#[test]
fn authenticated_multi_root_journey_reaches_scope_set_storage() {
    const STACK_SIZE: usize = 16 * 1024 * 1024;

    std::thread::Builder::new()
        .name("multi-root-journey".to_owned())
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .expect("multi-root journey runtime")
                .block_on(run_authenticated_multi_root_journey());
        })
        .expect("multi-root journey thread")
        .join()
        .expect("multi-root journey thread must not panic");
}

#[cfg(unix)]
async fn run_authenticated_multi_root_journey() {
    let home = TempDir::new().expect("home");
    let profile_root = home.path().join("profile");
    let first = repository();
    let second = home.path().join("linked-worktree");
    let third = repository();
    let second_text = second.to_string_lossy().to_string();
    git(
        first.path(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked-worktree",
            &second_text,
        ],
    );
    let first_handshake = DaemonHandshake {
        project_path: Some(first.path().to_path_buf()),
        allow_init: true,
        client_identity: test_client_identity_for(profile_root.clone()),
        ..test_handshake_defaults()
    };
    let second_handshake = DaemonHandshake {
        project_path: Some(second.clone()),
        allow_init: true,
        client_identity: first_handshake.client_identity.clone(),
        ..test_handshake_defaults()
    };
    let third_handshake = DaemonHandshake {
        project_path: Some(third.path().to_path_buf()),
        allow_init: true,
        client_identity: first_handshake.client_identity.clone(),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&profile_root);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "multi-root-journey");
    let scope_set_id = ScopeSetId::new("scope-set.daemon-journey").expect("scope set id");

    // A malformed multi-root payload is still rejected on its own terms, and
    // still before the daemon spends a project admission on it.
    let observed_at = now();
    let (deadline, cancellation) = controls("invalid-read", observed_at);
    let mut invalid_read = DaemonInvocationRequest::multi_root_scope_set_read(
        "request.multi-root.invalid-read",
        MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
        observed_at,
        deadline,
        cancellation,
    );
    let DaemonInvocationPayload::MultiRootScopeSetRead {
        observed_at: invalid_observed_at,
        ..
    } = &mut invalid_read.payload
    else {
        unreachable!("constructed read payload")
    };
    *invalid_observed_at = UtcMicros(0);
    let invalid_response =
        execute_daemon_invocation(&engine, &first_handshake, wire_round_trip(&invalid_read)).await;
    assert!(matches!(
        invalid_response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::InvalidRequest
        }
    ));
    let portable_invalid = execute_portable_daemon_invocation(
        engine.lifecycle.clone(),
        engine.store_administration.clone(),
        Arc::clone(&engine.project_open_gates),
        &first_handshake,
        &engine.invocation,
        engine.http_application_registry.clone(),
        wire_round_trip(&invalid_read),
        Some(Arc::clone(&engine.project_open_attempts)),
    )
    .await;
    assert!(matches!(
        portable_invalid.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::InvalidRequest
        }
    ));
    assert_eq!(
        engine.project_open_attempts.load(Ordering::Relaxed),
        0,
        "an invalid multi-root payload must be rejected before project admission"
    );

    let (first_key, _, first_server, _) = engine
        .open_project_server(&first_handshake)
        .await
        .expect("first owner");
    let first_project = tracedecay_domain::ProjectId::new(
        first_key
            .owner
            .project_id
            .clone()
            .expect("first project id"),
    )
    .expect("first project");
    let registry = engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("registered profile database");
    let owner = registry
        .project_registry_context_by_id(first_project.as_str())
        .await
        .expect("registered project lookup")
        .expect("first project context");
    let fallback = super::super::graph_resolution::retained_project_graph_resolver(
        engine.store_administration.clone(),
    )(
        crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
            owner,
            second.clone(),
        ),
    )
    .await;
    let Err(crate::errors::TraceDecayError::ProjectRoute {
        reason_code,
        retryable,
        ..
    }) = fallback
    else {
        panic!("an unmounted linked worktree must not fall back to its primary graph");
    };
    assert_eq!(reason_code, "project_route_unavailable");
    assert!(retryable);
    let (second_key, _, second_server, _) = engine
        .open_project_server(&second_handshake)
        .await
        .expect("linked worktree owner");
    let second_project = tracedecay_domain::ProjectId::new(
        second_key
            .owner
            .project_id
            .clone()
            .expect("second project id"),
    )
    .expect("second project");
    assert_eq!(first_project, second_project);
    let selected_root = second.canonicalize().expect("linked canonical root");
    let selected = message_search(
        first_server.as_ref(),
        json!({
            "query": "linked worktree routing evidence",
            "format": "json",
            "project_path": selected_root,
        }),
    )
    .await;
    assert_eq!(
        selected["selected_project_root"],
        json!(selected_root),
        "the active server must route a path selector to the exact mounted linked worktree: {selected}"
    );
    let linked = message_search(
        second_server.as_ref(),
        json!({
            "query": "linked worktree routing evidence",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(linked["selected_project_root"], json!(selected_root));
    let (third_key, _, _, _) = engine
        .open_project_server(&third_handshake)
        .await
        .expect("distinct project owner");
    let third_project = tracedecay_domain::ProjectId::new(
        third_key
            .owner
            .project_id
            .clone()
            .expect("third project id"),
    )
    .expect("third project");
    assert_ne!(first_project, third_project);
    let first_uri = url::Url::from_file_path(first.path())
        .expect("first URI")
        .to_string();
    let second_uri = url::Url::from_file_path(&second)
        .expect("second URI")
        .to_string();
    // A single folder that is not the active project is still refused: a lone
    // sibling hint must not reroute the session.
    let (deadline, cancellation) = controls("sibling-root-lsp", now());
    let sibling_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.sibling-root.lsp",
            "client.sibling-root",
            Some(second_uri.clone()),
            vec![second_uri.clone()],
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        sibling_root_lsp.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    assert_eq!(
        engine.invocation.service.active_lsp_runtime_count().await,
        0,
        "a sibling single-folder hint must not mount a runtime"
    );

    let (deadline, cancellation) = controls("single-root-lsp", now());
    let single_root_lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.single-root.lsp",
            "client.single-root",
            Some(first_uri.clone()),
            vec![first_uri.clone()],
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(
        matches!(
            single_root_lsp.outcome,
            DaemonInvocationOutcome::LspOpened {
                scope_set_id: None,
                scope_set_digest: None,
                ..
            }
        ),
        "{:#?}",
        single_root_lsp.outcome
    );
    assert_eq!(
        engine
            .invocation
            .lsp_session_registry
            .lock()
            .await
            .active_sessions(),
        1,
        "single-root initialize must keep the existing runtime path working"
    );

    // A multi-folder initialize now admits a federated workspace and reports
    // the authorized scope set it was bound to.
    let (deadline, cancellation) = controls("multi-root-lsp", now());
    let lsp = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::lsp_open(
            "request.multi-root.lsp",
            "client.multi-root",
            Some(first_uri.clone()),
            vec![second_uri.clone(), first_uri.clone()],
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::LspOpened {
        scope_set_id: lsp_scope_set_id,
        scope_set_digest: lsp_scope_set_digest,
        ..
    } = &lsp.outcome
    else {
        panic!(
            "multi-folder initialize must open a session: {:?}",
            lsp.outcome
        );
    };
    let lsp_scope_set_id = lsp_scope_set_id
        .clone()
        .expect("federated initialize must report its scope set id");
    assert!(
        lsp_scope_set_digest.is_some(),
        "federated initialize must report its scope set digest"
    );
    let read_observed_at = now();
    let (read_deadline, read_cancellation) = controls("lsp-scope-set-read", read_observed_at);
    for root in [first.path(), second.as_path()] {
        assert!(
            engine
                .invocation
                .service
                .persisted_scope_set(
                    root,
                    None,
                    &lsp_scope_set_id,
                    tracedecay_application::MultiRootApplicationOperation::ScopeSetRead,
                    read_observed_at,
                    &read_deadline,
                    &read_cancellation,
                )
                .await
                .is_some(),
            "federated admission must persist the scope set in every participating store"
        );
    }

    // Compare-and-swap is the authorization boundary for an explicit scope set.
    let observed_at = now();
    let (deadline, cancellation) = controls("cas", observed_at);
    let cas = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![
                    RegisteredRootSelectorV1::new(second_project.clone(), &second)
                        .expect("linked registered root"),
                    RegisteredRootSelectorV1::new(third_project, third.path())
                        .expect("distinct registered root"),
                ],
            )
            .expect("CAS request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { outcome, .. } = &cas.outcome
    else {
        panic!("multi-root CAS must reach the executor: {:?}", cas.outcome);
    };
    let tracedecay_application::ApplicationOutcome::Evidence(packet) = outcome else {
        panic!("multi-root CAS must return evidence");
    };
    let cas_result = packet
        .payload
        .clone()
        .expect("multi-root CAS evidence must carry a result");
    assert!(matches!(
        cas_result.status,
        MultiRootScopeSetCasStatusV1::Applied
    ));
    let stored = cas_result
        .scope_set
        .expect("applied CAS must return the scope set");
    let canonical_scope_sets = registry
        .authorized_scope_set_storage()
        .expect("canonical scope-set storage");
    let read_observed_at = now();
    let (read_deadline, read_cancellation) = controls("cas-scope-set-read", read_observed_at);
    for root in [first.path(), second.as_path(), third.path()] {
        assert_eq!(
            engine
                .invocation
                .service
                .persisted_scope_set(
                    root,
                    Some(&canonical_scope_sets),
                    &scope_set_id,
                    tracedecay_application::MultiRootApplicationOperation::ScopeSetRead,
                    read_observed_at,
                    &read_deadline,
                    &read_cancellation,
                )
                .await
                .as_ref(),
            Some(&stored),
            "an applied CAS must be durable in the canonical profile store"
        );
    }
    let mut stored_locators = stored
        .roots()
        .iter()
        .map(|root| root.locator().map(|locator| locator.canonical_root.clone()))
        .collect::<Option<Vec<_>>>()
        .expect("CAS roots must retain exact registered locators");
    stored_locators.sort();
    let mut expected_locators = vec![
        second.canonicalize().expect("linked canonical root"),
        third.path().canonicalize().expect("third canonical root"),
    ];
    expected_locators.sort();
    assert_eq!(stored_locators, expected_locators);

    // The read surface returns exactly what the CAS persisted.
    let observed_at = now();
    let (deadline, cancellation) = controls("read", observed_at);
    let read = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_scope_set_read(
            "request.multi-root.read",
            MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootScopeSetRead { outcome, .. } = &read.outcome else {
        panic!(
            "multi-root read must reach the executor: {:?}",
            read.outcome
        );
    };
    let tracedecay_application::ApplicationOutcome::Evidence(packet) = outcome else {
        panic!("multi-root read must return evidence");
    };
    assert_eq!(packet.payload.clone().flatten().as_ref(), Some(&stored));

    // A stale revision or digest is refused by the executor, not by a gate.
    let observed_at = now();
    let (deadline, cancellation) = controls("stale-execute", observed_at);
    let stale = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.stale-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("digest"),
                MultiRootOperationV1::Work {
                    request: json!({
                        "operation": "snapshot",
                        "request": { "page_size": 1 }
                    }),
                },
                0,
                None,
            )
            .expect("execute request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        stale.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));

    // Cancellation is observed before any root-local read begins.
    let observed_at = now();
    let cancelled = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.cancelled-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                stored.digest().clone(),
                MultiRootOperationV1::Query { request: json!({}) },
                0,
                None,
            )
            .expect("cancelled execute request"),
            observed_at,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000)))
                .expect("cancelled execute deadline"),
            CancellationContext::cancelled("cancel.multi-root.execute", observed_at)
                .expect("cancelled execute cancellation"),
        ),
    )
    .await;
    assert!(matches!(
        cancelled.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: tracedecay_application::ApplicationProblem::Cancelled { .. }
        }
    ));

    // Every operation family fans out over the authorized scope set.
    for (index, operation) in [
        MultiRootOperationV1::Work { request: json!({}) },
        MultiRootOperationV1::Git { request: json!({}) },
        MultiRootOperationV1::Feedback { request: json!({}) },
        MultiRootOperationV1::Impact { request: json!({}) },
        MultiRootOperationV1::Query { request: json!({}) },
    ]
    .into_iter()
    .enumerate()
    {
        let observed_at = now();
        let (deadline, cancellation) = controls(&format!("execute-{index}"), observed_at);
        let response = execute_daemon_invocation(
            &engine,
            &first_handshake,
            DaemonInvocationRequest::multi_root_execute(
                format!("request.multi-root.execute-{index}"),
                MultiRootExecuteRequestV1::new(
                    scope_set_id.clone(),
                    stored.revision(),
                    stored.digest().clone(),
                    operation,
                    0,
                    None,
                )
                .expect("execute request"),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        assert!(
            matches!(
                response.outcome,
                DaemonInvocationOutcome::MultiRootQueryPage { .. }
            ),
            "execute-{index} must reach the multi-root executor: {:?}",
            response.outcome
        );
    }

    // Seed enough real Work state in every root to require a second page.
    for (root_ordinal, handshake) in [&first_handshake, &second_handshake, &third_handshake]
        .into_iter()
        .enumerate()
    {
        for task_ordinal in 0..2 {
            let observed_at = now();
            let (deadline, cancellation) = controls(
                &format!("seed-work-{root_ordinal}-{task_ordinal}"),
                observed_at,
            );
            let response = execute_daemon_invocation(
                &engine,
                handshake,
                DaemonInvocationRequest::work_application(
                    format!("request.multi-root.seed-work-{root_ordinal}-{task_ordinal}"),
                    WorkApplicationInvocationV1::Create(CreateWorkCommand {
                        task_id: TaskId::new(format!(
                            "task.multi-root.{root_ordinal}.{task_ordinal}"
                        ))
                        .expect("task id"),
                        title: format!("Multi-root task {root_ordinal}.{task_ordinal}"),
                        dependencies: std::collections::BTreeSet::new(),
                        command_id: WorkCommandId::new(format!(
                            "command.multi-root.{root_ordinal}.{task_ordinal}"
                        ))
                        .expect("command id"),
                        occurred_at: observed_at,
                    }),
                    observed_at,
                    deadline,
                    cancellation,
                ),
            )
            .await;
            assert!(
                matches!(
                    response.outcome,
                    DaemonInvocationOutcome::WorkApplication { .. }
                ),
                "Work seed must use the production root runtime: {:?}",
                response.outcome
            );
        }
    }

    // A real result page resumes only through the durable authenticated cursor.
    let resumable_operation = MultiRootOperationV1::Work {
        request: json!({
            "operation": "snapshot",
            "request": { "page_size": 1 }
        }),
    };
    let observed_at = now();
    let (deadline, cancellation) = controls("resumable-execute", observed_at);
    let first_page = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.resumable-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                stored.digest().clone(),
                resumable_operation.clone(),
                0,
                None,
            )
            .expect("resumable execute request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome:
            tracedecay_application::ApplicationOutcome::Evidence(
                tracedecay_application::EvidencePacket {
                    payload: Some(first_page),
                    ..
                },
            ),
        ..
    } = first_page.outcome
    else {
        panic!("real multi-root page must carry resumable evidence");
    };
    let continuation = first_page
        .continuation
        .clone()
        .expect("first page must carry a continuation");
    let cursor_authenticator = registry
        .load_session_cursor_key_provider_result()
        .await
        .expect("durable multi-root cursor authority");
    let continuation_state = super::super::multi_root_continuation::open(
        &continuation,
        &cursor_authenticator,
        observed_at,
    )
    .expect("authenticated multi-root continuation");
    assert_eq!(continuation_state.next_page, 1);
    assert_eq!(
        continuation_state.root_generations.len(),
        stored.roots().len()
    );
    assert!(continuation_state.root_cursors.iter().all(|root| {
        matches!(
            root.cursor.as_ref(),
            Some(tracedecay_application::MultiRootRootContinuationV1::Work(_))
        )
    }));
    assert!(
        continuation_state.last_order_key.is_some(),
        "a page with emitted evidence must freeze its total-order key"
    );

    let resumed_at = now();
    let (deadline, cancellation) = controls("resume-execute", resumed_at);
    let resumed = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.resume-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                stored.digest().clone(),
                resumable_operation.clone(),
                1,
                Some(continuation.clone()),
            )
            .expect("resume execute request"),
            resumed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome:
            tracedecay_application::ApplicationOutcome::Evidence(
                tracedecay_application::EvidencePacket {
                    payload: Some(resumed_page),
                    ..
                },
            ),
        ..
    } = resumed.outcome
    else {
        panic!("resumed Work page must carry multi-root evidence");
    };
    assert!(
        resumed_page.continuation.is_none(),
        "the second underlying Work page must terminate the aggregate cursor"
    );
    for root in resumed_page.roots {
        let tracedecay_domain::ScopeOutcome::Exact(values) = root.outcome else {
            panic!("each authorized Work root must resume exactly");
        };
        assert_eq!(
            values
                .first()
                .and_then(|value| value.get("changed"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "each root must advance to its second Work item instead of replaying page one"
        );
    }

    let mut tampered = continuation.as_str().to_owned();
    let replacement = if tampered.ends_with('0') { '1' } else { '0' };
    tampered.pop();
    tampered.push(replacement);
    let tampered =
        tracedecay_application::MultiRootContinuationV1::from_opaque(tampered).expect("opaque");
    let tampered_at = now();
    let (deadline, cancellation) = controls("tampered-execute", tampered_at);
    let tampered = execute_daemon_invocation(
        &engine,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.tampered-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                stored.revision(),
                stored.digest().clone(),
                resumable_operation,
                1,
                Some(tampered),
            )
            .expect("tampered execute request"),
            tampered_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(matches!(
        tampered.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));

    engine.shutdown_all().await;

    let restarted = test_daemon_engine_for_profile(&profile_root);
    restarted
        .open_project_server(&first_handshake)
        .await
        .expect("restarted primary owner");
    let restarted_scope_sets = restarted
        .store_administration
        .registered_profile_database()
        .await
        .expect("restarted profile database")
        .authorized_scope_set_storage()
        .expect("restarted scope-set storage");
    assert_eq!(
        restarted_scope_sets.read(&scope_set_id).unwrap().as_ref(),
        Some(&stored),
        "restart must retain the canonical scope set"
    );
    let observed_at = now();
    let (deadline, cancellation) = controls("restart-execute", observed_at);
    let restarted_execute = execute_daemon_invocation(
        &restarted,
        &first_handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.restart-execute",
            MultiRootExecuteRequestV1::new(
                scope_set_id,
                stored.revision(),
                stored.digest().clone(),
                MultiRootOperationV1::Work {
                    request: json!({
                        "operation": "snapshot",
                        "request": { "page_size": 1 }
                    }),
                },
                0,
                None,
            )
            .expect("restart execute request"),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(
        matches!(
            restarted_execute.outcome,
            DaemonInvocationOutcome::MultiRootQueryPage { .. }
        ),
        "restart must execute the persisted linked-worktree locator: {:#?}",
        restarted_execute.outcome
    );
    for root in stored.roots() {
        let locator = root.locator().expect("registered root locator");
        assert!(
            restarted
                .invocation
                .service
                .lsp_owner_matches_scope(&locator.canonical_root, root.scope())
                .await,
            "execute must cold-mount each exact registered root"
        );
    }
    restarted.shutdown_all().await;
}
