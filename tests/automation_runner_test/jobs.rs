//! User-defined scheduled job tests (Hermes cron parity, audit R9):
//! persistence round-trip, cron parsing, schedule due-decision, file
//! delivery, pre-run command gating, and ledger recording.

use crate::support::*;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tracedecay::automation::jobs::{
    AutomationJob, JobDelivery, UserJobRunOptions, job_schedule_decision, job_task_key, load_jobs,
    run_user_job_with_backend, save_jobs, validate_job,
};
use tracedecay::automation::scheduler::{AutomationSchedule, cron_is_due, parse_schedule};

fn sample_job(id: &str) -> AutomationJob {
    AutomationJob {
        id: id.to_string(),
        name: "Daily digest".to_string(),
        prompt: "Summarize what changed today.".to_string(),
        schedule: Some("hourly".to_string()),
        enabled: true,
        interval_secs: None,
        cooldown_secs: None,
        skill_ids: Vec::new(),
        pre_run_command: None,
        delivery: JobDelivery::File { path: None },
        created_at: 1_715_000_000,
        updated_at: 1_715_000_000,
        extra: BTreeMap::new(),
    }
}

fn enabled_job_config() -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    }
}

struct ContentBackend {
    calls: AtomicUsize,
    content: &'static str,
}

impl ContentBackend {
    fn new(content: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            content,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for ContentBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::UserJob);
        assert!(request.contract.task_key.starts_with("user_job:"));
        assert_eq!(request.contract.prompt_version, "user_job:v1");
        assert!(!request.contract.strict_json);
        assert!(request.prompt.contains("## Job prompt"));
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.content.to_string(),
            output_json: None,
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

#[tokio::test]
async fn job_persistence_round_trips() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let mut job = sample_job("daily-digest");
    job.skill_ids = vec!["automation-run-review".to_string()];
    job.pre_run_command = Some("git log --oneline -5".to_string());
    job.delivery = JobDelivery::Webhook {
        url: "https://example.test/hook".to_string(),
    };
    let other = AutomationJob {
        id: "cron-job".to_string(),
        schedule: Some("*/5 * * * *".to_string()),
        ..sample_job("cron-job")
    };
    save_jobs(root, &[job.clone(), other.clone()])
        .await
        .unwrap();

    let loaded = load_jobs(root).await.unwrap();
    assert_eq!(loaded, vec![job, other]);
    assert!(
        load_jobs(temp.path().join("missing").as_path())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn load_jobs_skips_corrupt_entries_without_losing_valid_jobs() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let valid_one = sample_job("valid-one");
    let valid_two = sample_job("valid-two");
    let document = json!({
        "schema_version": 1,
        "jobs": [
            valid_one,
            {
                "id": "broken",
                "name": "Broken",
                "prompt": 42,
                "enabled": true
            },
            valid_two
        ]
    });
    fs::write(
        root.join("automation_jobs.json"),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let loaded = load_jobs(root).await.unwrap();
    let ids: Vec<&str> = loaded.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, vec!["valid-one", "valid-two"]);
}

#[tokio::test]
async fn job_persistence_preserves_unknown_job_fields() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let document = json!({
        "schema_version": 1,
        "jobs": [
            {
                "id": "future-job",
                "name": "Future job",
                "prompt": "Preserve future fields.",
                "enabled": true,
                "delivery": {"mode": "file"},
                "future_flag": true,
                "nested_future": {"level": 2}
            }
        ]
    });
    fs::write(
        root.join("automation_jobs.json"),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let loaded = load_jobs(root).await.unwrap();
    save_jobs(root, &loaded).await.unwrap();
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("automation_jobs.json")).unwrap()).unwrap();

    assert_eq!(persisted["jobs"][0]["future_flag"], json!(true));
    assert_eq!(persisted["jobs"][0]["nested_future"], json!({"level": 2}));
}

