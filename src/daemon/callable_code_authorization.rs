use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::{
    ApplicationOperation, ApplicationProblem, AuthorityReceipt, CallableCodeAuthorizationAdmission,
    CallableCodeAuthorizationFuture, CallableCodeAuthorizationPort, RequestAdmission,
    RequestContext, ResolvedScope, RetryDirective,
};
use tracedecay_domain::{ComponentVersion, UtcMicros};

use crate::application::ProjectSourceAccessSnapshot;
use crate::application::configuration::{ConfigurationControlStore, ProjectConfigurationRuntime};

type CurrentAccessFuture<'a> = Pin<
    Box<dyn Future<Output = Result<ProjectSourceAccessSnapshot, ApplicationProblem>> + Send + 'a>,
>;

trait CurrentCallableCodeAccessPort: Send + Sync {
    fn current_access(&self, observed_at: UtcMicros) -> CurrentAccessFuture<'_>;
}

struct ProductionCallableCodeAccessPort {
    project_root: PathBuf,
    scope: ResolvedScope,
    configuration: Arc<ProjectConfigurationRuntime>,
}

impl CurrentCallableCodeAccessPort for ProductionCallableCodeAccessPort {
    fn current_access(&self, observed_at: UtcMicros) -> CurrentAccessFuture<'_> {
        Box::pin(async move {
            let current = self
                .configuration
                .configuration_store()
                .current()
                .await
                .map_err(|_| concealed())?;
            let configuration = tracedecay_usecases::config::PinnedRuntimeConfiguration::new(
                self.configuration.configuration_target().clone(),
                current.revision_id,
                current.snapshot,
            )
            .map_err(|_| concealed())?;
            crate::daemon::project_open_owners::daemon_owned_project_source_access_at(
                &self.scope,
                &self.project_root,
                &configuration,
                observed_at,
            )
            .map_err(|_| concealed())
        })
    }
}

#[derive(Clone)]
pub(super) struct DaemonCallableCodeAuthorizationSource {
    access: Arc<dyn CurrentCallableCodeAccessPort>,
}

impl DaemonCallableCodeAuthorizationSource {
    pub(super) fn production(
        project_root: PathBuf,
        scope: ResolvedScope,
        configuration: Arc<ProjectConfigurationRuntime>,
    ) -> Self {
        Self {
            access: Arc::new(ProductionCallableCodeAccessPort {
                project_root,
                scope,
                configuration,
            }),
        }
    }

    pub(super) async fn current(
        &self,
        observed_at: UtcMicros,
    ) -> Result<ProjectSourceAccessSnapshot, ApplicationProblem> {
        self.access.current_access(observed_at).await
    }

    pub(super) fn authorize(
        &self,
        admitted_access: ProjectSourceAccessSnapshot,
    ) -> DaemonCallableCodeAuthorization {
        DaemonCallableCodeAuthorization {
            source: self.clone(),
            admitted_access,
        }
    }
}

pub(super) struct DaemonCallableCodeAuthorization {
    source: DaemonCallableCodeAuthorizationSource,
    admitted_access: ProjectSourceAccessSnapshot,
}

impl DaemonCallableCodeAuthorization {
    async fn route_receipt(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> Result<AuthorityReceipt, ApplicationProblem> {
        let current = self.source.current(observed_at).await?;
        if !same_authority(&self.admitted_access, &current)
            || context.admission_at(observed_at) != RequestAdmission::Admitted
            || !current.allows(context, operation, observed_at)
        {
            return Err(concealed());
        }
        let policy = tracedecay_application::PolicyDecisionRef::new(
            format!(
                "route.callable-code.{}",
                current.binding.binding_id.as_str()
            ),
            1,
            current.configuration_provenance_digest,
            ComponentVersion::new("project-source-access.v1").map_err(|_| concealed())?,
        )
        .map_err(|_| concealed())?;
        AuthorityReceipt::from_context(context, policy, observed_at).map_err(|_| concealed())
    }
}

impl CallableCodeAuthorizationPort for DaemonCallableCodeAuthorization {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<
        'a,
        Result<CallableCodeAuthorizationAdmission, ApplicationProblem>,
    > {
        Box::pin(async move {
            self.route_receipt(context, operation, observed_at)
                .await
                .map(CallableCodeAuthorizationAdmission::Routed)
        })
    }

    fn recheck_publication<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a CallableCodeAuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<'a, Result<AuthorityReceipt, ApplicationProblem>> {
        Box::pin(async move {
            let CallableCodeAuthorizationAdmission::Routed(admission) = admission else {
                return Err(concealed());
            };
            let current = self.route_receipt(context, operation, observed_at).await?;
            if admission.grant_id != current.grant_id
                || admission.grant_revision != current.grant_revision
                || admission.grant_digest != current.grant_digest
                || admission.authorized_scope_digest != current.authorized_scope_digest
                || admission.disclosure != current.disclosure
                || admission.policy != current.policy
            {
                return Err(concealed());
            }
            Ok(current)
        })
    }
}

