use tracedecay_runtime_core::logging::format_daemon_log_line;

#[cfg(unix)]
#[test]
fn scheduler_application_problem_log_excludes_hostile_payload() {
    use tracedecay_contracts::retained_surfaces::AutomationRunProblemV1;
    use tracedecay_contracts::{
        ApplicationProblem, ApplicationProblemEnvelope, LegalAction, RequestId, ResolvedScope,
        RetainedSurfaceOperation, RetryDirective, SafeDiagnostic,
        retained_surface_application_operation,
    };
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

    const SECRET: &str = "sk-scheduler-log-canary-1234567890";
    let request_id = RequestId::new("request.scheduler.log-privacy").unwrap();
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate).unwrap();
    let envelope = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        ApplicationProblem::ResetRequired {
            diagnostic: SafeDiagnostic::new(
                "application.memory-automation-run.reset-required",
                format!("hostile automatic fact content api_key={SECRET}"),
            )
            .unwrap(),
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reset],
        },
    )
    .unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.scheduler-log-privacy").unwrap(),
        RepositoryId::new("repository.scheduler-log-privacy").unwrap(),
        WorktreeId::new("worktree.scheduler-log-privacy").unwrap(),
        None,
    )
    .unwrap();
    let request =
        tracedecay_automation_runtime::automation::effect_runtime::memory_curator_run_request(
            "run.scheduler-log-privacy",
            24,
            0.72,
        )
        .unwrap();
    let problem =
        AutomationRunProblemV1::new(&request, scope, envelope, Vec::new(), &request_id).unwrap();
    let fields = super::super::scheduler::scheduler_application_problem_log_fields(
        std::path::Path::new("/projects/log-privacy"),
        tracedecay_automation_runtime::automation::backend::AgentTaskKind::MemoryCurator,
        &problem,
    );
    let line = format_daemon_log_line("scheduler_task_application_problem", &fields);

    assert!(!line.contains(SECRET));
    assert!(!line.contains("hostile automatic fact content"));
    assert!(line.contains("request.scheduler.log-privacy"));
    assert!(line.contains("run.scheduler-log-privacy"));
    assert!(line.contains("problem_kind=reset_required"));
    assert!(line.contains("problem_code=application.memory-automation-run.reset-required"));
    assert!(line.contains("committed_receipt_count=0"));
}
#[test]
fn daemon_log_line_escapes_quotes_and_backslashes() {
    let line = format_daemon_log_line(
        "client_error",
        &[("error", r#"failed at "step" \ retry"#.to_string())],
    );

    assert_eq!(
        line,
        r#"[tracedecay] event=client_error error="failed at \"step\" \\ retry""#
    );
}

#[test]
fn daemon_log_line_escapes_control_characters() {
    let line = format_daemon_log_line(
        "client_error",
        &[("error", "first\nsecond\rthird\tfourth".to_string())],
    );

    assert_eq!(
        line,
        r#"[tracedecay] event=client_error error="first\nsecond\rthird\tfourth""#
    );
}

#[test]
fn retention_degraded_log_formats_the_pass_and_failure() {
    let fields = [
        ("pass", "semantic_vector_generations".to_string()),
        (
            "failure",
            "unavailable:semantic retrieval is not calibrated".to_string(),
        ),
    ];
    assert_eq!(
        format_daemon_log_line("retention_degraded", &fields),
        "[tracedecay] event=retention_degraded pass=semantic_vector_generations failure=\"unavailable:semantic retrieval is not calibrated\""
    );
}
