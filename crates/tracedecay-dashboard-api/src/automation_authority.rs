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
use tracedecay_agent_hosts::ports::session_evidence::{LcmGrepSort, LcmScope};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardAutomationAuthorityErrorV1 {
    Unavailable { detail: String },
    Denied { detail: String },
    Invalid { detail: String },
    NotFound { detail: String },
    Conflict { detail: String },
    Failed { detail: String },
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
        }
    }
}

pub(crate) fn automation_authority_error_response(
    error: DashboardAutomationAuthorityErrorV1,
) -> (StatusCode, Json<Value>) {
    (
        error.status_code(),
        Json(super::util::http_detail(error.detail())),
    )
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
    MemoryCurator {
        max_clusters: usize,
        min_confidence: f64,
    },
    SessionReflection {
        provider: Option<String>,
        query: Option<String>,
        evidence_limit: Option<usize>,
        scope: Option<LcmScope>,
        session_id: Option<String>,
        include_summaries: Option<bool>,
        sort: Option<LcmGrepSort>,
        source: Option<String>,
        role: Option<String>,
        start_time: Option<i64>,
        end_time: Option<i64>,
    },
    SkillWriting {
        provider: Option<String>,
        query: Option<String>,
        evidence_limit: Option<usize>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DashboardAutomationRunInvocationV1 {
    pub project_root: PathBuf,
    pub request: DashboardAutomationRunRequestV1,
}

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
    Box<dyn Future<Output = Result<Value, DashboardAutomationAuthorityErrorV1>> + Send + 'static>,
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
    ) -> Result<Value, DashboardAutomationAuthorityErrorV1> {
        if !project_root.is_absolute() {
            return Err(DashboardAutomationAuthorityErrorV1::unavailable(
                "dashboard automation project authority must be an absolute path",
            ));
        }
        (self.run)(DashboardAutomationRunInvocationV1 {
            project_root: project_root.to_path_buf(),
            request,
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
                DashboardAutomationRunRequestV1::SkillWriting {
                    provider: None,
                    query: None,
                    evidence_limit: None,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(DashboardAutomationAuthorityErrorV1::Unavailable { .. })
        ));
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
}
