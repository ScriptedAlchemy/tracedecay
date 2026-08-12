//! Mounted ignored-dependency admission for one exact daemon project scope.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_application::{RequestAdmission, RequestContext, ResolvedScope};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_usecases::{
    code_index::{
        CodeIndexIgnoredDependencyAdmissionErrorV1, CodeIndexIgnoredDependencyAdmissionFutureV1,
        CodeIndexIgnoredDependencyAdmissionPortV1, CodeIndexIgnoredDependencyAdmissionRequestV1,
    },
    context::application_observed_at,
};

use crate::daemon::code_index_scheduler::{
    CodeIndexIgnoredDependencyRefusalV1, CodeIndexIgnoredDependencyRequestV1,
    CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
};

struct ProjectCodeIndexIgnoredDependencyAdmissionPortV1 {
    schedulers: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
    database_writable: bool,
}

struct RequestContextOnlyCodeIndexControlV1<'a> {
    context: &'a RequestContext,
}

impl CodeIndexExecutionControlV1 for RequestContextOnlyCodeIndexControlV1<'_> {
    fn is_cancelled(&self) -> bool {
        matches!(
            self.context.admission_at(application_observed_at()),
            RequestAdmission::Cancelled
        )
    }

    fn is_deadline_exceeded(&self) -> bool {
        matches!(
            self.context.admission_at(application_observed_at()),
            RequestAdmission::TimedOut
        )
    }
}

impl CodeIndexIgnoredDependencyAdmissionPortV1
    for ProjectCodeIndexIgnoredDependencyAdmissionPortV1
{
    fn admit<'a>(
        &'a self,
        request: CodeIndexIgnoredDependencyAdmissionRequestV1<'a>,
    ) -> CodeIndexIgnoredDependencyAdmissionFutureV1<'a> {
        Box::pin(async move {
            request.context().validate().map_err(|error| {
                CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
                    detail: format!("ignored-dependency request context is invalid: {error}"),
                }
            })?;
            if request.context().scope() != &self.scope {
                return Err(CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
                    detail:
                        "ignored-dependency request scope is outside the mounted project binding"
                            .to_owned(),
                });
            }
            if !self.database_writable {
                return Err(CodeIndexIgnoredDependencyAdmissionErrorV1::ReadOnly);
            }
            let project_root = self.project_root.canonicalize().map_err(|error| {
                CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
                    detail: format!("ignored-dependency project root is unavailable: {error}"),
                }
            })?;
            let scheduler_request = CodeIndexIgnoredDependencyRequestV1 {
                scope: self.scope.clone(),
                expected_generation: request.source_generation().clone(),
                verified_imports: request.imports().to_vec(),
            };
            let control: Arc<dyn CodeIndexExecutionControlV1 + Send + Sync + '_> =
                Arc::new(RequestContextOnlyCodeIndexControlV1 {
                    context: request.context(),
                });
            if control.is_cancelled() {
                return Err(CodeIndexIgnoredDependencyAdmissionErrorV1::Cancelled);
            }
            if control.is_deadline_exceeded() {
                return Err(CodeIndexIgnoredDependencyAdmissionErrorV1::TimedOut);
            }
            match self
                .schedulers
                .index_verified_ignored_dependency(&project_root, scheduler_request, control)
                .await
            {
                Ok(outcome) => Ok(outcome.generation_id),
                Err(error) => Err(map_ignored_dependency_scheduler_error(
                    &self.schedulers,
                    &project_root,
                    &self.scope,
                    error,
                )
                .await),
            }
        })
    }
}

async fn map_ignored_dependency_scheduler_error(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    error: CodeIndexSchedulerErrorV1,
) -> CodeIndexIgnoredDependencyAdmissionErrorV1 {
    match error {
        CodeIndexSchedulerErrorV1::IgnoredDependency(
            CodeIndexIgnoredDependencyRefusalV1::Cancelled,
        ) => CodeIndexIgnoredDependencyAdmissionErrorV1::Cancelled,
        CodeIndexSchedulerErrorV1::IgnoredDependency(
            CodeIndexIgnoredDependencyRefusalV1::DeadlineExceeded,
        ) => CodeIndexIgnoredDependencyAdmissionErrorV1::TimedOut,
        CodeIndexSchedulerErrorV1::IgnoredDependency(
            CodeIndexIgnoredDependencyRefusalV1::StaleGeneration,
        ) => match exact_serving_generation(schedulers, project_root, scope).await {
            Some(active_generation) => {
                CodeIndexIgnoredDependencyAdmissionErrorV1::Stale { active_generation }
            }
            None => CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
                detail: "ignored-dependency admission found no exact-scope serving generation"
                    .to_owned(),
            },
        },
        error => CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
            detail: error.to_string(),
        },
    }
}

async fn exact_serving_generation(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
) -> Option<tracedecay_domain::CodeGenerationId> {
    let serving = schedulers.serving_code_scope(project_root).await?;
    if serving.repository_id != scope.repository_id || serving.worktree_id != scope.worktree_id {
        return None;
    }
    let generation = serving.serving_generation?;
    let snapshot = generation.snapshot();
    (generation.manifest().project_id == scope.project_id
        && snapshot.repository == scope.repository_id
        && snapshot.worktree.as_ref() == Some(&scope.worktree_id)
        && snapshot.reference == scope.reference)
        .then(|| generation.manifest().generation_id.clone())
}

pub(crate) fn project_code_index_ignored_dependency_admission_port(
    schedulers: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
    database_writable: bool,
) -> Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1> {
    Arc::new(ProjectCodeIndexIgnoredDependencyAdmissionPortV1 {
        schedulers,
        project_root,
        scope,
        database_writable,
    })
}
