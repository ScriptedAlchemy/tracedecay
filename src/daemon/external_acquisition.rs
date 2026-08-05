//! Daemon-mounted canonical external acquisition over existing provider owners.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::feedback::{
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GitHubReviewReadRequestV1, feedback_surface_operation,
};
use tracedecay_application::{
    AuthorizationPhase, AuthorizationRequest, RequestAdmission, RequestContext,
};
use tracedecay_domain::configuration::SourceKindV1;
use tracedecay_domain::{ManifestDigest, ProviderId, UtcMicros};
use tracedecay_hooks::HookEventEnvelopeV2;

use crate::application::advisory::{
    Pr13AdvisoryProductionStartupRegistrationV1, ProjectGitHubAnchorAuthorityV1,
};
use crate::application::external_source_acquisition::{
    ExternalSourceAcquisitionOwnerV1, SourceAcquisitionPolicyV1, SourceAcquisitionRunOutcomeV1,
};
use crate::application::external_source_github::{
    GitHubExternalSourceAcquisitionV1, GitHubExternalSourceOpenErrorV1,
};
use crate::application::external_source_store::RuntimeExternalSourceStore;
use crate::application::observation::ObservationCancellation;
use crate::application::source_authorization::{
    ProjectSourceAccessOutcome, project_source_access_snapshot_for_request,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::global_db::configuration::OwnedGlobalDbConfigurationControlStore;

use super::DaemonInvocationState;

pub(crate) type DaemonExternalAcquisitionFutureV1<'a> =
    Pin<Box<dyn Future<Output = DaemonExternalAcquisitionOutcomeV1> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DaemonExternalAcquisitionOutcomeV1 {
    Processed(SourceAcquisitionRunOutcomeV1),
    Deferred(SourceAcquisitionRunOutcomeV1),
    Unavailable,
}

pub(crate) trait DaemonExternalAcquisitionRuntimeV1: Send + Sync {
    fn handle_github_event<'a>(
        &'a self,
        context: &'a RequestContext,
        request: GitHubReviewReadRequestV1,
        stable_signal_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> DaemonExternalAcquisitionFutureV1<'a>;

    fn cancel(&self);
}

type ProductionGitHubAdapterV1 = GitHubExternalSourceAcquisitionV1<
    Arc<ProjectGitHubAnchorAuthorityV1>,
    Arc<ProjectGitHubAnchorAuthorityV1>,
    OwnedGlobalDbConfigurationControlStore,
>;
type ProductionGitHubAcquisitionOwnerV1 = ExternalSourceAcquisitionOwnerV1<
    RuntimeExternalSourceStore,
    ProductionGitHubAdapterV1,
    ProductionGitHubAdapterV1,
    RuntimeExternalSourceStore,
>;

pub(crate) struct ProductionGitHubExternalAcquisitionV1 {
    owner: Arc<ProductionGitHubAcquisitionOwnerV1>,
    adapter: Arc<ProductionGitHubAdapterV1>,
    cancellation: ObservationCancellation,
    worker: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ProductionGitHubExternalAcquisitionV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        registration: &Pr13AdvisoryProductionStartupRegistrationV1,
        store: RuntimeExternalSourceStore,
        configuration: OwnedGlobalDbConfigurationControlStore,
        context: RequestContext,
        request: GitHubReviewReadRequestV1,
        provider: ProviderId,
        sink_revision: u64,
        sink_digest: ManifestDigest,
    ) -> std::result::Result<Self, GitHubExternalSourceOpenErrorV1> {
        let github = registration
            .runtime()
            .github_owner()
            .ok_or(GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let operation = feedback_surface_operation("github_review_ingest")
            .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?
            .filter(|operation| {
                operation.capability_id().as_str() == GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1
            })
            .ok_or(GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        let observed_at = tracedecay_application::now_micros();
        let authorization = AuthorizationRequest {
            context: &context,
            operation: &operation,
            phase: AuthorizationPhase::Admission,
            observed_at,
        };
        let ProjectSourceAccessOutcome::Allowed(source_access) =
            project_source_access_snapshot_for_request(
                &configuration,
                &authorization,
                SourceKindV1::GitHub,
            )
            .await
        else {
            return Err(GitHubExternalSourceOpenErrorV1::InvalidAuthority);
        };
        let adapter = Arc::new(GitHubExternalSourceAcquisitionV1::new(
            github,
            store.clone(),
            configuration,
            context,
            request,
            &source_access,
            provider,
            sink_revision,
            sink_digest,
        )?);
        let policy = SourceAcquisitionPolicyV1::new(
            5,
            Duration::from_secs(5),
            Duration::from_millis(250),
            Duration::from_secs(30),
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        let owner = Arc::new(
            ExternalSourceAcquisitionOwnerV1::new(
                Arc::new(store.clone()),
                Arc::clone(&adapter),
                Arc::clone(&adapter),
                Arc::new(store),
                policy,
            )
            .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?,
        );
        Ok(Self {
            owner,
            adapter,
            cancellation: ObservationCancellation::default(),
            worker: std::sync::Mutex::new(None),
        })
    }

    fn start_worker(&self) {
        let owner = Arc::clone(&self.owner);
        let cancellation = self.cancellation.clone();
        let worker = tokio::spawn(async move {
            owner.run_background(&cancellation).await;
        });
        *self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
    }

    fn stop_worker(&self) {
        self.cancellation.cancel();
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            worker.abort();
        }
    }

    async fn handle(
        &self,
        context: &RequestContext,
        request: GitHubReviewReadRequestV1,
        stable_signal_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> DaemonExternalAcquisitionOutcomeV1 {
        if context.validate().is_err()
            || context.admission_at(observed_at) != RequestAdmission::Admitted
        {
            return DaemonExternalAcquisitionOutcomeV1::Unavailable;
        }
        if request
            != match self.adapter.acquisition_request() {
                crate::application::external_source_acquisition::SourceAcquisitionRequestV1::GitHubReview {
                    scope,
                    operation,
                    pull_request_id,
                    ..
                } => GitHubReviewReadRequestV1 {
                    operation: *operation,
                    scope: scope.clone(),
                    pull_request_id: pull_request_id.clone(),
                },
            }
        {
            return DaemonExternalAcquisitionOutcomeV1::Unavailable;
        }
        let event = match self.adapter.event(stable_signal_digest) {
            Ok(event) => event,
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        if self
            .owner
            .admit_event(
                self.adapter.definition(),
                self.adapter.binding(),
                self.adapter.acquisition_request(),
                event,
                observed_at,
            )
            .await
            .is_err()
        {
            return DaemonExternalAcquisitionOutcomeV1::Unavailable;
        }
        DaemonExternalAcquisitionOutcomeV1::Deferred(SourceAcquisitionRunOutcomeV1::Idle)
    }
}

impl DaemonExternalAcquisitionRuntimeV1 for ProductionGitHubExternalAcquisitionV1 {
    fn handle_github_event<'a>(
        &'a self,
        context: &'a RequestContext,
        request: GitHubReviewReadRequestV1,
        stable_signal_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> DaemonExternalAcquisitionFutureV1<'a> {
        Box::pin(async move {
            self.handle(context, request, stable_signal_digest, observed_at)
                .await
        })
    }

    fn cancel(&self) {
        self.stop_worker();
    }
}

impl Drop for ProductionGitHubExternalAcquisitionV1 {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

pub(crate) fn open_external_source_store(
    database: &RegisteredGlobalDb,
    configured_provider: Option<&ProviderId>,
) -> Result<Option<RuntimeExternalSourceStore>> {
    configured_provider
        .map(|_| {
            RuntimeExternalSourceStore::new(
                database.runtime().clone(),
                database.authority().clone(),
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open external acquisition store failed: {error}"),
            })
        })
        .transpose()
}

pub(crate) async fn mount_production_github_external_acquisition(
    invocation: &DaemonInvocationState,
    project_root: &std::path::Path,
    registration: &Pr13AdvisoryProductionStartupRegistrationV1,
    database: Arc<RegisteredGlobalDb>,
    context: Option<RequestContext>,
    request: Option<GitHubReviewReadRequestV1>,
    provider: Option<ProviderId>,
    store: Option<RuntimeExternalSourceStore>,
) -> Result<Option<Arc<dyn DaemonExternalAcquisitionRuntimeV1>>> {
    let (provider, store, context, request) = match (provider, store, context, request) {
        (Some(provider), Some(store), Some(context), Some(request)) => {
            (provider, store, context, request)
        }
        (None, None, None, None) => return Ok(None),
        _ => {
            return Err(TraceDecayError::Config {
                message: "project-open external acquisition authority is incomplete".to_owned(),
            });
        }
    };
    let sink_revision = database.runtime().binding().authority_epoch.get();
    let sink_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.external-source.project-runtime-sink.v1",
        database.runtime().binding(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open external acquisition sink is invalid: {error}"),
    })?;
    let configuration =
        OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(database);
    let runtime = ProductionGitHubExternalAcquisitionV1::new(
        registration,
        store,
        configuration,
        context,
        request,
        provider,
        sink_revision,
        sink_digest,
    )
    .await
    .map_err(|_| TraceDecayError::Config {
        message: "project-open external acquisition provider is unavailable".to_owned(),
    })?;
    let runtime = Arc::new(runtime);
    runtime.start_worker();
    let runtime: Arc<dyn DaemonExternalAcquisitionRuntimeV1> = runtime;
    invocation
        .advisory_runtime_registrar()
        .register_external_acquisition(project_root.to_path_buf(), Arc::clone(&runtime))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open external acquisition registration failed: {error}"),
        })?;
    Ok(Some(runtime))
}

pub(crate) async fn handle_github_hook_event(
    runtime: Option<&Arc<dyn DaemonExternalAcquisitionRuntimeV1>>,
    context: &RequestContext,
    request: Option<&GitHubReviewReadRequestV1>,
    envelope: &HookEventEnvelopeV2,
    observed_at: UtcMicros,
) -> DaemonExternalAcquisitionOutcomeV1 {
    let (Some(runtime), Some(request)) = (runtime, request) else {
        return DaemonExternalAcquisitionOutcomeV1::Unavailable;
    };
    let signal = match tracedecay_domain::canonical_sha256(&(
        "tracedecay.github-review.external-source-hook-event.v1",
        &envelope.event_id,
        &envelope.producer,
        &envelope.protected_session_id,
        &envelope.project_id,
        &envelope.repository_id,
        &envelope.worktree_id,
    )) {
        Ok(signal) => signal,
        Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
    };
    runtime
        .handle_github_event(context, request.clone(), signal, observed_at)
        .await
}
