//! One-lock snapshot of the project runtimes used by invocation dispatch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::invocation::{
    DaemonAdvisoryCycleInvocationOwner, DaemonFeedbackInvocationOwner, DaemonLspInvocationOwner,
    RegisteredConfigurationRuntime, RegisteredFeedbackRuntime, RegisteredRetainedRuntime,
    RegisteredWorkRuntime,
};
use tracedecay_usecases::feedback::concrete::FeedbackRuntime;

use super::{ProjectRuntimeRegistryV1, ProjectRuntimeRequestLeaseV1};

/// The per-project components one request may need, resolved together.
///
/// This is the typed readiness result for an admitted project request: the
/// exact registered root, the owner-publication stage, and the owners that
/// were published under that root. A request spelling that only matches the
/// canonical or Windows-verbatim key still names that same result.
#[derive(Default)]
pub struct ProjectRequestRuntimesV1 {
    _request_lease: Option<ProjectRuntimeRequestLeaseV1>,
    admitted: bool,
    pub resolved_root: Option<PathBuf>,
    pub publication: Option<super::ProjectRuntimePublicationStateV1>,
    pub feedback: Option<Arc<FeedbackRuntime>>,
    pub feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    pub advisory_cycle: Option<DaemonAdvisoryCycleInvocationOwner>,
    pub configuration: Option<RegisteredConfigurationRuntime>,
    pub work: Option<RegisteredWorkRuntime>,
    pub retained: Option<RegisteredRetainedRuntime>,
    pub lsp_owner: Option<DaemonLspInvocationOwner>,
}

impl ProjectRuntimeRegistryV1 {
    pub fn admit_request(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
    ) -> Option<ProjectRuntimeRequestLeaseV1> {
        let candidate_roots = candidate_roots(project_root, canonical_root);
        let mut fences = self.lock_root_fences();
        if self.closed.load(Ordering::Acquire) {
            hotpath::gauge!("daemon.service.request_admission.closed_total").inc(1_u64);
            tracing::warn!(
                event = "project_request_admission",
                outcome = "unavailable",
                reason = "registry_closed",
                "project request runtime registry is closed"
            );
            return None;
        }
        if candidate_roots.iter().any(|root| fences.contains(root)) {
            hotpath::gauge!("daemon.service.request_admission.fenced_total").inc(1_u64);
            tracing::warn!(
                event = "project_request_admission",
                outcome = "unavailable",
                reason = "root_fenced",
                "project request root is fenced"
            );
            return None;
        }
        let runtimes = self.lock_runtimes();
        let Some(resolved_root) =
            super::resolved_runtime_key(&runtimes, project_root, canonical_root)
        else {
            hotpath::gauge!("daemon.service.request_admission.runtime_missing_total").inc(1_u64);
            tracing::warn!(
                event = "project_request_admission",
                outcome = "unavailable",
                reason = "runtime_missing",
                "project request runtime is not registered"
            );
            return None;
        };
        if fences.contains(&resolved_root) {
            hotpath::gauge!("daemon.service.request_admission.fenced_total").inc(1_u64);
            tracing::warn!(
                event = "project_request_admission",
                outcome = "unavailable",
                reason = "root_fenced",
                "project request root is fenced"
            );
            return None;
        }
        if candidate_roots.iter().any(|root| {
            fences
                .request_leases
                .get(root)
                .is_some_and(|count| *count == usize::MAX)
        }) {
            hotpath::gauge!("daemon.service.request_admission.lease_overflow_total").inc(1_u64);
            tracing::warn!(
                event = "project_request_admission",
                outcome = "unavailable",
                reason = "lease_overflow",
                "project request lease counter is exhausted"
            );
            return None;
        }
        let mut lease_roots = candidate_roots;
        lease_roots.insert(resolved_root);
        for root in &lease_roots {
            *fences.request_leases.entry(root.clone()).or_default() += 1;
        }
        drop(runtimes);
        drop(fences);
        hotpath::gauge!("daemon.service.request_in_flight").inc(1.0);
        Some(ProjectRuntimeRequestLeaseV1 {
            inner: Arc::new(super::ProjectRuntimeRequestLeaseInnerV1 {
                registry: self.clone(),
                roots: lease_roots,
                canonical_root: canonical_root.map(Path::to_path_buf),
            }),
        })
    }

