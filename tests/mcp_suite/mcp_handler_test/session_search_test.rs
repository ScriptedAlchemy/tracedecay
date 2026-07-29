use crate::support::*;
use serde_json::json;

/// Same contract for `tracedecay_message_search`: invalid scope values fail
/// closed instead of broadening the search to every session.
#[tokio::test]
async fn message_search_rejects_invalid_scope() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for invalid in ["everything", "", "parents"] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", "scope": invalid}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("scope must be one of all, parents_only, subagents_only"),
            "unexpected error for scope {invalid:?}: {err}"
        );
    }

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "provider": "unknown-agent"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("unknown session provider 'unknown-agent'"),
        "unexpected provider error: {err}"
    );

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "message_type": "promptish"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("message_type must be one of all, direct_user, tool_result"),
        "unexpected message_type error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Regression: catch-up flag ordering — transcript_ingest_done must lag
// ---------------------------------------------------------------------------

/// `wait_for_startup_catch_up` must wait for the transcript-ingest task to
/// complete (transcript_ingest_done), not just the file-tree sync
/// (startup_catch_up_done). This test verifies that after waiting, the
/// `transcript_ingest_done` flag is always true.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_startup_catch_up_waits_for_transcript_ingest_flag() {
    let dir = test_temp_dir();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let server = tracedecay::mcp::McpServer::new(cg.into_inner(), None).await;

    let completed = server
        .wait_for_startup_catch_up(std::time::Duration::from_secs(30))
        .await;

    assert!(completed, "wait_for_startup_catch_up timed out after 30s");

    // After the wait returns true, both flags must be set.
    assert!(
        server.startup_catch_up_done(),
        "startup_catch_up_done must be true after wait"
    );
    assert!(
        server.transcript_ingest_done(),
        "transcript_ingest_done must be true after wait"
    );

    server.shutdown().await;
}

/// `project_scope` is a closed enum: any value other than `all_registered`
/// must fail closed rather than silently degrade to a single-project search.
#[tokio::test]
async fn message_search_rejects_unsupported_project_scope() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for invalid in ["everything", "all", "registered", "ALL_REGISTERED"] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", "project_scope": invalid}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("project_scope must be omitted or all_registered"),
            "unexpected error for project_scope {invalid:?}: {err}"
        );
    }
}

/// The PR15-deferred `all_registered` scope cannot be paired with a
/// single-project selector.
#[tokio::test]
async fn message_search_rejects_all_registered_with_project_selector() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for selector in [
        json!({"project_id": "proj_x"}),
        json!({"project_path": "/some/path"}),
        json!({"project_selector": {"path": "/some/path"}}),
    ] {
        let mut args = json!({"query": "anything", "project_scope": "all_registered"});
        args.as_object_mut()
            .unwrap()
            .extend(selector.as_object().unwrap().clone());
        let err = expect_tool_error(
            handle_tool_call(&cg, "tracedecay_message_search", args, None, None).await,
        );
        assert!(
            err.contains(
                "project_scope cannot be combined with project_id, project_path, or project_selector"
            ),
            "unexpected error for selector {selector}: {err}"
        );
    }
}
