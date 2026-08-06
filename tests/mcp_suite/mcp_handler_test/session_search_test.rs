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

/// The `all_registered` scope cannot be paired with a single-project
/// selector.
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