#[test]
fn job_validation_rejects_bad_definitions() {
    let mut job = sample_job("ok-job");
    validate_job(&job).unwrap();

    job.id = "../escape".to_string();
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("prompt");
    job.prompt = "  ".to_string();
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("sched");
    job.schedule = Some("61 * * * *".to_string());
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("delivery");
    job.delivery = JobDelivery::File {
        path: Some("../outside.md".to_string()),
    };
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("reserved-job-state");
    job.delivery = JobDelivery::File {
        path: Some("automation_jobs.json".to_string()),
    };
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("reserved-run-ledger");
    job.delivery = JobDelivery::File {
        path: Some("automation_runs.jsonl".to_string()),
    };
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("reserved-artifact-dir");
    job.delivery = JobDelivery::File {
        path: Some("automation_artifacts/run/output.md".to_string()),
    };
    assert!(validate_job(&job).is_err());

    let mut job = sample_job("safe-custom-output");
    job.delivery = JobDelivery::File {
        path: Some("job-output/custom.md".to_string()),
    };
    validate_job(&job).unwrap();

    let mut job = sample_job("hook");
    job.delivery = JobDelivery::Webhook {
        url: "ftp://example.test".to_string(),
    };
    assert!(validate_job(&job).is_err());
}

#[test]
fn job_validation_rejects_webhook_ssrf_targets() {
    for url in [
        "http://localhost/hook",
        "http://localhost.localdomain/hook",
        "http://127.0.0.1/hook",
        "http://10.0.0.5/hook",
        "http://172.16.0.5/hook",
        "http://192.168.1.10/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/hook",
        "http://[fe80::1]/hook",
        "http://[fc00::1]/hook",
    ] {
        let mut job = sample_job("hook");
        job.delivery = JobDelivery::Webhook {
            url: url.to_string(),
        };
        assert!(validate_job(&job).is_err(), "{url} must be rejected");
    }

    let mut job = sample_job("hook");
    job.delivery = JobDelivery::Webhook {
        url: "https://example.test/hook".to_string(),
    };
    validate_job(&job).unwrap();
}

#[test]
fn parse_schedule_accepts_five_field_cron_expressions() {
    let AutomationSchedule::Cron(cron) = parse_schedule(Some("*/15 9-17 * * 1-5")).unwrap() else {
        panic!("expected cron schedule");
    };
    // 2026-07-01 was a Wednesday. 09:15 UTC matches, 08:15 and weekend do not.
    // 2026-07-01 09:15:00 UTC = 1782897300.
    let wednesday_0915 = 1_782_897_300;
    assert!(cron.matches(wednesday_0915));
    assert!(!cron.matches(wednesday_0915 - 3_600)); // 08:15
    assert!(!cron.matches(wednesday_0915 + 4 * 86_400)); // Sunday 09:15

    // Legacy shorthands keep working.
    assert_eq!(
        parse_schedule(Some("hourly")).unwrap(),
        AutomationSchedule::Interval {
            every_secs: 60 * 60
        }
    );

    // Day-of-week 7 folds onto Sunday (0).
    let AutomationSchedule::Cron(sunday) = parse_schedule(Some("0 0 * * 7")).unwrap() else {
        panic!("expected cron schedule");
    };
    // 2026-07-05 00:00 UTC was a Sunday = 1783209600.
    assert!(sunday.matches(1_783_209_600));

    let AutomationSchedule::Cron(weekday_only) = parse_schedule(Some("0 0 */1 * 1")).unwrap()
    else {
        panic!("expected cron schedule");
    };
    // `*/1` is equivalent to `*`, so the restricted day-of-week field should
    // still be authoritative here. Monday matches; Tuesday does not.
    assert!(weekday_only.matches(1_783_296_000)); // 2026-07-06 00:00 UTC.
    assert!(!weekday_only.matches(1_783_382_400)); // 2026-07-07 00:00 UTC.

    assert!(parse_schedule(Some("60 * * * *")).is_err());
    assert!(parse_schedule(Some("* * * * * *")).is_err());
    assert!(parse_schedule(Some("*/0 * * * *")).is_err());
    assert!(parse_schedule(Some("5-1 * * * *")).is_err());
}

#[test]
fn cron_previous_occurrence_drives_due_decision() {
    let AutomationSchedule::Cron(daily_noon) = parse_schedule(Some("0 12 * * *")).unwrap() else {
        panic!("expected cron schedule");
    };
    // 2026-07-01 13:00 UTC = 1782910800; the last occurrence was 12:00
    // (1782907200).
    let now = 1_782_910_800;
    assert_eq!(daily_noon.previous_occurrence(now), Some(1_782_907_200));
    // Ran before noon -> due; ran after noon -> not due.
    assert!(cron_is_due(&daily_noon, Some(1_782_900_000), now));
    assert!(!cron_is_due(&daily_noon, Some(1_782_907_260), now));
    assert!(cron_is_due(&daily_noon, None, now));
}

