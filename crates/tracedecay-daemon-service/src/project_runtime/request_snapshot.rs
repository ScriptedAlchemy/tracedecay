//! One-lock snapshot of the project runtimes used by invocation dispatch.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::invocation::{
    DaemonAdvisoryCycleInvocationOwner, DaemonFeedbackInvocationOwner, DaemonLspInvocationOwner,
    RegisteredConfigurationRuntime, RegisteredFeedbackRuntime, RegisteredRetainedRuntime,
    RegisteredWorkRuntime,
};
use tracedecay_application::feedback::concrete::FeedbackRuntime;

use super::{
    ProjectRuntime, ProjectRuntimePublicationStateV1, ProjectRuntimeRegistryV1,
    ProjectRuntimeRequestLeaseV1, ProjectRuntimeResolutionV1,
};

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
    pub publication: Option<ProjectRuntimePublicationStateV1>,
    pub feedback: Option<Arc<FeedbackRuntime>>,
    pub feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    pub advisory_cycle: Option<DaemonAdvisoryCycleInvocationOwner>,
    pub configuration: Option<RegisteredConfigurationRuntime>,
    pub work: Option<RegisteredWorkRuntime>,
    pub retained: Option<RegisteredRetainedRuntime>,
    pub lsp_owner: Option<DaemonLspInvocationOwner>,
    pub source_edit: Option<Arc<crate::project_owner_registration::ProjectSourceEditOwnerV1>>,
}

/// The owners of the registered runtime as they stood under the admission
/// lock. Every snapshot taken from one lease serves exactly these.
pub(super) struct AdmittedProjectRuntimeV1 {
    publication: ProjectRuntimePublicationStateV1,
    feedback: Option<Arc<FeedbackRuntime>>,
    feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    advisory_cycle: Option<DaemonAdvisoryCycleInvocationOwner>,
    configuration: Option<RegisteredConfigurationRuntime>,
    work: Option<RegisteredWorkRuntime>,
    retained: Option<RegisteredRetainedRuntime>,
    lsp_owner: Option<DaemonLspInvocationOwner>,
    source_edit: Option<Arc<crate::project_owner_registration::ProjectSourceEditOwnerV1>>,
}

impl AdmittedProjectRuntimeV1 {
    fn capture(runtime: &ProjectRuntime) -> Self {
        let feedback = runtime.feedback.as_ref();
        Self {
            publication: runtime.publication,
            feedback: feedback.map(RegisteredFeedbackRuntime::runtime),
            feedback_owner: feedback.map(RegisteredFeedbackRuntime::invocation_owner),
            advisory_cycle: runtime.advisory_cycle.clone(),
            configuration: runtime.configuration.clone(),
            work: runtime.work.clone(),
            retained: runtime.retained.clone(),
            lsp_owner: runtime.lsp_owner.clone(),
            source_edit: runtime.source_edit.clone(),
        }
    }
}

impl ProjectRuntimeRegistryV1 {
    pub fn admit_request(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
    ) -> Option<ProjectRuntimeRequestLeaseV1> {
        let candidate_roots = super::candidate_request_roots(project_root, canonical_root);
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
        // The one resolution for this request: the key it counts the lease
        // on and the owners it will serve are read from the same entry under
        // the same lock.
        let (resolved_root, admitted) =
            match super::resolve_runtime(&runtimes, project_root, canonical_root) {
                ProjectRuntimeResolutionV1::Unique(root, runtime) => (
                    root.to_path_buf(),
                    AdmittedProjectRuntimeV1::capture(runtime),
                ),
                ProjectRuntimeResolutionV1::Missing => {
                    hotpath::gauge!("daemon.service.request_admission.runtime_missing_total")
                        .inc(1_u64);
                    tracing::warn!(
                        event = "project_request_admission",
                        outcome = "unavailable",
                        reason = "runtime_missing",
                        "project request runtime is not registered"
                    );
                    return None;
                }
                ProjectRuntimeResolutionV1::Ambiguous => {
                    hotpath::gauge!("daemon.service.request_admission.runtime_ambiguous_total")
                        .inc(1_u64);
                    tracing::warn!(
                        event = "project_request_admission",
                        outcome = "unavailable",
                        reason = "runtime_ambiguous",
                        "project request root matches more than one registered runtime"
                    );
                    return None;
                }
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
        lease_roots.insert(resolved_root.clone());
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
                registered_root: resolved_root,
                admitted,
            }),
        })
    }

    /// Admit one request and serve the owners it was admitted under.
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
        match self.admit_request(project_root, canonical_root) {
            Some(request_lease) => ProjectRequestRuntimesV1::from_lease(request_lease),
            None => ProjectRequestRuntimesV1::default(),
        }
    }

    /// Serve the owners an already-admitted lease was admitted under.
    ///
    /// The lease must belong to this registry and its counted roots must name
    /// `project_root`; nothing is resolved again, so a registry change since
    /// admission cannot swap in an owner the lease never counted.
    pub fn request_runtimes_with_admission(
        &self,
        project_root: &Path,
        admission: &ProjectRuntimeRequestLeaseV1,
    ) -> ProjectRequestRuntimesV1 {
        if !admission.covers(self, project_root) {
            return ProjectRequestRuntimesV1::default();
        }
        ProjectRequestRuntimesV1::from_lease(admission.clone())
    }
}

