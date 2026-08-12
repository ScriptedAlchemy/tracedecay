#[test]
fn daemon_log_line_formats_stable_key_value_fields() {
    let line = super::super::format_daemon_log_line(
        "scheduler_task",
        &[
            ("task", "memory_curator".to_string()),
            ("outcome", "not due yet".to_string()),
            ("project", "/tmp/example project".to_string()),
        ],
    );

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task task=memory_curator outcome=\"not due yet\" project=\"/tmp/example project\""
    );
}

#[test]
fn scheduler_application_problem_log_excludes_hostile_payload() {
    use tracedecay_application::retained_surfaces::{AutomationRunProblemV1, AutomationTaskV1};
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, LegalAction, RequestId, ResolvedScope,
        RetainedSurfaceOperation, RetryDirective, SafeDiagnostic,
        retained_surface_application_operation,
    };
    use tracedecay_domain::{ProjectId, RepositoryId, RunId, WorktreeId};

    const SECRET: &str = "sk-scheduler-log-canary-1234567890";
    let request_id = RequestId::new("request.scheduler.log-privacy").unwrap();
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::AutomationRun).unwrap();
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
    let problem = AutomationRunProblemV1::new(
        RunId::new("run.scheduler-log-privacy").unwrap(),
        AutomationTaskV1::MemoryCurator,
        scope,
        envelope,
        Vec::new(),
        &request_id,
    )
    .unwrap();
    let fields = super::super::scheduler::scheduler_application_problem_log_fields(
        std::path::Path::new("/projects/log-privacy"),
        tracedecay_agent_hosts::automation::backend::AgentTaskKind::MemoryCurator,
        &problem,
    );
    let line = super::super::format_daemon_log_line("scheduler_task_application_problem", &fields);

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
    let line = super::super::format_daemon_log_line(
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
    let line = super::super::format_daemon_log_line(
        "client_error",
        &[("error", "first\nsecond\rthird\tfourth".to_string())],
    );

    assert_eq!(
        line,
        r#"[tracedecay] event=client_error error="first\nsecond\rthird\tfourth""#
    );
}

#[cfg(unix)]
#[test]
fn scheduler_task_start_log_uses_task_key_and_project() {
    let line = super::super::format_daemon_log_line(
        "scheduler_task",
        &super::super::scheduler_task_log_fields(
            std::path::Path::new("/tmp/project with spaces"),
            tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
            "start",
        ),
    );

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task project=\"/tmp/project with spaces\" task=skill_writer outcome=start"
    );
}

#[cfg(unix)]
#[test]
fn scheduler_record_log_preserves_skipped_status_and_reason() {
    let record = tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: "run-123".to_string(),
        trigger: tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Scheduler,
        task: tracedecay_agent_hosts::automation::backend::AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some("memory_curator:v1".to_string()),
        response_schema: None,
        strict_json: None,
        model: None,
        status: tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Skipped,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 1,
        error: None,
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: Some("scheduler_interval_not_elapsed".to_string()),
        report_ref: None,
        artifacts: Vec::new(),
        started_at: "1000".to_string(),
        completed_at: "1001".to_string(),
        completed_at_micros: Some(1_001_000_000),
    };

    let line = super::super::daemon_scheduler_record_log_line(
        std::path::Path::new("/tmp/project"),
        &record,
    );

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task project=/tmp/project task=memory_curator outcome=skipped run_id=run-123 reason=scheduler_interval_not_elapsed"
    );
}
