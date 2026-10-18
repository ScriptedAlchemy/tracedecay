use std::path::{Component, Path};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::automation::artifacts::sha256_bytes;
use crate::automation::{AutomationCommittedReceipt, AutomationRunError, AutomationRunResult};
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::native_runtime;

use super::{
    AgentTaskResponse, AutomationJob, JOB_OUTPUT_DIR, JobDelivery, TraceDecayError,
    WEBHOOK_TIMEOUT_SECS, current_timestamp, job_task_key, job_webhook,
    validate_relative_output_path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum ExternalAutomationEffectDisposition {
    UserJobFileDelivered {
        target_digest: String,
        content_digest: String,
    },
    UserJobWebhookDelivered {
        target_digest: String,
        status: u16,
        content_digest: String,
    },
    UserJobDeliveryIndeterminate {
        mode: &'static str,
        target_digest: String,
        content_digest: String,
    },
    SkillWriting {
        created_count: usize,
        updated_count: usize,
        consolidation_count: usize,
        deployment: ExternalSkillDeploymentDisposition,
        mutation_manifest_digest: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSkillDeploymentDisposition {
    NotRequired,
    Complete,
    PartialFailure,
    Unavailable,
}

impl ExternalSkillDeploymentDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Complete => "complete",
            Self::PartialFailure => "partial_failure",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Payload-free identity for a committed non-memory automation effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAutomationEffectReceipt {
    run_id: String,
    task_key: String,
    disposition: ExternalAutomationEffectDisposition,
    manifest_digest: String,
}

impl ExternalAutomationEffectReceipt {
    pub(crate) fn new(
        run_id: &str,
        task_key: &str,
        disposition: ExternalAutomationEffectDisposition,
    ) -> Self {
        let disposition_identity = match &disposition {
            ExternalAutomationEffectDisposition::UserJobFileDelivered {
                target_digest,
                content_digest,
            } => format!("file:{target_digest}:{content_digest}"),
            ExternalAutomationEffectDisposition::UserJobWebhookDelivered {
                target_digest,
                status,
                content_digest,
            } => format!("webhook:{target_digest}:{status}:{content_digest}"),
            ExternalAutomationEffectDisposition::UserJobDeliveryIndeterminate {
                mode,
                target_digest,
                content_digest,
            } => format!("indeterminate:{mode}:{target_digest}:{content_digest}"),
            ExternalAutomationEffectDisposition::SkillWriting {
                created_count,
                updated_count,
                consolidation_count,
                deployment,
                mutation_manifest_digest,
            } => format!(
                "skill:{created_count}:{updated_count}:{consolidation_count}:{}:{mutation_manifest_digest}",
                deployment.as_str(),
            ),
        };
        let manifest_digest = sha256_bytes(
            format!(
                "{}:{run_id}{}:{task_key}{}:{disposition_identity}",
                run_id.len(),
                task_key.len(),
                disposition_identity.len(),
            )
            .as_bytes(),
        );
        Self {
            run_id: run_id.to_owned(),
            task_key: task_key.to_owned(),
            disposition,
            manifest_digest,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn task_key(&self) -> &str {
        &self.task_key
    }

    pub fn disposition(&self) -> &ExternalAutomationEffectDisposition {
        &self.disposition
    }

    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }
}

pub(crate) fn skill_writing_receipt(
    run_id: &str,
    created_count: usize,
    updated_count: usize,
    consolidation_count: usize,
    deployment: ExternalSkillDeploymentDisposition,
    mutation_manifest_digest: String,
) -> AutomationCommittedReceipt {
    AutomationCommittedReceipt::SkillWriting(ExternalAutomationEffectReceipt::new(
        run_id,
        "skill_writer",
        ExternalAutomationEffectDisposition::SkillWriting {
            created_count,
            updated_count,
            consolidation_count,
            deployment,
            mutation_manifest_digest,
        },
    ))
}

#[derive(Debug)]
pub(super) enum JobDeliveryOutcome {
    File { target_digest: String },
    Webhook { target_digest: String, status: u16 },
}

impl JobDeliveryOutcome {
    pub(super) fn committed_receipt(
        &self,
        run_id: &str,
        task_key: &str,
        content_digest: &str,
    ) -> AutomationCommittedReceipt {
        match self {
            Self::File { target_digest } => {
                file_delivery_receipt(run_id, task_key, target_digest, content_digest)
            }
            Self::Webhook {
                target_digest,
                status,
            } => webhook_delivery_receipt(run_id, task_key, target_digest, *status, content_digest),
        }
    }

    pub(super) fn report(&self) -> Value {
        match self {
            Self::File { target_digest } => json!({
                "mode": "file",
                "target_digest": target_digest,
            }),
            Self::Webhook {
                target_digest,
                status,
            } => json!({
                "mode": "webhook",
                "target_digest": target_digest,
                "status": status,
            }),
        }
    }
}

pub(super) fn file_delivery_receipt(
    run_id: &str,
    task_key: &str,
    target_digest: &str,
    content_digest: &str,
) -> AutomationCommittedReceipt {
    AutomationCommittedReceipt::UserJobDelivery(ExternalAutomationEffectReceipt::new(
        run_id,
        task_key,
        ExternalAutomationEffectDisposition::UserJobFileDelivered {
            target_digest: target_digest.to_owned(),
            content_digest: content_digest.to_owned(),
        },
    ))
}

pub(super) fn webhook_delivery_receipt(
    run_id: &str,
    task_key: &str,
    target_digest: &str,
    status: u16,
    content_digest: &str,
) -> AutomationCommittedReceipt {
    AutomationCommittedReceipt::UserJobDelivery(ExternalAutomationEffectReceipt::new(
        run_id,
        task_key,
        ExternalAutomationEffectDisposition::UserJobWebhookDelivered {
            target_digest: target_digest.to_owned(),
            status,
            content_digest: content_digest.to_owned(),
        },
    ))
}

pub(super) fn indeterminate_delivery_receipt(
    run_id: &str,
    task_key: &str,
    mode: &'static str,
    target_digest: &str,
    content_digest: &str,
) -> AutomationCommittedReceipt {
    AutomationCommittedReceipt::UserJobDelivery(ExternalAutomationEffectReceipt::new(
        run_id,
        task_key,
        ExternalAutomationEffectDisposition::UserJobDeliveryIndeterminate {
            mode,
            target_digest: target_digest.to_owned(),
            content_digest: content_digest.to_owned(),
        },
    ))
}

pub(super) async fn deliver_job_output(
    dashboard_root: &Path,
    job: &AutomationJob,
    run_id: &str,
    content_digest: &str,
    response: &AgentTaskResponse,
) -> AutomationRunResult<JobDeliveryOutcome> {
    let task_key = job_task_key(&job.id);
    match &job.delivery {
        JobDelivery::File { path } => {
            let target = file_target(dashboard_root, job, run_id, path.as_deref())?;
            let target_digest = sha256_bytes(target.display().to_string().as_bytes());
            let root = dashboard_root.to_path_buf();
            let mut file = crate::automation::blocking_fs(move || open_job_output(&root, &target))
                .await
                .map_err(|error| {
                    AutomationRunError::Runtime(TraceDecayError::Config {
                        message: format!("failed to open job output: {error}"),
                    })
                })?;
            let output = response.output_text.clone();
            crate::automation::blocking_fs(move || {
                use std::io::Write as _;
                let _span = tracing::trace_span!("automation.job_delivery.write_file").entered();
                file.write_all(output.as_bytes())?;
                file.flush()
            })
            .await
            .map_err(|_| {
                indeterminate_error(run_id, &task_key, "file", &target_digest, content_digest)
            })?;
            Ok(JobDeliveryOutcome::File { target_digest })
        }
        JobDelivery::Webhook { url } => {
            let payload = json!({
                "job_id": job.id,
                "name": job.name,
                "run_id": run_id,
                "content": response.output_text,
                "model": response.model,
                "completed_at": current_timestamp(),
            });
            let target_digest = sha256_bytes(url.as_bytes());
            let post_url = url.clone();
            let attempted = native_runtime::spawn_blocking(move || {
                job_webhook::post_json_url(
                    &post_url,
                    &payload,
                    Duration::from_secs(WEBHOOK_TIMEOUT_SECS),
                )
            })
            .await;
            let status = settle_webhook_attempt(
                attempted,
                run_id,
                &task_key,
                &target_digest,
                content_digest,
            )?;
            Ok(JobDeliveryOutcome::Webhook {
                target_digest,
                status,
            })
        }
    }
}

/// `attempted` is `Err` when the blocking worker ended without a result, so
/// whether the POST went out is unknown.
fn settle_webhook_attempt(
    attempted: std::result::Result<std::result::Result<u16, job_webhook::WebhookPostError>, String>,
    run_id: &str,
    task_key: &str,
    target_digest: &str,
    content_digest: &str,
) -> AutomationRunResult<u16> {
    match attempted {
        Err(_) => Err(indeterminate_error(
            run_id,
            task_key,
            "webhook",
            target_digest,
            content_digest,
        )),
        Ok(Ok(status)) => Ok(status),
        Ok(Err(job_webhook::WebhookPostError::NotAttempted(error))) => {
            Err(AutomationRunError::Runtime(error))
        }
        Ok(Err(job_webhook::WebhookPostError::Indeterminate)) => Err(indeterminate_error(
            run_id,
            task_key,
            "webhook",
            target_digest,
            content_digest,
        )),
    }
}

pub(super) fn delivery_context(
    dashboard_root: &Path,
    job: &AutomationJob,
    run_id: &str,
) -> Result<Value> {
    match &job.delivery {
        JobDelivery::File { path } => {
            let target = file_target(dashboard_root, job, run_id, path.as_deref())?;
            Ok(json!({
                "mode": "file",
                "target_digest": sha256_bytes(target.display().to_string().as_bytes()),
            }))
        }
        JobDelivery::Webhook { url } => Ok(json!({
            "mode": "webhook",
            "target_digest": sha256_bytes(url.as_bytes()),
        })),
    }
}

fn open_job_output(root: &Path, target: &Path) -> std::io::Result<std::fs::File> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "job output must stay beneath its directory",
        )
    };
    let relative = target.strip_prefix(root).map_err(|_| invalid())?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(invalid()),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let (name, parents) = components.split_last().ok_or_else(invalid)?;
    let mut directory = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    for parent in parents {
        directory = match directory.open_dir_nofollow(parent) {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match directory.create_dir(parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                directory.open_dir_nofollow(parent)?
            }
            Err(error) => return Err(error),
        };
    }
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .truncate(true)
        .follow(FollowSymlinks::No);
    Ok(directory.open_with(name, &options)?.into_std())
}

fn file_target(
    dashboard_root: &Path,
    job: &AutomationJob,
    run_id: &str,
    relative: Option<&str>,
) -> Result<std::path::PathBuf> {
    match relative {
        Some(relative) => {
            validate_relative_output_path(relative)?;
            Ok(dashboard_root.join(relative))
        }
        None => Ok(dashboard_root
            .join(JOB_OUTPUT_DIR)
            .join(&job.id)
            .join(format!("{run_id}.md"))),
    }
}

fn indeterminate_error(
    run_id: &str,
    task_key: &str,
    mode: &'static str,
    target_digest: &str,
    content_digest: &str,
) -> AutomationRunError {
    AutomationRunError::PartialEffect {
        run_id: run_id.to_owned(),
        committed_receipt: Box::new(indeterminate_delivery_receipt(
            run_id,
            task_key,
            mode,
            target_digest,
            content_digest,
        )),
        ledger_record: None,
        detail: "User job delivery was attempted but could not be proven uncommitted; reconcile the payload-free delivery receipt before retrying.",
    }
}

pub(super) fn after_delivery<T>(
    result: Result<T>,
    run_id: &str,
    receipt: AutomationCommittedReceipt,
    detail: &'static str,
) -> AutomationRunResult<T> {
    result.map_err(|_| AutomationRunError::PartialEffect {
        run_id: run_id.to_owned(),
        committed_receipt: Box::new(receipt),
        ledger_record: None,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_receipt_binds_run_task_and_target_without_retaining_target() {
        let target = format!("secret-{}", "x".repeat(32 * 1024));
        let content = format!("private-output-{}", "y".repeat(32 * 1024));
        let content_digest = sha256_bytes(content.as_bytes());
        let target_digest = sha256_bytes(target.as_bytes());
        let AutomationCommittedReceipt::UserJobDelivery(receipt) =
            file_delivery_receipt("run-7", "user_job:nightly", &target_digest, &content_digest)
        else {
            panic!("expected user-job delivery receipt");
        };

        assert_eq!(receipt.run_id(), "run-7");
        assert_eq!(receipt.task_key(), "user_job:nightly");
        let ExternalAutomationEffectDisposition::UserJobFileDelivered {
            target_digest,
            content_digest: receipt_content_digest,
        } = receipt.disposition()
        else {
            panic!("expected file delivery disposition");
        };
        assert_eq!(target_digest, &sha256_bytes(target.as_bytes()));
        assert_eq!(receipt_content_digest, &content_digest);
        assert!(!format!("{receipt:?}").contains("secret-"));
        assert!(!format!("{receipt:?}").contains("private-output-"));
        assert_eq!(receipt.manifest_digest().len(), 71);
    }

    #[test]
    fn webhook_receipt_binds_status_and_target_identity() {
        let content_digest = sha256_bytes(b"first output");
        let first = webhook_delivery_receipt(
            "run-8",
            "user_job:notify",
            &sha256_bytes(b"https://example.invalid/a"),
            202,
            &content_digest,
        );
        let second = webhook_delivery_receipt(
            "run-8",
            "user_job:notify",
            &sha256_bytes(b"https://example.invalid/b"),
            202,
            &content_digest,
        );

        assert_ne!(first, second);
        let AutomationCommittedReceipt::UserJobDelivery(receipt) = first else {
            panic!("expected user-job delivery receipt");
        };
        assert!(matches!(
            receipt.disposition(),
            ExternalAutomationEffectDisposition::UserJobWebhookDelivered { status: 202, .. }
        ));
    }

    #[test]
    fn same_delivery_target_with_different_content_has_a_different_receipt() {
        let first = webhook_delivery_receipt(
            "run-10",
            "user_job:notify",
            &sha256_bytes(b"https://example.invalid/delivery"),
            202,
            &sha256_bytes(b"first private output"),
        );
        let second = webhook_delivery_receipt(
            "run-10",
            "user_job:notify",
            &sha256_bytes(b"https://example.invalid/delivery"),
            202,
            &sha256_bytes(b"second private output"),
        );

        assert_ne!(first, second);
        let debug = format!("{first:?}{second:?}");
        assert!(!debug.contains("first private output"));
        assert!(!debug.contains("second private output"));
    }

    #[test]
    fn delivery_reports_and_indeterminate_receipts_never_retain_hostile_targets() {
        let target = "https://user:secret@example.invalid/private-hook?token=hostile";
        let target_digest = sha256_bytes(target.as_bytes());
        let job: AutomationJob = serde_json::from_value(json!({
            "id": "notify",
            "name": "Notify",
            "prompt": "send",
            "delivery": {"mode": "webhook", "url": target}
        }))
        .expect("job");
        let context = delivery_context(Path::new("/private/root"), &job, "run-hostile")
            .expect("delivery context");
        let context_text = serde_json::to_string(&context).expect("context json");
        assert!(context_text.contains(&target_digest));
        assert!(!context_text.contains(target));
        assert!(!context_text.contains("secret"));
        assert!(!context_text.contains("token="));
        let report = JobDeliveryOutcome::Webhook {
            target_digest: target_digest.clone(),
            status: 202,
        }
        .report();
        let report_text = serde_json::to_string(&report).expect("report json");
        assert!(report_text.contains(&target_digest));
        assert!(!report_text.contains(target));
        assert!(!report_text.contains("secret"));
        assert!(!report_text.contains("token="));

        let error = indeterminate_error(
            "run-hostile",
            "user_job:notify",
            "webhook",
            &target_digest,
            &sha256_bytes(b"private delivery body"),
        );
        let AutomationRunError::PartialEffect {
            committed_receipt,
            ledger_record,
            ..
        } = error
        else {
            panic!("attempted delivery must remain a typed partial effect")
        };
        let AutomationCommittedReceipt::UserJobDelivery(receipt) = *committed_receipt else {
            panic!("attempted delivery must carry a delivery receipt")
        };
        assert!(ledger_record.is_none());
        assert_eq!(receipt.run_id(), "run-hostile");
        assert_eq!(receipt.task_key(), "user_job:notify");
        let debug = format!("{receipt:?}");
        assert!(debug.contains(&target_digest));
        assert!(!debug.contains(target));
        assert!(!debug.contains("private delivery body"));
    }

    #[tokio::test]
    async fn file_open_failure_remains_runtime_without_a_committed_receipt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output_root = temp.path().join(JOB_OUTPUT_DIR);
        std::fs::create_dir_all(output_root.join("occupied")).expect("occupied target");
        let job: AutomationJob = serde_json::from_value(json!({
            "id": "private-file",
            "name": "Private file",
            "prompt": "write",
            "delivery": {"mode": "file", "path": "job-output/occupied"}
        }))
        .expect("job");
        let response = AgentTaskResponse {
            run_id: "run-file-attempt".to_owned(),
            task: crate::automation::backend::AgentTaskKind::UserJob,
            output_json: None,
            output_text: "private delivery body".to_owned(),
            model: None,
            provider: None,
            input_tokens: None,
            output_tokens: None,
        };
        let error = deliver_job_output(
            temp.path(),
            &job,
            "run-file-attempt",
            &sha256_bytes(response.output_text.as_bytes()),
            &response,
        )
        .await
        .expect_err("opening a directory as a delivery file must be unattempted");
        assert!(matches!(
            error,
            AutomationRunError::Runtime(TraceDecayError::Config { message })
                if message.starts_with("failed to open job output:")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn file_output_refuses_symlink_destinations_and_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("existing");
        std::fs::write(&target, b"preserve").expect("seed");
        std::os::unix::fs::symlink(&target, temp.path().join("sink")).expect("file symlink");
        assert!(super::open_job_output(temp.path(), &temp.path().join("sink")).is_err());
        std::os::unix::fs::symlink(outside.path(), temp.path().join("directory"))
            .expect("directory symlink");
        assert!(
            super::open_job_output(temp.path(), &temp.path().join("directory/existing")).is_err()
        );
        assert_eq!(std::fs::read(&target).expect("read"), b"preserve");
        let file = super::open_job_output(temp.path(), &temp.path().join("nested/output"))
            .expect("ordinary nested output");
        drop(file);
        assert!(temp.path().join("nested/output").is_file());
    }

    #[test]
    fn webhook_worker_panic_after_attempt_remains_a_bound_partial_effect() {
        let attempted = native_runtime::block_on(native_runtime::spawn_blocking(
            || -> std::result::Result<u16, job_webhook::WebhookPostError> {
                // The worker "attempted" its write; the panic lands afterwards.
                panic!("injected panic after write")
            },
        ))
        .expect("native runtime available");
        let error = settle_webhook_attempt(
            attempted,
            "run-webhook-panic",
            "user_job:notify",
            &sha256_bytes(b"https://secret.invalid/hook?token=private"),
            &sha256_bytes(b"private body"),
        )
        .expect_err("unknown worker phase must fail closed as indeterminate");
        let AutomationRunError::PartialEffect {
            committed_receipt,
            ledger_record,
            ..
        } = error
        else {
            panic!("worker panic must preserve an indeterminate receipt")
        };
        let AutomationCommittedReceipt::UserJobDelivery(receipt) = *committed_receipt else {
            panic!("worker panic must carry an indeterminate receipt")
        };
        assert!(ledger_record.is_none());
        assert_eq!(receipt.run_id(), "run-webhook-panic");
        assert_eq!(receipt.task_key(), "user_job:notify");
        let debug = format!("{receipt:?}");
        assert!(!debug.contains("secret.invalid"));
        assert!(!debug.contains("private body"));
    }

    #[test]
    fn skill_receipt_binds_exact_mutation_and_deployment_disposition() {
        let AutomationCommittedReceipt::SkillWriting(receipt) = skill_writing_receipt(
            "run-9",
            2,
            1,
            3,
            ExternalSkillDeploymentDisposition::PartialFailure,
            "sha256:mutation".to_string(),
        ) else {
            panic!("expected skill-writing receipt");
        };

        assert_eq!(receipt.run_id(), "run-9");
        assert_eq!(receipt.task_key(), "skill_writer");
        assert!(matches!(
            receipt.disposition(),
            ExternalAutomationEffectDisposition::SkillWriting {
                created_count: 2,
                updated_count: 1,
                consolidation_count: 3,
                deployment: ExternalSkillDeploymentDisposition::PartialFailure,
                mutation_manifest_digest,
            }
                if mutation_manifest_digest == "sha256:mutation"
        ));
    }
}
