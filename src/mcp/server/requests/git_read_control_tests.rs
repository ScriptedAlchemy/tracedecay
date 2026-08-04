use super::*;

#[tokio::test]
async fn execution_settlement_reports_only_observed_worker_state() {
    let not_started = DispatchExecutionSettlement::new().expect("settlement");
    assert_eq!(
        not_started.snapshot(),
        super::request_receipts::ToolCallWorkerSettlement::NotStarted
    );

    let joined = DispatchExecutionSettlement::new().expect("settlement");
    assert_eq!(joined.observe(async { 7_u8 }).await, 7);
    assert_eq!(
        joined.snapshot(),
        super::request_receipts::ToolCallWorkerSettlement::Joined
    );

    let indeterminate = DispatchExecutionSettlement::new().expect("settlement");
    let mut execution = Box::pin(indeterminate.observe(std::future::pending::<()>()));
    tokio::time::timeout(std::time::Duration::from_millis(1), execution.as_mut())
        .await
        .expect_err("pending execution");
    drop(execution);
    let super::request_receipts::ToolCallWorkerSettlement::Indeterminate(reconciliation) =
        indeterminate.snapshot()
    else {
        panic!("started execution must not fabricate a join");
    };
    assert!(reconciliation.id > 0);
    assert_eq!(
        reconciliation.status,
        super::request_receipts::ToolCallWorkerReconciliationStatus::Unavailable
    );
}

#[test]
fn controlled_operations_receive_live_registration_and_bounded_deadlines() {
    assert!(tool_supports_live_cancellation("tracedecay_search"));
    assert!(tool_supports_live_cancellation(
        "tracedecay_run_affected_tests"
    ));
    assert!(!tool_supports_live_cancellation("tracedecay_outline"));
    for tool_name in [
        "tracedecay_git_status",
        "tracedecay_git_diff",
        "tracedecay_git_history",
        "tracedecay_git_blame",
        "tracedecay_git_hunks",
    ] {
        assert!(tool_supports_live_cancellation(tool_name));
        let application_surface =
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
        assert!(
            application_surface.is_some(),
            "Git reads must enter the catalog-owned application surface",
        );
        let controlled_read = is_controlled_read_tool(tool_name);
        assert!(controlled_read);
        assert_eq!(
            dispatch_deadline_horizon_micros(application_surface.is_some(), controlled_read),
            Some(30_000_000)
        );
    }
    for tool_name in [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_api_migration_apply",
        "tracedecay_source_edit_reconcile",
    ] {
        assert!(is_source_edit_tool(tool_name));
        assert_eq!(
            dispatch_deadline_horizon_micros(true, true),
            Some(30_000_000)
        );
    }

    let request_id = "request.git-read-controls".to_owned();
    let signal = tracedecay_application::CancellationSignal::active(
        "cancellation.request.git-read-controls",
    )
    .expect("signal");
    let registry = std::sync::Mutex::new(HashMap::from([(request_id.clone(), signal.clone())]));
    {
        let _registration = ApplicationCancellationRegistration {
            registry: &registry,
            request_id: Some(request_id.clone()),
        };
        signal.cancel(tracedecay_domain::UtcMicros(1));
        assert!(registry.lock().expect("registry").contains_key(&request_id));
    }
    assert!(!registry.lock().expect("registry").contains_key(&request_id));
}

/// These tools walk git trees but are not application-surface operations
/// and are not source edits, so the horizon predicate used to return `None`
/// for them: they dispatched with no deadline at all while the cheaper
/// `tracedecay_git_status` was bounded at thirty seconds.
#[test]
fn git_reading_tools_receive_a_bounded_deadline() {
    for tool_name in [
        "tracedecay_admin_branch_add",
        "tracedecay_affected",
        "tracedecay_diff_context",
        "tracedecay_changelog",
        "tracedecay_commit_context",
        "tracedecay_pr_context",
        "tracedecay_branch_search",
        "tracedecay_branch_diff",
        "tracedecay_branch_list",
    ] {
        assert!(
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name)
                .is_none(),
            "{tool_name} is not an application-surface operation, so only the \
             git-dispatch predicate can bound it",
        );
        assert!(!is_source_edit_tool(tool_name));
        assert!(
            is_controlled_read_tool(tool_name),
            "{tool_name} walks a git tree and must be a controlled read",
        );
        assert_eq!(
            dispatch_deadline_horizon_micros(
                false,
                is_controlled_read_tool(tool_name) || is_source_edit_tool(tool_name),
            ),
            Some(30_000_000),
            "{tool_name} must dispatch with a bounded horizon",
        );
    }
}

/// The horizon predicate reads the canonical binding table, so it must not
/// sweep in reads from other dispatch families.
#[test]
fn non_git_reads_stay_outside_the_controlled_read_horizon() {
    for tool_name in [
        "tracedecay_outline",
        "tracedecay_body",
        "tracedecay_dead_code",
        "tracedecay_health",
        "tracedecay_context",
    ] {
        assert!(
            !is_controlled_read_tool(tool_name),
            "{tool_name} is not a git-walking read",
        );
    }
    assert!(is_controlled_read_tool("tracedecay_search"));
}
