//! Daemon-owned automation capabilities retained by the dashboard adapter.
//!
//! The HTTP surface carries typed commands and projects their outcomes. It
//! never resolves an ambient profile, materializes host skills, or starts its
//! own automation executor.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use serde_json::Value;
use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkill, ManagedSkillDraft, ManagedSkillUpdate,
};
use tracedecay_agent_hosts::automation::skill_writer::ManagedSkillDeploymentReceipt;
use tracedecay_application::ApplicationProblemEnvelope;
use tracedecay_application::retained_surfaces::{AutomationRunProblemV1, AutomationRunResultV1};

use super::DashboardHttpRequestControlV1;

#[derive(Clone, Debug, PartialEq)]
pub enum DashboardAutomationAuthorityErrorV1 {
    Unavailable { detail: String },
    Denied { detail: String },
    Invalid { detail: String },
    NotFound { detail: String },
    Conflict { detail: String },
    Failed { detail: String },
    ApplicationProblem(ApplicationProblemEnvelope),
    AutomationProblem(Box<AutomationRunProblemV1>),
}

impl DashboardAutomationAuthorityErrorV1 {
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::Unavailable {
            detail: detail.into(),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Unavailable { detail }
            | Self::Denied { detail }
            | Self::Invalid { detail }
            | Self::NotFound { detail }
            | Self::Conflict { detail }
            | Self::Failed { detail } => detail,
            Self::ApplicationProblem(problem) => &problem.problem.message,
            Self::AutomationProblem(problem) => &problem.problem.problem.message,
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Denied { .. } => StatusCode::FORBIDDEN,
            Self::Invalid { .. } => StatusCode::BAD_REQUEST,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Failed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ApplicationProblem(problem) => application_problem_status(problem),
            Self::AutomationProblem(problem) => application_problem_status(&problem.problem),
        }
    }
}

fn application_problem_status(problem: &ApplicationProblemEnvelope) -> StatusCode {
    match problem.problem.kind() {
        tracedecay_application::ApplicationProblemKind::PartialEffect
        | tracedecay_application::ApplicationProblemKind::Conflict
        | tracedecay_application::ApplicationProblemKind::Stale => StatusCode::CONFLICT,
        tracedecay_application::ApplicationProblemKind::InvalidRequest => StatusCode::BAD_REQUEST,
        tracedecay_application::ApplicationProblemKind::NotFoundOrNotAuthorized => {
            StatusCode::NOT_FOUND
        }
        tracedecay_application::ApplicationProblemKind::Unsupported => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        tracedecay_application::ApplicationProblemKind::ResetRequired
        | tracedecay_application::ApplicationProblemKind::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        tracedecay_application::ApplicationProblemKind::ExecutionFailed => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        tracedecay_application::ApplicationProblemKind::Saturated => StatusCode::TOO_MANY_REQUESTS,
        tracedecay_application::ApplicationProblemKind::Cancelled => StatusCode::REQUEST_TIMEOUT,
        tracedecay_application::ApplicationProblemKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
    }
}

pub(crate) fn automation_authority_error_response(
    error: DashboardAutomationAuthorityErrorV1,
) -> (StatusCode, Json<Value>) {
    let status = error.status_code();
    let payload = match error {
        DashboardAutomationAuthorityErrorV1::ApplicationProblem(problem) => {
            serde_json::json!({ "kind": "problem", "value": problem })
        }
        DashboardAutomationAuthorityErrorV1::AutomationProblem(problem) => {
            serde_json::json!({ "kind": "problem", "value": problem })
        }
        error => super::util::http_detail(error.detail()),
    };
    (status, Json(payload))
}