#[test]
fn job_schedule_decision_enforces_interval_cron_and_enabled() {
    let now = 1_782_910_800; // 2026-07-01 13:00 UTC
    let mut job = sample_job("digest");
    job.schedule = Some("0 12 * * *".to_string());

    // No prior run: due.
    assert_eq!(job_schedule_decision(&job, &[], now), None);

    // Prior scheduler run after the noon occurrence: not due.
    let mut ran_after = scheduler_record("run-1", AutomationRunStatus::Succeeded, 1_782_907_500);
    ran_after.task = AgentTaskKind::UserJob;
    ran_after.task_key = Some(job_task_key(&job.id));
    assert_eq!(
        job_schedule_decision(&job, std::slice::from_ref(&ran_after), now),
        Some("scheduler_cron_not_due")
    );

    // Prior run before the occurrence: due again.
    let mut ran_before = ran_after.clone();
    ran_before.completed_at = "1782900000".to_string();
    assert_eq!(job_schedule_decision(&job, &[ran_before], now), None);

    // Interval schedules respect elapsed time.
    job.schedule = Some("every 1h".to_string());
    let mut recent = ran_after.clone();
    recent.completed_at = (now - 60).to_string();
    assert_eq!(
        job_schedule_decision(&job, std::slice::from_ref(&recent), now),
        Some("scheduler_interval_not_elapsed")
    );
    let mut stale = recent.clone();
    stale.completed_at = (now - 7_200).to_string();
    assert_eq!(job_schedule_decision(&job, &[stale], now), None);

    // Failures apply the cooldown before retrying.
    let mut failed = recent.clone();
    failed.status = AutomationRunStatus::Failed;
    failed.error = Some("rate limit".to_string());
    assert_eq!(
        job_schedule_decision(&job, std::slice::from_ref(&failed), now),
        Some("scheduler_cooldown_active")
    );

    // Disabled jobs never run from the scheduler.
    job.enabled = false;
    assert_eq!(
        job_schedule_decision(&job, &[], now),
        Some("user_job_disabled")
    );

    // Manual schedules are never picked up.
    job.enabled = true;
    job.schedule = Some("manual".to_string());
    assert_eq!(
        job_schedule_decision(&job, &[], now),
        Some("scheduler_schedule_manual")
    );
}

#[tokio::test]
async fn user_job_delivers_output_to_file_and_records_ledger() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();

    let job = sample_job("daily-digest");
    let config = enabled_job_config();
    let backend = ContentBackend::new("# Digest\n\nNothing changed today.");
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &backend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("user-job-run-1".to_string()),
            profile_root: Some(profile_root),
            project_root: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.report["status"], json!("delivered"));

    // File delivery landed under the default job-output directory.
    let output_path = dashboard_root
        .join("job-output")
        .join("daily-digest")
        .join("user-job-run-1.md");
    let delivered = fs::read_to_string(&output_path).unwrap();
    assert_eq!(delivered, "# Digest\n\nNothing changed today.");
    assert_eq!(
        run.report["delivery"]["path"],
        json!(output_path.display().to_string())
    );

    // Ledger recording under user_job:<id> with the standard artifact chain.
    let record = &run.ledger_record;
    assert_eq!(record.task, AgentTaskKind::UserJob);
    assert_eq!(record.task_key.as_deref(), Some("user_job:daily-digest"));
    assert_eq!(record.prompt_version.as_deref(), Some("user_job:v1"));
    assert_eq!(record.status, AutomationRunStatus::Succeeded);
    assert_eq!(record.trigger, AutomationTrigger::Dashboard);
    let kinds: Vec<&str> = record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );

    let records = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, "user-job-run-1");
    assert_eq!(
        records[0].task_key.as_deref(),
        Some("user_job:daily-digest")
    );
}

#[tokio::test]
async fn user_job_pre_run_command_is_refused_unless_allowed() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();

    let mut job = sample_job("cmd-job");
    job.pre_run_command = Some("echo hello-from-command".to_string());

    // Default config: allow_job_commands=false -> refused, backend not called.
    let config = enabled_job_config();
    assert!(!config.allow_job_commands);
    let backend = ContentBackend::new("unused");
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &backend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("cmd-run-1".to_string()),
            profile_root: Some(profile_root.clone()),
            project_root: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(backend.calls(), 0);
    assert_eq!(run.report["status"], json!("skipped"));
    assert_eq!(run.report["reason"], json!("job_commands_disabled"));
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("job_commands_disabled")
    );

    // Opting in runs the command and injects its stdout into the prompt.
    struct AssertCommandOutputBackend;
    impl AgentTaskBackend for AssertCommandOutputBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> tracedecay_automation::Result<AgentTaskResponse> {
            assert!(request.prompt.contains("## Pre-run command output"));
            assert!(request.prompt.contains("hello-from-command"));
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "done".to_string(),
                output_json: None,
                model: Some("fixture-model".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }
    let config = AutomationConfig {
        allow_job_commands: true,
        ..enabled_job_config()
    };
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &AssertCommandOutputBackend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("cmd-run-2".to_string()),
            profile_root: Some(profile_root),
            project_root: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(run.report["status"], json!("delivered"));
}

