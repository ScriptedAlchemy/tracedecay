//! Concrete runtime-state composition for feedback-cycle watermark checks.
//!
//! The daemon owns each underlying authority. This adapter only joins their
//! immutable outputs into the transport-neutral application runtime state; it
//! never resolves a path, scans Git, selects an analyzer, or caches a snapshot.

use std::future::Future;
use std::pin::Pin;

use tracedecay_application::context::RequestAdmission;
use tracedecay_application::{RequestContext, feedback::FeedbackRuntimeStatePort};
use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackBaselineHorizonV1, FeedbackContentIdentityV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackEvaluationInputV1, FeedbackScopeV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ManifestDigest, ProjectId, RepositoryId, WorktreeId,
    canonical_sha256,
};

const OVERLAY_RUNTIME_WATERMARK_DOMAIN: &str = "tracedecay.feedback.overlay-runtime.v1";

pub type FeedbackRuntimeAuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Option<T>> + Send + 'a>>;

/// Immutable project/root binding without a mutable HEAD value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackProjectBindingV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub branch_ref: String,
}

impl FeedbackProjectBindingV1 {
    fn with_head(&self, head_commit_id: CommitId) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: self.project_id.clone(),
            repository_id: self.repository_id.clone(),
            worktree_id: self.worktree_id.clone(),
            branch_ref: self.branch_ref.clone(),
            head_commit_id,
        }
    }
}

/// Exact composed runtime address supplied to the baseline/watermark owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackRuntimeAddressV1 {
    pub scope: FeedbackScopeV1,
    pub content: FeedbackContentIdentityV1,
    pub generation_id: Option<CodeGenerationId>,
    pub configuration_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
}

/// Baseline state and watermark returned atomically by the durable
/// generation/history owner. `baseline_horizon: None` is an authoritative
/// no-prior-baseline answer, while an absent future result is unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackBaselineWatermarkV1 {
    pub runtime_watermark: ManifestDigest,
    pub baseline_horizon: Option<FeedbackBaselineHorizonV1>,
}

pub trait FeedbackProjectAuthority {
    fn project_binding<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, FeedbackProjectBindingV1>;
}

pub trait FeedbackHeadAuthority {
    fn head_commit_id<'a>(
        &'a self,
        context: &'a RequestContext,
        project: &'a FeedbackProjectBindingV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, CommitId>;
}

pub trait FeedbackContentAuthority {
    fn content_identity<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
        scope: &'a FeedbackScopeV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, FeedbackContentIdentityV1>;
}

pub trait FeedbackGenerationAuthority {
    fn clean_generation_id<'a>(
        &'a self,
        context: &'a RequestContext,
        scope: &'a FeedbackScopeV1,
        content: &'a FeedbackContentIdentityV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, CodeGenerationId>;
}

pub trait FeedbackConfigurationAuthority {
    fn configuration_digest<'a>(
        &'a self,
        context: &'a RequestContext,
        scope: &'a FeedbackScopeV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, ManifestDigest>;
}

pub trait FeedbackPolicyAuthority {
    fn policy_digest<'a>(
        &'a self,
        context: &'a RequestContext,
        scope: &'a FeedbackScopeV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, ManifestDigest>;
}

pub trait FeedbackBaselineWatermarkAuthority {
    fn baseline_watermark<'a>(
        &'a self,
        context: &'a RequestContext,
        address: &'a FeedbackRuntimeAddressV1,
    ) -> FeedbackRuntimeAuthorityFuture<'a, FeedbackBaselineWatermarkV1>;
}

/// Concrete adapter over the seven daemon-owned runtime authorities. A new
/// resolve reads every dependency again, preserving the service's pre/post
/// port watermark checks instead of caching a stale composition.
pub struct AuthoritativeFeedbackRuntimeAdapter<P, H, G, C, F, Y, B> {
    project: P,
    head: H,
    generation: G,
    content: C,
    configuration: F,
    policy: Y,
    baseline: B,
}

impl<P, H, G, C, F, Y, B> AuthoritativeFeedbackRuntimeAdapter<P, H, G, C, F, Y, B> {
    pub fn new(
        project: P,
        head: H,
        generation: G,
        content: C,
        configuration: F,
        policy: Y,
        baseline: B,
    ) -> Self {
        Self {
            project,
            head,
            generation,
            content,
            configuration,
            policy,
            baseline,
        }
    }
}

impl<P, H, G, C, F, Y, B> FeedbackRuntimeStatePort
    for AuthoritativeFeedbackRuntimeAdapter<P, H, G, C, F, Y, B>