impl ProjectRequestRuntimesV1 {
    fn from_lease(request_lease: ProjectRuntimeRequestLeaseV1) -> Self {
        let admitted = &request_lease.inner.admitted;
        Self {
            admitted: true,
            resolved_root: Some(request_lease.inner.registered_root.clone()),
            publication: Some(admitted.publication),
            feedback: admitted.feedback.clone(),
            feedback_owner: admitted.feedback_owner.clone(),
            advisory_cycle: admitted.advisory_cycle.clone(),
            configuration: admitted.configuration.clone(),
            work: admitted.work.clone(),
            retained: admitted.retained.clone(),
            lsp_owner: admitted.lsp_owner.clone(),
            source_edit: admitted.source_edit.clone(),
            _request_lease: Some(request_lease),
        }
    }

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
    use tracedecay_contracts::{ApplicationProblem, SafeDiagnostic};
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

    /// The registry changes after admission and before the snapshot is
    /// taken: the owner under the admitted key is replaced. The lease still
    /// serves the owner it counted; only a fresh admission sees the
    /// replacement.
    #[tokio::test]
    async fn admitted_lease_serves_the_admitted_owner_after_the_registry_changes() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project_root = PathBuf::from("/projects/replaced-owner");
        let admitted_project = ProjectId::new("project.admitted-owner").expect("project id");
        let replacement_project = ProjectId::new("project.replacement-owner").expect("project id");
        registry
            .register(
                project_root.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    admitted_project.clone(),
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("admitted owner registration");
        let admission = registry
            .admit_request(&project_root, None)
            .expect("request admission");

        registry
            .publish(
                project_root.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    replacement_project.clone(),
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("replacement owner publication");
        let attempt = registry
            .begin_publication(&project_root)
            .expect("publication attempt");
        assert!(registry.mark_publication_ready(&attempt));

        let snapshot = registry.request_runtimes_with_admission(&project_root, &admission);
        assert!(snapshot.is_admitted());
        assert_eq!(
            snapshot.advisory_cycle.expect("admitted owner").project_id,
            admitted_project,
            "a lease must serve the owner its request count was taken for, not a later replacement"
        );
        assert_eq!(
            snapshot.publication,
            Some(ProjectRuntimePublicationStateV1::Warming),
            "the publication stage is the one admission observed"
        );

        let fresh = registry.request_runtimes(Some(&project_root), None).await;
        assert_eq!(
            fresh.advisory_cycle.expect("replacement owner").project_id,
            replacement_project,
            "a new admission is counted against, and serves, the replacement"
        );
        assert_eq!(
            fresh.publication,
            Some(ProjectRuntimePublicationStateV1::Ready)
        );
    }

    #[tokio::test]
    async fn captured_admission_cannot_cross_into_another_registered_root() {
        let registry = ProjectRuntimeRegistryV1::default();
        let alias = PathBuf::from("/projects/admitted-alias");
        let admitted = PathBuf::from("/projects/admitted-root");
        let foreign = PathBuf::from("/projects/foreign-root");
        let admitted_project = ProjectId::new("project.admitted-root").expect("project id");
        registry
            .register(
                admitted.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    admitted_project.clone(),
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("admitted owner registration");
        registry
            .register(
                foreign.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    ProjectId::new("project.foreign-root").expect("project id"),
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("foreign owner registration");
        let admission = registry
            .admit_request(&alias, Some(&admitted))
            .expect("capture admitted alias");

        assert!(admission.covers(&registry, &alias));
        assert!(!admission.covers(&registry, &foreign));
        assert!(
            !registry
                .request_runtimes_with_admission(&foreign, &admission)
                .is_admitted(),
            "the admitted root cached on A must not make an unrelated B look covered"
        );

        registry.lock_root_fences().quiesced.insert(foreign.clone());
        assert!(
            !registry
                .request_runtimes_with_admission(&foreign, &admission)
                .is_admitted(),
            "an A lease must not bypass B's root fence"
        );
        let snapshot = registry.request_runtimes_with_admission(&alias, &admission);
        assert_eq!(snapshot.resolved_root.as_deref(), Some(admitted.as_path()));
        assert_eq!(
            snapshot.advisory_cycle.expect("admitted owner").project_id,
            admitted_project
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captured_admission_does_not_follow_a_retargeted_alias() {
        let registry = ProjectRuntimeRegistryV1::default();
        let fixture = tempfile::tempdir().expect("retargeted alias fixture");
        let alias = fixture.path().join("alias");
        let original = fixture.path().join("original");
        let replacement = fixture.path().join("replacement");
        std::fs::create_dir(&original).expect("original root");
        std::fs::create_dir(&replacement).expect("replacement root");
        std::os::unix::fs::symlink(&original, &alias).expect("original alias");
        let original = original.canonicalize().expect("canonical original root");
        let replacement = replacement
            .canonicalize()
            .expect("canonical replacement root");
        let original_project = ProjectId::new("project.original-target").expect("project id");
        let replacement_project = ProjectId::new("project.replacement-target").expect("project id");
        registry
            .register(
                original.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    original_project.clone(),
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("original owner registration");
        registry
            .register(
                replacement.clone(),
                DaemonAdvisoryCycleInvocationOwner::new(
                    replacement_project,
                    Arc::new(UnavailableAdvisoryCycle),
                ),
            )
            .await
            .expect("replacement owner registration");
        let admitted_root = alias.canonicalize().expect("admitted alias target");
        let admission = registry
            .admit_request(&alias, Some(&admitted_root))
            .expect("capture original alias admission");
        std::fs::remove_file(&alias).expect("remove original alias");
        std::os::unix::fs::symlink(&replacement, &alias).expect("retarget alias");
        assert_eq!(
            alias.canonicalize().expect("retargeted alias target"),
            replacement
        );

        let snapshot = registry.request_runtimes_with_admission(&alias, &admission);

        assert_eq!(snapshot.resolved_root.as_deref(), Some(original.as_path()));
        assert_eq!(
            snapshot.advisory_cycle.expect("captured owner").project_id,
            original_project,
            "an alias retarget after admission must not switch the callable owner"
        );
    }
}