#[tokio::test]
async fn user_job_pre_run_command_runs_from_project_root() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    let project_root = temp.path().join("project");
    fs::create_dir_all(&profile_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    fs::write(project_root.join("marker.txt"), "project-marker").unwrap();

    let mut job = sample_job("cmd-cwd");
    job.pre_run_command = Some("cat marker.txt".to_string());

    struct AssertProjectCommandOutputBackend;
    impl AgentTaskBackend for AssertProjectCommandOutputBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> tracedecay_automation::Result<AgentTaskResponse> {
            assert!(request.prompt.contains("## Pre-run command output"));
            assert!(request.prompt.contains("project-marker"));
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "done".to_string(),
                output_json: None,
                model: Some("fixture-model".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    let config = AutomationConfig {
        allow_job_commands: true,
        ..enabled_job_config()
    };
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &AssertProjectCommandOutputBackend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("cmd-cwd-run".to_string()),
            profile_root: Some(profile_root),
            project_root: Some(project_root),
        },
    )
    .await
    .unwrap();
    assert_eq!(run.report["status"], json!("delivered"));
}

#[tokio::test]
async fn scheduler_user_job_uses_explicit_profile_root_for_attached_skills() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    let project_root = temp.path().join("project");
    fs::create_dir_all(&profile_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "job-context".to_string(),
            title: "Job context".to_string(),
            summary: "Adds job context.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Use the profile-specific job context.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "job-context")
        .await
        .unwrap();

    let mut job = sample_job("profile-skill");
    job.skill_ids = vec!["job-context".to_string()];

    struct AssertAttachedSkillBackend;
    impl AgentTaskBackend for AssertAttachedSkillBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> tracedecay_automation::Result<AgentTaskResponse> {
            assert!(request.prompt.contains("## Attached skill: Job context"));
            assert!(request.prompt.contains("profile-specific job context"));
            assert_eq!(request.context["attached_skills"], json!(["job-context"]));
            assert_eq!(request.context["missing_skills"], json!([]));
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "done".to_string(),
                output_json: None,
                model: Some("fixture-model".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    let run = run_user_job_with_backend(
        &dashboard_root,
        &enabled_job_config(),
        &AssertAttachedSkillBackend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some("profile-skill-run".to_string()),
            profile_root: Some(profile_root),
            project_root: Some(project_root),
        },
    )
    .await
    .unwrap();
    assert_eq!(run.report["status"], json!("delivered"));
}

#[tokio::test]
async fn user_job_does_not_attach_archived_managed_skills() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    let project_root = temp.path().join("project");
    fs::create_dir_all(&profile_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "archived-job-context".to_string(),
            title: "Archived job context".to_string(),
            summary: "Must not be attached after archival.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "ARCHIVED_SKILL_BODY_MUST_NOT_RUN".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "archived-job-context")
        .await
        .unwrap();
    tracedecay::automation::managed_skills::archive_managed_skill(
        &profile_root,
        "archived-job-context",
    )
    .await
    .unwrap();

    let mut job = sample_job("archived-profile-skill");
    job.skill_ids = vec!["archived-job-context".to_string()];

    struct AssertArchivedSkillExcludedBackend;
    impl AgentTaskBackend for AssertArchivedSkillExcludedBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> tracedecay_automation::Result<AgentTaskResponse> {
            assert!(!request.prompt.contains("ARCHIVED_SKILL_BODY_MUST_NOT_RUN"));
            assert_eq!(request.context["attached_skills"], json!([]));
            assert_eq!(
                request.context["missing_skills"],
                json!(["archived-job-context"])
            );
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "done".to_string(),
                output_json: None,
                model: Some("fixture-model".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    let run = run_user_job_with_backend(
        &dashboard_root,
        &enabled_job_config(),
        &AssertArchivedSkillExcludedBackend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some("archived-profile-skill-run".to_string()),
            profile_root: Some(profile_root),
            project_root: Some(project_root),
        },
    )
    .await
    .unwrap();
    assert_eq!(run.report["status"], json!("delivered"));
}

#[tokio::test]
async fn user_job_backend_failure_records_failed_ledger_entry() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();

    let job = sample_job("fails");
    let config = AutomationConfig {
        timeout_secs: 1,
        ..enabled_job_config()
    };
    let backend = FailingBackend::new(AgentTaskKind::UserJob);
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &backend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("fail-run-1".to_string()),
            profile_root: Some(profile_root),
            project_root: None,
        },
    )
    .await
    .unwrap();

    // The backend failure is transient, but this test pins the failed-ledger
    // record, not retry semantics (covered by backend.rs retry tests) —
    // timeout_secs: 1 short-circuits the backoff so the test stays fast.
    assert_eq!(backend.calls(), 1);
    assert_eq!(run.report["status"], json!("failed"));
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Failed);
    assert_eq!(
        run.ledger_record.task_key.as_deref(),
        Some("user_job:fails")
    );
    assert_eq!(
        run.ledger_record.error_classification,
        Some(AgentTaskFailureClass::Unavailable)
    );
    let records = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
}

