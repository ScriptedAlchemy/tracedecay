use serde_json::{Value, json};
use tracedecay_domain::SESSION_EVIDENCE_BUDGET_SUPPRESSED;

use super::{
    AutomationRunResultV1, AutomationSkipReasonV1, AutomationTaskV1, automation_request,
    with_request_digest, zero_terminal,
};

#[test]
fn zero_effect_completion_and_skip_are_typed_without_partial_receipts() {
    for status in ["completed", "skipped"] {
        let result = serde_json::from_value::<AutomationRunResultV1>(zero_terminal(status))
            .expect("typed zero-effect terminal");
        assert!(result.matches_terminal());
    }
}

#[test]
fn unknown_skill_skip_reasons_fail_closed() {
    for reason in ["skill_writer_evidence_unavailable", "skill_writer_not_due"] {
        assert!(AutomationSkipReasonV1::from_ledger_reason(reason).is_none());
        let mut terminal = zero_terminal("skipped");
        terminal["terminal"]["reason"] = json!(reason);
        assert!(serde_json::from_value::<AutomationRunResultV1>(terminal).is_err());
    }
}

#[test]
fn skipped_reason_must_belong_to_the_selected_task() {
    let mut terminal = zero_terminal("skipped");
    terminal["terminal"]["reason"] = json!("session_reflector_disabled");
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(terminal)
            .expect("typed cross-task skip")
            .matches_terminal()
    );
}

#[test]
fn disabled_job_commands_are_a_user_job_skip_only() {
    let reason = AutomationSkipReasonV1::from_ledger_reason("job_commands_disabled")
        .expect("known user-job skip");
    assert!(reason.matches_task(AutomationTaskV1::UserJob));
    assert!(!reason.matches_task(AutomationTaskV1::SkillWriter));
}

#[test]
fn session_evidence_unavailability_skips_session_backed_writers() {
    let reason =
        AutomationSkipReasonV1::from_ledger_reason("session_evidence_retrieval_unavailable")
            .expect("known session-evidence skip");
    assert!(reason.matches_task(AutomationTaskV1::SessionReflector));
    assert!(reason.matches_task(AutomationTaskV1::SkillWriter));
    assert!(reason.matches_task(AutomationTaskV1::CombinedReview));
    assert!(!reason.matches_task(AutomationTaskV1::MemoryCurator));
    assert!(!reason.matches_task(AutomationTaskV1::UserJob));
}

#[test]
fn budget_backoff_suppression_is_a_typed_session_evidence_skip() {
    assert_eq!(
        SESSION_EVIDENCE_BUDGET_SUPPRESSED,
        "session_evidence_budget_suppressed"
    );
    let reason = AutomationSkipReasonV1::from_ledger_reason(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        .expect("known session-evidence budget suppression skip");
    assert_eq!(
        reason,
        AutomationSkipReasonV1::SessionEvidenceBudgetSuppressed
    );
    assert!(reason.matches_task(AutomationTaskV1::SessionReflector));
    assert!(reason.matches_task(AutomationTaskV1::SkillWriter));
    assert!(reason.matches_task(AutomationTaskV1::CombinedReview));
    assert!(!reason.matches_task(AutomationTaskV1::MemoryCurator));
    assert!(!reason.matches_task(AutomationTaskV1::UserJob));
}

#[test]
fn skill_writer_empty_evidence_is_a_typed_session_evidence_skip() {
    let reason = AutomationSkipReasonV1::from_ledger_reason("no_skill_writer_evidence")
        .expect("skill-writer empty evidence is a registered skip");
    assert_eq!(reason, AutomationSkipReasonV1::NoSessionEvidence);
    assert!(reason.matches_task(AutomationTaskV1::SkillWriter));
    assert!(reason.matches_task(AutomationTaskV1::CombinedReview));
}

#[test]
fn external_effect_receipts_are_task_run_and_input_bound() {
    let receipt = |kind: &str, run_id: &str, task_key: &str| {
        json!({
            "kind": kind,
            "receipt": {
                "run_id": run_id,
                "task_key": task_key,
                "manifest_digest": format!("sha256:{}", "a".repeat(64))
            }
        })
    };
    let terminal = |task: &str, receipt: Value| {
        json!({
            "run_id": "run.external.effect",
            "task": task,
            "request_digest": format!("sha256:{}", "b".repeat(64)),
            "terminal": {
                "status": "completed",
                "summary": {
                    "reviewed_count": 1,
                    "accepted_count": 1,
                    "rejected_count": 0,
                    "skipped_count": 0
                }
            },
            "committed_receipts": [receipt]
        })
    };

    let skill = terminal(
        "skill_writer",
        receipt("skill_writing", "run.external.effect", "skill_writer"),
    );
    assert!(
        serde_json::from_value::<AutomationRunResultV1>(skill)
            .is_ok_and(|result| result.matches_terminal())
    );
    for (task, kind, run_id, task_key) in [
        (
            "user_job",
            "skill_writing",
            "run.external.effect",
            "user_job:nightly",
        ),
        (
            "user_job",
            "user_job_delivery",
            "run.other",
            "user_job:nightly",
        ),
        (
            "skill_writer",
            "skill_writing",
            "run.external.effect",
            "user_job:nightly",
        ),
        (
            "user_job",
            "user_job_delivery",
            "run.external.effect",
            "skill_writer",
        ),
        (
            "user_job",
            "user_job_delivery",
            "run.external.effect",
            "user_job:",
        ),
    ] {
        assert!(
            !serde_json::from_value::<AutomationRunResultV1>(terminal(
                task,
                receipt(kind, run_id, task_key),
            ))
            .is_ok_and(|result| result.matches_terminal())
        );
    }

    let request = automation_request("run.external.effect", AutomationTaskV1::UserJob);
    let result = serde_json::from_value::<AutomationRunResultV1>(with_request_digest(
        terminal(
            "user_job",
            receipt(
                "user_job_delivery",
                "run.external.effect",
                "user_job:nightly",
            ),
        ),
        &request,
    ))
    .expect("bound user-job result");
    assert!(result.matches_admission(&request));
    let mut other_job = request;
    let crate::retained_surfaces::AutomationTaskRequestV1::UserJob(options) = &mut other_job.task
    else {
        panic!("user-job request")
    };
    options.job_id = "other".to_owned();
    assert!(!result.matches_admission(&other_job));
}

#[test]
fn zero_effect_result_is_bound_to_the_full_request() {
    let result = serde_json::from_value::<AutomationRunResultV1>(zero_terminal("completed"))
        .expect("zero-effect result");
    let request = automation_request("run.memory.zero", AutomationTaskV1::MemoryCurator);
    assert!(result.matches_admission(&request));
    assert!(!result.matches_admission(&automation_request(
        "run.memory.other",
        AutomationTaskV1::MemoryCurator,
    )));
    assert!(!result.matches_admission(&automation_request(
        "run.memory.zero",
        AutomationTaskV1::SessionReflector,
    )));
    let mut changed_input = request;
    let crate::retained_surfaces::AutomationTaskRequestV1::MemoryCurator(options) =
        &mut changed_input.task
    else {
        panic!("memory curator request")
    };
    options.fact_review_limit += 1;
    assert!(!result.matches_admission(&changed_input));
}
