//! One-lock snapshot of the project runtimes used by invocation dispatch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::daemon::service::invocation::{
    DaemonAdvisoryCycleInvocationOwner, DaemonFeedbackInvocationOwner, DaemonLspInvocationOwner,
    RegisteredConfigurationRuntime, RegisteredFeedbackRuntime, RegisteredRetainedRuntime,
    RegisteredWorkRuntime,
};
use tracedecay_usecases::feedback::concrete::FeedbackRuntime;

use super::{ProjectRuntimeRegistryV1, ProjectRuntimeRequestLeaseV1};

/// The per-project components one request may need, resolved together.
#[derive(Default)]
pub(in crate::daemon::service) struct ProjectRequestRuntimesV1 {
    _request_lease: Option<ProjectRuntimeRequestLeaseV1>,
    admitted: bool,
    pub(in crate::daemon::service) feedback: Option<Arc<FeedbackRuntime>>,
    pub(in crate::daemon::service) feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    pub(in crate::daemon::service) advisory_cycle: Option<DaemonAdvisoryCycleInvocationOwner>,
    pub(in crate::daemon::service) configuration: Option<RegisteredConfigurationRuntime>,
    pub(in crate::daemon::service) work: Option<RegisteredWorkRuntime>,
    pub(in crate::daemon::service) retained: Option<RegisteredRetainedRuntime>,
    pub(in crate::daemon::service) lsp_owner: Option<DaemonLspInvocationOwner>,
}

impl ProjectRuntimeRegistryV1 {
    pub(in crate::daemon) fn admit_request(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
    ) -> Option<ProjectRuntimeRequestLeaseV1> {
        let candidate_roots = candidate_roots(project_root, canonical_root);
        let mut fences = self.lock_root_fences();
        if self.closed.load(Ordering::Acquire)
            || candidate_roots.iter().any(|root| fences.contains(root))
        {
            return None;
        }
        let runtimes = self.lock_runtimes();
        if !candidate_roots
            .iter()
            .any(|root| runtimes.contains_key(root))
            || candidate_roots.iter().any(|root| {
                fences
                    .request_leases
                    .get(root)
                    .is_some_and(|count| *count == usize::MAX)
            })
        {
            return None;
        }
        for root in &candidate_roots {
            *fences.request_leases.entry(root.clone()).or_default() += 1;
        }
        drop(runtimes);
        drop(fences);
        Some(ProjectRuntimeRequestLeaseV1 {
            inner: Arc::new(super::ProjectRuntimeRequestLeaseInnerV1 {
                registry: self.clone(),
                roots: candidate_roots,
            }),
        })
    }

    /// Resolve all request runtimes from one consistent registry view.
    ///
    /// Only LSP retains canonical-root fallback because it is the sole runtime
    /// historically registered by either spelling of an opened root.
    pub(in crate::daemon::service) async fn request_runtimes(
        &self,
        project_root: Option<&Path>,
        canonical_root: Option<&Path>,
    ) -> ProjectRequestRuntimesV1 {
        let Some(project_root) = project_root else {
            return ProjectRequestRuntimesV1::default();
        };
        let Some(request_lease) = self.admit_request(project_root, canonical_root) else {
            return ProjectRequestRuntimesV1::default();
        };
        let runtimes = self.lock_runtimes();
        let runtime = runtimes.get(project_root);
        let feedback = runtime.and_then(|runtime| runtime.feedback.as_ref());
        let lsp_owner = Self::component_with_canonical_fallback::<DaemonLspInvocationOwner>(
            &runtimes,
            project_root,
            canonical_root,
        );
        let snapshot = ProjectRequestRuntimesV1 {
            _request_lease: Some(request_lease),
            admitted: true,
            feedback: feedback.map(RegisteredFeedbackRuntime::runtime),
            feedback_owner: feedback.map(RegisteredFeedbackRuntime::invocation_owner),
            advisory_cycle: runtime.and_then(|runtime| runtime.advisory_cycle.clone()),
            configuration: runtime.and_then(|runtime| runtime.configuration.clone()),
            work: runtime.and_then(|runtime| runtime.work.clone()),
            retained: runtime.and_then(|runtime| runtime.retained.clone()),
            lsp_owner,
        };
        drop(runtimes);
        snapshot
    }

    pub(in crate::daemon::service) fn request_runtimes_with_admission(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
        admission: &ProjectRuntimeRequestLeaseV1,
    ) -> ProjectRequestRuntimesV1 {
        if !admission.covers(self, project_root) {
            return ProjectRequestRuntimesV1::default();
        }
        let runtimes = self.lock_runtimes();
        let runtime = runtimes.get(project_root);
        let feedback = runtime.and_then(|runtime| runtime.feedback.as_ref());
        ProjectRequestRuntimesV1 {
            _request_lease: None,
            admitted: true,
            feedback: feedback.map(RegisteredFeedbackRuntime::runtime),
            feedback_owner: feedback.map(RegisteredFeedbackRuntime::invocation_owner),
            advisory_cycle: runtime.and_then(|runtime| runtime.advisory_cycle.clone()),
            configuration: runtime.and_then(|runtime| runtime.configuration.clone()),
            work: runtime.and_then(|runtime| runtime.work.clone()),
            retained: runtime.and_then(|runtime| runtime.retained.clone()),
            lsp_owner: Self::component_with_canonical_fallback::<DaemonLspInvocationOwner>(
                &runtimes,
                project_root,
                canonical_root,
            ),
        }
    }
}

fn candidate_roots(project_root: &Path, canonical_root: Option<&Path>) -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::from([project_root.to_path_buf()]);
    if let Some(canonical_root) = canonical_root {
        roots.insert(canonical_root.to_path_buf());
    }
    roots
}

impl ProjectRequestRuntimesV1 {
    pub(in crate::daemon::service) fn is_admitted(&self) -> bool {
        self.admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::service::invocation::{
        DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationPort,
        DaemonAdvisoryCycleInvocationRequest,
    };
    use tracedecay_application::{ApplicationProblem, SafeDiagnostic};
    use tracedecay_domain::ProjectId;

    struct UnavailableAdvisoryCycle;

    impl DaemonAdvisoryCycleInvocationPort for UnavailableAdvisoryCycle {
        fn invoke(
            &self,
            _request: DaemonAdvisoryCycleInvocationRequest,
        ) -> DaemonAdvisoryCycleInvocationFuture<'_> {
            Box::pin(async {
                Err(ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "feedback.test-advisory-owner".to_owned(),
                    message: "The test advisory owner is unavailable".to_owned(),
                }))
            })
        }
    }

    #[tokio::test]
    async fn request_snapshot_carries_the_exact_mounted_advisory_owner() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project_root = PathBuf::from("/projects/advisory-owner");
        let project_id = ProjectId::new("project.advisory-owner").expect("project id");
        let owner = DaemonAdvisoryCycleInvocationOwner::new(
            project_id.clone(),
            Arc::new(UnavailableAdvisoryCycle),
        );
        registry
            .register(project_root.clone(), owner)
            .await
            .expect("advisory owner registration");

        let snapshot = registry.request_runtimes(Some(&project_root), None).await;

        let mounted = snapshot
            .advisory_cycle
            .expect("mounted advisory owner must be in the request snapshot");
        assert_eq!(mounted.project_id, project_id);
    }
}