pub(crate) fn exact_automation_authority(
    state: &super::DashboardState,
) -> Result<&DashboardAutomationAuthorityV1, DashboardAutomationAuthorityErrorV1> {
    let authority = state.automation_authority.as_ref().ok_or_else(|| {
        DashboardAutomationAuthorityErrorV1::unavailable(
            "dashboard automation authority is not mounted",
        )
    })?;
    Ok(authority)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DashboardAutomationRunRequestV1 {
    UserJob { job_id: String, run_id: String },
}

#[derive(Clone, Debug)]
pub struct DashboardAutomationRunInvocationV1 {
    pub project_root: PathBuf,
    pub request: DashboardAutomationRunRequestV1,
    pub control: DashboardHttpRequestControlV1,
}

pub type DashboardAutomationRunOutcomeV1 = AutomationRunResultV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardManagedSkillCommandV1 {
    Create {
        draft: ManagedSkillDraft,
        pinned: Option<bool>,
    },
    Update {
        id: String,
        base_checksum: String,
        update: ManagedSkillUpdate,
    },
    Disable {
        id: String,
    },
    Archive {
        id: String,
    },
    Restore {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardManagedSkillCommandOutcomeV1 {
    pub skill: ManagedSkill,
    pub deployment: ManagedSkillDeploymentReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardManagedSkillCommandInvocationV1 {
    pub project_root: PathBuf,
    pub command: DashboardManagedSkillCommandV1,
}

pub type DashboardAutomationRunFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<
                    DashboardAutomationRunOutcomeV1,
                    DashboardAutomationAuthorityErrorV1,
                >,
            > + Send
            + 'static,
    >,
>;
pub type DashboardAutomationRunPortV1 = Arc<
    dyn Fn(DashboardAutomationRunInvocationV1) -> DashboardAutomationRunFutureV1
        + Send
        + Sync
        + 'static,
>;

pub type DashboardManagedSkillCommandFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<
                    DashboardManagedSkillCommandOutcomeV1,
                    DashboardAutomationAuthorityErrorV1,
                >,
            > + Send
            + 'static,
    >,
>;
pub type DashboardManagedSkillCommandPortV1 = Arc<
    dyn Fn(DashboardManagedSkillCommandInvocationV1) -> DashboardManagedSkillCommandFutureV1
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone)]
pub struct DashboardAutomationAuthorityV1 {
    profile_root: PathBuf,
    run: DashboardAutomationRunPortV1,
    managed_skill_command: DashboardManagedSkillCommandPortV1,
}

impl DashboardAutomationAuthorityV1 {
    pub fn new(
        profile_root: PathBuf,
        run: DashboardAutomationRunPortV1,
        managed_skill_command: DashboardManagedSkillCommandPortV1,
    ) -> Result<Self, DashboardAutomationAuthorityErrorV1> {
        if !profile_root.is_absolute() {
            return Err(DashboardAutomationAuthorityErrorV1::unavailable(
                "dashboard automation profile authority must be an absolute path",
            ));
        }
        Ok(Self {
            profile_root,
            run,
            managed_skill_command,
        })
    }

    pub fn profile_root(&self) -> &std::path::Path {
        &self.profile_root
    }

    pub async fn run(
        &self,
        project_root: &std::path::Path,
        request: DashboardAutomationRunRequestV1,
        control: DashboardHttpRequestControlV1,
    ) -> Result<DashboardAutomationRunOutcomeV1, DashboardAutomationAuthorityErrorV1> {
        if !project_root.is_absolute() {
            return Err(DashboardAutomationAuthorityErrorV1::unavailable(
                "dashboard automation project authority must be an absolute path",
            ));
        }
        (self.run)(DashboardAutomationRunInvocationV1 {
            project_root: project_root.to_path_buf(),
            request,
            control,
        })
        .await
    }

    pub async fn execute_managed_skill_command(
        &self,
        project_root: &std::path::Path,
        command: DashboardManagedSkillCommandV1,
    ) -> Result<DashboardManagedSkillCommandOutcomeV1, DashboardAutomationAuthorityErrorV1> {
        if !project_root.is_absolute() {
            return Err(DashboardAutomationAuthorityErrorV1::unavailable(
                "dashboard automation project authority must be an absolute path",
            ));
        }
        (self.managed_skill_command)(DashboardManagedSkillCommandInvocationV1 {
            project_root: project_root.to_path_buf(),
            command,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_control() -> DashboardHttpRequestControlV1 {
        let observed_at = tracedecay_domain::UtcMicros(1_000_000);
        DashboardHttpRequestControlV1 {
            request_id: tracedecay_application::RequestId::new("request.dashboard-automation-test")
                .expect("request identity"),
            deadline: tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                2_000_000,
            ))
            .expect("request deadline"),
            cancellation: tracedecay_application::CancellationSignal::active(
                "cancel.dashboard-automation-test",
            )
            .expect("request cancellation"),
            observed_at,
        }
    }

    fn unavailable_run_port() -> DashboardAutomationRunPortV1 {
        Arc::new(|_| {
            Box::pin(async {
                Err(DashboardAutomationAuthorityErrorV1::unavailable(
                    "test run authority",
                ))
            })
        })
    }

    fn unavailable_skill_port() -> DashboardManagedSkillCommandPortV1 {
        Arc::new(|_| {
            Box::pin(async {
                Err(DashboardAutomationAuthorityErrorV1::unavailable(
                    "test skill authority",
                ))
            })
        })
    }

    #[test]
    fn automation_authority_rejects_a_relative_profile_root() {
        let result = DashboardAutomationAuthorityV1::new(
            PathBuf::from("ambient-profile"),
            unavailable_run_port(),
            unavailable_skill_port(),
        );

        assert!(matches!(
            result,
            Err(DashboardAutomationAuthorityErrorV1::Unavailable { .. })
        ));
    }

    #[test]
    fn automation_authority_preserves_the_exact_selected_profile_root() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\profiles\selected")
        } else {
            PathBuf::from("/profiles/selected")
        };
        let authority = DashboardAutomationAuthorityV1::new(
            root.clone(),
            unavailable_run_port(),
            unavailable_skill_port(),
        )
        .expect("absolute selected profile root");

        assert_eq!(authority.profile_root(), root);
    }

    #[tokio::test]
    async fn automation_authority_rejects_a_relative_selected_project_root() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\profiles\selected")
        } else {
            PathBuf::from("/profiles/selected")
        };
        let authority = DashboardAutomationAuthorityV1::new(
            root,
            unavailable_run_port(),
            unavailable_skill_port(),
        )
        .expect("absolute selected profile root");

        let result = authority
            .run(
                std::path::Path::new("ambient-project"),
                DashboardAutomationRunRequestV1::UserJob {
                    job_id: "nightly-summary".to_owned(),
                    run_id: "dashboard_user_job_nightly-summary_1000000".to_owned(),
                },
                request_control(),
            )
            .await;

        assert!(matches!(
            result,
            Err(DashboardAutomationAuthorityErrorV1::Unavailable { .. })
        ));
    }

    #[tokio::test]
    async fn automation_authority_passes_only_user_job_identity_to_the_daemon_port() {
        let observed = Arc::new(std::sync::Mutex::new(None));
        let observed_run = Arc::clone(&observed);
        let run: DashboardAutomationRunPortV1 = Arc::new(move |invocation| {
            *observed_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(invocation.request);
            Box::pin(async {
                Err(DashboardAutomationAuthorityErrorV1::unavailable(
                    "test user-job authority",
                ))
            })
        });
        let profile_root = if cfg!(windows) {
            PathBuf::from(r"C:\profiles\selected")
        } else {
            PathBuf::from("/profiles/selected")
        };
        let project_root = if cfg!(windows) {
            PathBuf::from(r"C:\projects\selected")
        } else {
            PathBuf::from("/projects/selected")
        };
        let authority =
            DashboardAutomationAuthorityV1::new(profile_root, run, unavailable_skill_port())
                .expect("absolute selected profile root");

        let _ = authority
            .run(
                &project_root,
                DashboardAutomationRunRequestV1::UserJob {
                    job_id: "nightly-summary".to_owned(),
                    run_id: "dashboard_user_job_nightly-summary_1000000".to_owned(),
                },
                request_control(),
            )
            .await;

        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            Some(DashboardAutomationRunRequestV1::UserJob {
                job_id: "nightly-summary".to_owned(),
                run_id: "dashboard_user_job_nightly-summary_1000000".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn automation_authority_delivers_the_same_live_request_signal_to_the_run_port() {
        let observed = Arc::new(std::sync::Mutex::new(None));
        let observed_run = Arc::clone(&observed);
        let run: DashboardAutomationRunPortV1 = Arc::new(move |invocation| {
            *observed_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(invocation.control.cancellation().clone());
            Box::pin(async {
                serde_json::from_value(serde_json::json!({
                    "run_id": "dashboard_user_job_nightly-summary_1000000",
                    "task": "user_job",
                    "request_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "terminal": {
                        "status": "completed",
                        "summary": {
                            "reviewed_count": 0,
                            "accepted_count": 0,
                            "rejected_count": 0,
                            "skipped_count": 0
                        }
                    },
                    "committed_receipts": []
                }))
                .map_err(|error| DashboardAutomationAuthorityErrorV1::Failed {
                    detail: error.to_string(),
                })
            })
        });
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\profiles\selected")
        } else {
            PathBuf::from("/profiles/selected")
        };
        let project_root = if cfg!(windows) {
            PathBuf::from(r"C:\projects\selected")
        } else {
            PathBuf::from("/projects/selected")
        };
        let authority = DashboardAutomationAuthorityV1::new(root, run, unavailable_skill_port())
            .expect("absolute selected profile root");
        let control = request_control();

        authority
            .run(
                &project_root,
                DashboardAutomationRunRequestV1::UserJob {
                    job_id: "nightly-summary".to_owned(),
                    run_id: "dashboard_user_job_nightly-summary_1000000".to_owned(),
                },
                control.clone(),
            )
            .await
            .expect("run authority result");

        let observed_signal = observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("run port cancellation signal");
        assert!(!observed_signal.is_cancelled());
        assert!(
            control
                .cancellation()
                .cancel(tracedecay_domain::UtcMicros(1_500_000))
        );
        assert!(observed_signal.is_cancelled());
    }

    #[test]
    fn authority_denial_remains_forbidden_at_the_http_boundary() {
        let (status, Json(payload)) =
            automation_authority_error_response(DashboardAutomationAuthorityErrorV1::Denied {
                detail: "automation policy denied this command".to_owned(),
            });

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            payload["detail"],
            serde_json::json!("automation policy denied this command")
        );
    }

    #[test]
    fn invalid_commands_and_missing_targets_keep_distinct_http_states() {
        let (invalid_status, _) =
            automation_authority_error_response(DashboardAutomationAuthorityErrorV1::Invalid {
                detail: "managed skill draft is invalid".to_owned(),
            });
        let (missing_status, _) =
            automation_authority_error_response(DashboardAutomationAuthorityErrorV1::NotFound {
                detail: "managed skill was not found".to_owned(),
            });

        assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
        assert_eq!(missing_status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn admitted_execution_failure_is_an_internal_server_error() {
        let request_id =
            tracedecay_application::RequestId::new("request.dashboard-automation-execution-failed")
                .expect("request identity");
        let problem = tracedecay_application::ApplicationProblem::execution_failed(
            tracedecay_application::ApplicationExecutionFailureClassV1::Permanent,
            tracedecay_application::SafeDiagnostic::new(
                "application.dashboard-automation.execution-failed",
                "The admitted dashboard automation execution failed.",
            )
            .expect("safe execution-failure diagnostic"),
        )
        .expect("execution-failure problem");
        let envelope = tracedecay_application::ApplicationProblemEnvelope::new(
            tracedecay_application::ResultContractRef::new(
                tracedecay_tool_catalog::SchemaId::new(
                    "schema.dashboard-automation.execution-failed-result",
                )
                .expect("result schema identity"),
                1,
            )
            .expect("result contract"),
            request_id,
            problem,
        )
        .expect("execution-failure envelope");

        let (status, Json(payload)) = automation_authority_error_response(
            DashboardAutomationAuthorityErrorV1::ApplicationProblem(envelope),
        );

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(payload["kind"], "problem");
        assert_eq!(payload["value"]["problem"]["kind"], "execution_failed");
    }
}