#[tokio::test]
async fn scheduler_trigger_skips_repeat_and_respects_lock_discipline() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();

    let mut job = sample_job("sched-job");
    job.schedule = Some("manual".to_string());
    let config = enabled_job_config();
    let backend = ContentBackend::new("unused");

    // Manual schedule: the scheduler skips, recording the skip once.
    for run_id in ["sched-run-1", "sched-run-2"] {
        let run = run_user_job_with_backend(
            &dashboard_root,
            &config,
            &backend,
            &job,
            UserJobRunOptions {
                trigger: AutomationTrigger::Scheduler,
                run_id: Some(run_id.to_string()),
                profile_root: Some(profile_root.clone()),
                project_root: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(run.report["reason"], json!("scheduler_schedule_manual"));
    }
    assert_eq!(backend.calls(), 0);
    let records = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(
        records.len(),
        1,
        "repeat scheduler skips must persist only once"
    );
    assert_eq!(records[0].run_id, "sched-run-1");
}

#[tokio::test]
async fn manual_job_trigger_respects_existing_per_job_lock() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();
    let lock_dir = dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("user_job_locked-job.lock"), b"9999999999").unwrap();

    let job = sample_job("locked-job");
    let config = enabled_job_config();
    let backend = ContentBackend::new("unused");
    let run = run_user_job_with_backend(
        &dashboard_root,
        &config,
        &backend,
        &job,
        UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("dashboard-lock-run".to_string()),
            profile_root: Some(profile_root),
            project_root: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.report["status"], json!("skipped"));
    assert_eq!(run.report["reason"], json!("job_lock_active"));
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
}

#[tokio::test]
async fn concurrent_manual_job_triggers_do_not_double_execute() {
    struct SlowBackend {
        calls: AtomicUsize,
    }
    impl AgentTaskBackend for SlowBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> tracedecay_automation::Result<AgentTaskResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "done".to_string(),
                output_json: None,
                model: Some("fixture-model".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(&profile_root).unwrap();
    let job = sample_job("concurrent-job");
    let config = enabled_job_config();
    let backend = SlowBackend {
        calls: AtomicUsize::new(0),
    };

    let (first, second) = tokio::join!(
        run_user_job_with_backend(
            &dashboard_root,
            &config,
            &backend,
            &job,
            UserJobRunOptions {
                trigger: AutomationTrigger::Dashboard,
                run_id: Some("concurrent-run-1".to_string()),
                profile_root: Some(profile_root.clone()),
                project_root: None,
            },
        ),
        run_user_job_with_backend(
            &dashboard_root,
            &config,
            &backend,
            &job,
            UserJobRunOptions {
                trigger: AutomationTrigger::ManualCli,
                run_id: Some("concurrent-run-2".to_string()),
                profile_root: Some(profile_root),
                project_root: None,
            },
        )
    );
    let runs = [first.unwrap(), second.unwrap()];
    let delivered = runs
        .iter()
        .filter(|run| run.report["status"] == json!("delivered"))
        .count();
    let locked = runs
        .iter()
        .filter(|run| run.report["reason"] == json!("job_lock_active"))
        .count();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(delivered, 1);
    assert_eq!(locked, 1);
}
