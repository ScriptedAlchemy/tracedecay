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
            crate::automation::backend::AgentTaskKind::SkillWriter,
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
    let record = crate::automation::run_ledger::AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: "run-123".to_string(),
        trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
        task: crate::automation::backend::AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some("memory_curator:v1".to_string()),
        response_schema: None,
        strict_json: None,
        model: None,
        status: crate::automation::run_ledger::AutomationRunStatus::Skipped,
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

#[cfg(unix)]
#[test]
fn automation_staged_log_line_is_stable() {
    let line = super::super::format_daemon_log_line(
        "automation_staged",
        &super::super::automation_staged_log_fields(
            std::path::Path::new("/tmp/project"),
            &crate::automation::staged_notice::AutomationPendingCounts {
                fact_proposals: crate::automation::staged_notice::PendingReviewCount::Counted(2),
                skills: crate::automation::staged_notice::PendingReviewCount::Counted(1),
            },
        ),
    );

    assert_eq!(
        line,
        "[tracedecay] event=automation_staged project=/tmp/project pending_fact_proposals=2 pending_skills=1"
    );
}

#[cfg(unix)]
#[test]
fn automation_staged_log_line_marks_an_unreadable_queue() {
    let line = super::super::format_daemon_log_line(
        "automation_staged",
        &super::super::automation_staged_log_fields(
            std::path::Path::new("/tmp/project"),
            &crate::automation::staged_notice::AutomationPendingCounts {
                fact_proposals: crate::automation::staged_notice::PendingReviewCount::Counted(2),
                skills: crate::automation::staged_notice::PendingReviewCount::unreadable(
                    "profile root missing",
                ),
            },
        ),
    );

    assert_eq!(
        line,
        "[tracedecay] event=automation_staged project=/tmp/project pending_fact_proposals=2 pending_skills=unreadable"
    );
}