fn same_authority(
    admitted: &ProjectSourceAccessSnapshot,
    current: &ProjectSourceAccessSnapshot,
) -> bool {
    admitted.scope == current.scope
        && admitted.requester == current.requester
        && admitted.binding == current.binding
        && admitted.configuration_revision == current.configuration_revision
        && admitted.configuration_digest == current.configuration_digest
        && admitted.configuration_provenance_digest == current.configuration_provenance_digest
        && admitted.effective_capabilities == current.effective_capabilities
}

fn concealed() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use tracedecay_application::{
        CallableCodeOperationKind, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
        Deadline, DisclosureClass, RequestId, callable_code_operations,
    };
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceKindV1,
    };
    use tracedecay_domain::{
        ActorId, LocatorDigest, ManifestDigest, ProjectId, RepositoryId, SourceBindingId,
        WorktreeId, canonical_sha256,
    };

    use super::*;

    struct MutableAccess {
        current: Mutex<ProjectSourceAccessSnapshot>,
    }

    impl CurrentCallableCodeAccessPort for MutableAccess {
        fn current_access(&self, _observed_at: UtcMicros) -> CurrentAccessFuture<'_> {
            Box::pin(async move {
                Ok(self
                    .current
                    .lock()
                    .unwrap_or_else(|_| panic!("mutable access lock"))
                    .clone())
            })
        }
    }

    fn access(operation: &ApplicationOperation) -> ProjectSourceAccessSnapshot {
        let scope = ResolvedScope::new(
            ProjectId::new("project.callable-auth").expect("project"),
            RepositoryId::new("repository.callable-auth").expect("repository"),
            WorktreeId::new("worktree.callable-auth").expect("worktree"),
            None,
        )
        .expect("scope");
        ProjectSourceAccessSnapshot {
            scope: scope.clone(),
            requester: ActorId::new("actor.callable-auth").expect("actor"),
            binding: ScopeSourceBinding::new(
                SourceBindingId::new("binding.callable-auth").expect("binding"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("locator"),
                AuthorityRef::Project(scope.project_id.clone()),
            )
            .expect("binding"),
            configuration_revision: ConfigurationRevisionId::new("revision.callable-auth.1")
                .expect("revision"),
            configuration_digest: canonical_sha256(&"callable-auth-configuration")
                .expect("configuration digest"),
            configuration_provenance_digest: canonical_sha256(
                &"callable-auth-configuration-provenance",
            )
            .expect("provenance digest"),
            effective_capabilities: BTreeSet::from([operation.capability_id().clone()]),
            grant_expires_at: UtcMicros(100),
        }
    }

    fn context(
        access: &ProjectSourceAccessSnapshot,
        operation: &ApplicationOperation,
    ) -> RequestContext {
        let digest: ManifestDigest =
            canonical_sha256(&"callable-auth-grant").expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.callable-auth").expect("grant"),
            1,
            digest,
            access.requester.clone(),
            UtcMicros(1),
            access.grant_expires_at,
            access.scope.clone(),
            BTreeSet::from([operation.capability_id().clone()]),
            BTreeSet::from([operation.use_case_id().clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            access.requester.clone(),
            access.scope.clone(),
            grant,
            RequestId::new("request.callable-auth").expect("request"),
            Deadline::new(UtcMicros(90)).expect("deadline"),
            CancellationContext::active("cancel.callable-auth").expect("cancellation"),
        )
        .expect("context")
    }

    #[tokio::test]
    async fn configuration_mutation_after_mount_fails_publication_revalidation_closed() {
        let operation = callable_code_operations()
            .expect("operations")
            .get(CallableCodeOperationKind::ExactOccurrence)
            .clone();
        let mounted = access(&operation);
        let mutable = Arc::new(MutableAccess {
            current: Mutex::new(mounted.clone()),
        });
        let source = DaemonCallableCodeAuthorizationSource {
            access: mutable.clone(),
        };
        let authorization = source.authorize(mounted.clone());
        let context = context(&mounted, &operation);
        let admission = authorization
            .admit(&context, &operation, UtcMicros(10))
            .await
            .expect("initial current authority");

        {
            let mut current = mutable
                .current
                .lock()
                .unwrap_or_else(|_| panic!("mutable access lock"));
            current.configuration_revision =
                ConfigurationRevisionId::new("revision.callable-auth.2").expect("revision");
            current.configuration_digest =
                canonical_sha256(&"revoked-callable-auth-configuration").expect("digest");
            current.effective_capabilities.clear();
        }

        assert!(
            authorization
                .recheck_publication(&context, &operation, &admission, UtcMicros(11))
                .await
                .is_err(),
            "configuration/capability mutation must conceal the result"
        );
        assert!(
            source
                .authorize(mounted)
                .admit(&context, &operation, UtcMicros(12))
                .await
                .is_err(),
            "a later call must not reuse project-open access"
        );
    }
}