where
    P: FeedbackProjectAuthority + Sync,
    H: FeedbackHeadAuthority + Sync,
    G: FeedbackGenerationAuthority + Sync,
    C: FeedbackContentAuthority + Sync,
    F: FeedbackConfigurationAuthority + Sync,
    Y: FeedbackPolicyAuthority + Sync,
    B: FeedbackBaselineWatermarkAuthority + Sync,
{
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<
        'a,
        Option<tracedecay_application::feedback::FeedbackRuntimeStateV1>,
    > {
        Box::pin(async move {
            if !admitted(context, input) {
                return None;
            }
            let project = self.project.project_binding(context, input).await?;
            if !admitted(context, input) {
                return None;
            }
            let head = self.head.head_commit_id(context, &project).await?;
            let scope = project.with_head(head);
            scope.validate().ok()?;
            if !admitted(context, input) {
                return None;
            }
            let content = self
                .content
                .content_identity(context, input, &scope)
                .await?;
            content.validate().ok()?;
            if !admitted(context, input) {
                return None;
            }
            let generation_id = match &content {
                FeedbackContentIdentityV1::SavedContent { .. } => Some(
                    self.generation
                        .clean_generation_id(context, &scope, &content)
                        .await?,
                ),
                FeedbackContentIdentityV1::EphemeralOverlay { .. } => None,
            };
            if !admitted(context, input) {
                return None;
            }
            let configuration_digest = self
                .configuration
                .configuration_digest(context, &scope)
                .await?;
            configuration_digest.validate().ok()?;
            if !admitted(context, input) {
                return None;
            }
            let policy_digest = self.policy.policy_digest(context, &scope).await?;
            policy_digest.validate().ok()?;
            if !admitted(context, input) {
                return None;
            }

            let address = FeedbackRuntimeAddressV1 {
                scope: scope.clone(),
                content: content.clone(),
                generation_id: generation_id.clone(),
                configuration_digest: configuration_digest.clone(),
                policy_digest: policy_digest.clone(),
            };
            let (runtime_watermark, baseline_horizon) = match &content {
                FeedbackContentIdentityV1::SavedContent {
                    generation_digest,
                    file_digest,
                } => {
                    let watermark = self.baseline.baseline_watermark(context, &address).await?;
                    if !admitted(context, input) {
                        return None;
                    }
                    watermark.runtime_watermark.validate().ok()?;
                    let horizon = if address_matches_input(&address, input) {
                        match watermark.baseline_horizon {
                            None => None,
                            Some(horizon) => {
                                let generation = generation_id.as_ref()?;
                                horizon
                                    .validate_for(
                                        generation,
                                        generation_digest,
                                        &scope.head_commit_id,
                                        file_digest,
                                    )
                                    .ok()?;
                                Some(horizon)
                            }
                        }
                    } else {
                        // A changed address must remain observable as stale,
                        // not be rejected as malformed because its baseline
                        // belongs to the newer runtime.
                        None
                    };
                    (watermark.runtime_watermark, horizon)
                }
                FeedbackContentIdentityV1::EphemeralOverlay { .. } => (
                    canonical_sha256(&(
                        OVERLAY_RUNTIME_WATERMARK_DOMAIN,
                        &scope,
                        &content,
                        &configuration_digest,
                        &policy_digest,
                    ))
                    .ok()?,
                    None,
                ),
            };
            let authoritative = FeedbackAuthoritativeRuntimeStateV1 {
                snapshot: FeedbackCycleRuntimeSnapshotV1 {
                    scope,
                    content,
                    policy_digest,
                    configuration_digest,
                },
                baseline_horizon,
                runtime_watermark,
            };
            let runtime = tracedecay_application::feedback::FeedbackRuntimeStateV1::new(
                authoritative,
                generation_id,
            )
            .ok()?;
            runtime.validate_for(input).ok()?;
            Some(runtime)
        })
    }
}

fn admitted(context: &RequestContext, input: &FeedbackEvaluationInputV1) -> bool {
    context.admission_at(input.observed_at) == RequestAdmission::Admitted
}

fn address_matches_input(
    address: &FeedbackRuntimeAddressV1,
    input: &FeedbackEvaluationInputV1,
) -> bool {
    address.scope == input.request.scope
        && address.content == input.request.content
        && address.generation_id == input.target.generation_id
        && address.configuration_digest == input.request.configuration_digest
        && address.policy_digest == input.request.policy_digest
}