    /// Resolve all request runtimes from one consistent registry view.
    ///
    /// Owners are keyed by the registered root. A request spelling that only
    /// matches through canonicalize, the admitted canonical root, or the
    /// Windows verbatim/ordinary pair still resolves that same entry.
    #[hotpath::skip]
    pub async fn request_runtimes(
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
        self.snapshot_request_runtimes(project_root, canonical_root, Some(request_lease))
    }

    pub fn request_runtimes_with_admission(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
        admission: &ProjectRuntimeRequestLeaseV1,
    ) -> ProjectRequestRuntimesV1 {
        if !admission.covers(self, project_root) {
            return ProjectRequestRuntimesV1::default();
        }
        self.snapshot_request_runtimes(project_root, canonical_root, None)
    }

    fn snapshot_request_runtimes(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
        request_lease: Option<ProjectRuntimeRequestLeaseV1>,
    ) -> ProjectRequestRuntimesV1 {
        let runtimes = self.lock_runtimes();
        let resolved_root = super::resolved_runtime_key(&runtimes, project_root, canonical_root);
        let runtime = resolved_root.as_ref().and_then(|root| runtimes.get(root));
        let feedback = runtime.and_then(|runtime| runtime.feedback.as_ref());
        let publication = runtime.map(|runtime| runtime.publication);
        let lsp_owner = Self::component_with_canonical_fallback::<DaemonLspInvocationOwner>(
            &runtimes,
            project_root,
            canonical_root,
        );
        ProjectRequestRuntimesV1 {
            _request_lease: request_lease,
            admitted: true,
            resolved_root,
            publication,
            feedback: feedback.map(RegisteredFeedbackRuntime::runtime),
            feedback_owner: feedback.map(RegisteredFeedbackRuntime::invocation_owner),
            advisory_cycle: runtime.and_then(|runtime| runtime.advisory_cycle.clone()),
            configuration: runtime.and_then(|runtime| runtime.configuration.clone()),
            work: runtime.and_then(|runtime| runtime.work.clone()),
            retained: runtime.and_then(|runtime| runtime.retained.clone()),
            lsp_owner,
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
    pub fn is_admitted(&self) -> bool {
        self.admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invocation::{
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

    #[tokio::test]
    async fn request_snapshot_resolves_owners_from_the_admitted_canonical_root() {
        let registry = ProjectRuntimeRegistryV1::default();
        let alias = PathBuf::from("/projects/storage-status-alias");
        let canonical = PathBuf::from("/projects/storage-status-canonical");
        let project_id = ProjectId::new("project.storage-status-alias").expect("project id");
        let owner = DaemonAdvisoryCycleInvocationOwner::new(
            project_id.clone(),
            Arc::new(UnavailableAdvisoryCycle),
        );
        registry
            .register(canonical.clone(), owner)
            .await
            .expect("canonical owner registration");

        let snapshot = registry
            .request_runtimes(Some(&alias), Some(&canonical))
            .await;

        assert!(
            snapshot.is_admitted(),
            "admission already accepts the alias plus canonical candidate set"
        );
        assert_eq!(
            snapshot.resolved_root.as_deref(),
            Some(canonical.as_path()),
            "the typed readiness result must carry the registered root, not only the request spelling"
        );
        assert_eq!(
            snapshot.publication,
            Some(crate::ProjectRuntimePublicationStateV1::Warming)
        );
        let mounted = snapshot.advisory_cycle.expect(
            "an owner registered under the admitted canonical root must be callable through the request spelling",
        );
        assert_eq!(mounted.project_id, project_id);
    }
}
