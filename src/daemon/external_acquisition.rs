//! Daemon-mounted canonical external acquisition over existing provider owners.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_application::{RequestAdmission, RequestContext};
use tracedecay_domain::{ManifestDigest, ProviderId, UtcMicros};
use tracedecay_hooks::HookEventEnvelopeV2;

use crate::application::advisory::{
    GitHubReviewRuntimeOwnerV1, Pr13AdvisoryProductionStartupRegistrationV1,
    ProjectGitHubAnchorAuthorityV1,
};
use crate::application::external_source_acquisition::{
    ExternalSourceAcquisitionOwnerV1, SourceAcquisitionPolicyV1, SourceAcquisitionRunOutcomeV1,
};
use crate::application::external_source_github::{
    GitHubExternalSourceAcquisitionV1, GitHubExternalSourceOpenErrorV1,
};
use crate::application::external_source_store::RuntimeExternalSourceStore;
use crate::application::observation::ObservationCancellation;
use crate::application::source_authorization::ProjectSourceAccessSnapshot;
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
}

type ProductionGitHubOwnerV1 = GitHubReviewRuntimeOwnerV1<
    Arc<ProjectGitHubAnchorAuthorityV1>,
    Arc<ProjectGitHubAnchorAuthorityV1>,
>;

pub(crate) struct ProductionGitHubExternalAcquisitionV1 {
    github: Arc<ProductionGitHubOwnerV1>,
    store: RuntimeExternalSourceStore,
    configuration: OwnedGlobalDbConfigurationControlStore,
    source_access: ProjectSourceAccessSnapshot,
    provider: ProviderId,
    sink_revision: u64,
    sink_digest: ManifestDigest,
}

impl ProductionGitHubExternalAcquisitionV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        registration: &Pr13AdvisoryProductionStartupRegistrationV1,
        store: RuntimeExternalSourceStore,
        configuration: OwnedGlobalDbConfigurationControlStore,
        source_access: ProjectSourceAccessSnapshot,
        provider: ProviderId,
        sink_revision: u64,
        sink_digest: ManifestDigest,
    ) -> Result<Self, GitHubExternalSourceOpenErrorV1> {
        let github = registration
            .runtime()
            .github_owner()
            .ok_or(GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        Ok(Self {
            github,
            store,
            configuration,
            source_access,
            provider,
            sink_revision,
            sink_digest,
        })
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
        let adapter = match GitHubExternalSourceAcquisitionV1::new(
            Arc::clone(&self.github),
            self.store.clone(),
            self.configuration.clone(),
            context.clone(),
            request,
            &self.source_access,
            self.provider.clone(),
            self.sink_revision,
            self.sink_digest.clone(),
        ) {
            Ok(adapter) => Arc::new(adapter),
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        let policy = match SourceAcquisitionPolicyV1::new(
            5,
            Duration::from_secs(5),
            Duration::from_millis(250),
            Duration::from_secs(30),
        ) {
            Ok(policy) => policy,
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        let owner = match ExternalSourceAcquisitionOwnerV1::new(
            Arc::new(self.store.clone()),
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            Arc::new(self.store.clone()),
            policy,
        ) {
            Ok(owner) => owner,
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        let cancellation = ObservationCancellation::default();
        let resumed = match owner.run_one(observed_at, &cancellation).await {
            Ok(outcome) => outcome,
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        let event = match adapter.event(stable_signal_digest) {
            Ok(event) => event,
            Err(_) => return DaemonExternalAcquisitionOutcomeV1::Unavailable,
        };
        if owner
            .admit_event(adapter.definition(), adapter.binding(), event, observed_at)
            .await
            .is_err()
        {
            return DaemonExternalAcquisitionOutcomeV1::Unavailable;
        }
        match owner.run_one(observed_at, &cancellation).await {
            Ok(SourceAcquisitionRunOutcomeV1::Idle) => {
                DaemonExternalAcquisitionOutcomeV1::Deferred(resumed)
            }
            Ok(outcome) => DaemonExternalAcquisitionOutcomeV1::Processed(outcome),
            Err(_) => DaemonExternalAcquisitionOutcomeV1::Unavailable,
        }
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
    source_access: ProjectSourceAccessSnapshot,
    provider: Option<ProviderId>,
    store: Option<RuntimeExternalSourceStore>,
) -> Result<Option<Arc<dyn DaemonExternalAcquisitionRuntimeV1>>> {
    let (provider, store) = match (provider, store) {
        (Some(provider), Some(store)) => (provider, store),
        (None, None) => return Ok(None),
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
        source_access,
        provider,
        sink_revision,
        sink_digest,
    )
    .map_err(|_| TraceDecayError::Config {
        message: "project-open external acquisition provider is unavailable".to_owned(),
    })?;
    let runtime: Arc<dyn DaemonExternalAcquisitionRuntimeV1> = Arc::new(runtime);
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
