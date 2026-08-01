//! Owned PR13 production-authority composition for daemon registration.

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_hooks::HookFeedbackDeliveryPortV1;

use crate::tracedecay::TraceDecay;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_runtime_core::db::Database;

use super::github_runtime::GitHubSourceAccessAuthorityV1;
use super::proximity_runtime::production_proximity_evidence_authority_v1;
use super::{
    CiCodeAnchorStoreV1, CiRetainedProviderObservationAuthorityV1,
    Pr13AdvisoryDaemonStartupRegistrationV1, Pr13AdvisoryHookLookupNoticeV1,
    Pr13AdvisoryHookNoticeSinkV1, Pr13AdvisoryProviderAuthoritiesV1, ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1, ProductionCiProviderConfigV1,
    ProjectGitHubAnchorAuthorityV1, SharedCanonicalProximityEvidenceAuthorityV1,
    github_anchor_authorities_arc_v1, new_pr13_advisory_hook_delivery_port,
    open_production_ci_provider_authorities_v1, unavailable_production_ci_provider_authorities_v1,
};
use tracedecay_domain::feedback::FeedbackScopeV1;

pub type Pr13AdvisoryProductionProviderAuthoritiesV1 = Pr13AdvisoryProviderAuthoritiesV1<
    Arc<ProjectGitHubAnchorAuthorityV1>,
    Arc<ProjectGitHubAnchorAuthorityV1>,
    ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1,
    SharedCanonicalProximityEvidenceAuthorityV1,
    OwnedGlobalDbConfigurationControlStore,
>;

pub type Pr13AdvisoryProductionHookDeliveryPortV1 =
    Arc<dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync>;

pub type Pr13AdvisoryProductionStartupRegistrationV1 = Pr13AdvisoryDaemonStartupRegistrationV1<
    Arc<ProjectGitHubAnchorAuthorityV1>,
    Arc<ProjectGitHubAnchorAuthorityV1>,
    ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1,
    SharedCanonicalProximityEvidenceAuthorityV1,
    OwnedGlobalDbConfigurationControlStore,
>;

/// Registrar-ready owned handles. Every authority retains the already-open
/// daemon database/runtime it reads; dropping the registered bundle tears down
/// those references without opening or cleaning up another store.
pub struct Pr13AdvisoryProductionAuthoritiesV1 {
    pub providers: Pr13AdvisoryProductionProviderAuthoritiesV1,
    pub hook_delivery_port: Pr13AdvisoryProductionHookDeliveryPortV1,
}

impl Pr13AdvisoryProductionAuthoritiesV1 {
    pub fn into_registrar_parts(
        self,
    ) -> (
        Pr13AdvisoryProductionProviderAuthoritiesV1,
        Pr13AdvisoryProductionHookDeliveryPortV1,
    ) {
        (self.providers, self.hook_delivery_port)
    }
}

#[derive(Clone)]
pub struct Pr13AdvisoryProductionOpenV1 {
    pub database: Database,
    pub(crate) project_runtime_db: Arc<RegisteredGlobalDb>,
    pub graph: Arc<TraceDecay>,
    pub code_index_identity:
        Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>,
    pub project_root: PathBuf,
    pub feedback_scope: FeedbackScopeV1,
    pub ci_config: Option<ProductionCiProviderConfigV1>,
    pub github_source_access: Option<Arc<dyn GitHubSourceAccessAuthorityV1>>,
    pub ci_retained: Arc<dyn CiRetainedProviderObservationAuthorityV1>,
    pub ci_code_anchors: Arc<dyn CiCodeAnchorStoreV1>,
    pub hook_v2: Arc<Pr13AdvisoryHookNoticeSinkV1>,
    pub legacy_hook: Arc<Pr13AdvisoryHookNoticeSinkV1>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum Pr13AdvisoryProductionOpenErrorV1 {
    #[error("PR13 GitHub anchor/remap authority could not open for the exact project scope")]
    GitHubAuthorityUnavailable,
    #[error("PR13 proximity authority could not open for the exact project worktree")]
    ProximityAuthorityUnavailable,
    #[error("PR13 CI provider authorities could not open")]
    CiAuthorityUnavailable,
}

/// Constructs every registrar authority that can be derived from existing
/// project runtime components. No fixture or adapter-local source is accepted.
pub fn open_pr13_advisory_production_authorities(
    input: Pr13AdvisoryProductionOpenV1,
) -> Result<Pr13AdvisoryProductionAuthoritiesV1, Pr13AdvisoryProductionOpenErrorV1> {
    let Pr13AdvisoryProductionOpenV1 {
        database,
        project_runtime_db,
        graph,
        code_index_identity,
        project_root,
        feedback_scope,
        ci_config,
        github_source_access,
        ci_retained,
        ci_code_anchors,
        hook_v2,
        legacy_hook,
    } = input;
    let github = github_anchor_authorities_arc_v1(
        database,
        project_root.clone(),
        feedback_scope.clone(),
        Arc::clone(&code_index_identity),
    )
    .ok_or(Pr13AdvisoryProductionOpenErrorV1::GitHubAuthorityUnavailable)?;
    let proximity_evidence = production_proximity_evidence_authority_v1(
        Arc::clone(&project_runtime_db),
        graph,
        feedback_scope.clone(),
        project_root,
        code_index_identity,
    )
    .ok_or(Pr13AdvisoryProductionOpenErrorV1::ProximityAuthorityUnavailable)?;
    let configuration = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
        project_runtime_db,
    );
    let ci = match ci_config {
        Some(config) => {
            open_production_ci_provider_authorities_v1(config, ci_retained, ci_code_anchors)
                .map_err(|_| Pr13AdvisoryProductionOpenErrorV1::CiAuthorityUnavailable)?
        }
        None => unavailable_production_ci_provider_authorities_v1(),
    };
    let (ci_source, ci_exact_evidence) = ci.into_registrar_parts();
    let hook_delivery_port =
        new_pr13_advisory_hook_delivery_port(feedback_scope, hook_v2, legacy_hook);

    Ok(Pr13AdvisoryProductionAuthoritiesV1 {
        providers: Pr13AdvisoryProviderAuthoritiesV1 {
            github_remapper: github.github_remapper,
            github_anchors: github.github_anchors,
            ci_source,
            ci_exact_evidence,
            proximity_evidence,
            github_source_access,
            configuration,
        },
        hook_delivery_port,
    })
}
