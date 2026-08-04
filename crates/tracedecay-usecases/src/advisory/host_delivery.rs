//! Cross-host projection glue for a completed PR13 advisory publication.
//!
//! This module owns no feedback state, analyzer, suggestion channel, retry
//! loop, or host transport. It mounts the already-authoritative PR12 read
//! owner/store and LSP factory, then emits only a content-free Hook V2 notice
//! that directs a host back to those canonical read surfaces.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::{ApplicationContractError, RequestContext, ResolvedScope};
use tracedecay_domain::feedback::{
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackResultId, FeedbackScopeV1,
};
use tracedecay_domain::{CodeGenerationId, DomainError, ManifestDigest};
use tracedecay_hooks::{
    HookEventEnvelopeV2, HookFeedbackDeliveryOutcomeV1, HookFeedbackDeliveryPortV1,
    HookFeedbackDeliveryRouteV1, HookFeedbackRollbackSwitchV1, HookRuntimeErrorV1,
    HookScopedFeedbackV1, deliver_feedback_with_rollback,
};
use tracedecay_lsp::DaemonLspProviderBundle;

use crate::feedback::concrete::{ConcretePr12FeedbackOwner, ProjectFeedbackStore};
use crate::feedback::observations::{
    Plan26DeliveryRouteV1, Plan26FeedbackObservationEmitterV1, Plan26FeedbackOperationV1,
    Plan26FeedbackOutcomeV1, Plan26FeedbackSourceEventV1, Plan26HookScoutPhaseV1,
};
use crate::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_host_integration::{
    HostCapabilityStateV1, HostCapabilityUnavailableReasonV1, HostCapabilityV1, HostKindV1,
    HostRegistrationRouteV1, stock_host_capabilities, stock_host_registration_evidence,
};

use super::runtime::{
    AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest,
    Pr13AdvisoryDaemonRegistrationV1, Pr13AdvisoryProviderAuthoritiesV1,
    Pr13AdvisoryRuntimeOpenErrorV1, Pr13AdvisoryRuntimeOpenV1,
    open_pr13_advisory_daemon_registration,
};

/// A content-free notification for a host to perform its usual authorized
/// feedback lookup. Finding text, anchors, suggestions, and provider payloads
/// remain in the shared PR12 publication store.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pr13AdvisoryHookLookupNoticeV1 {
    pub scope: FeedbackScopeV1,
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub generation_id: CodeGenerationId,
    pub generation_digest: ManifestDigest,
    pub returned_findings: u64,
    pub omitted_findings: u64,
}

impl Pr13AdvisoryHookLookupNoticeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope.validate()?;
        self.result_id.validate()?;
        self.cycle_id.validate()?;
        self.generation_id.validate()?;
        self.generation_digest.validate()?;
        self.returned_findings
            .checked_add(self.omitted_findings)
            .ok_or(DomainError::NonCanonical {
                field: "advisory hook finding counts",
            })?;
        Ok(())
    }
}

impl HookScopedFeedbackV1 for Pr13AdvisoryHookLookupNoticeV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        self.validate().is_ok()
            && domain_hash16(self.scope.project_id.as_str(), "project") == envelope.project_id
            && domain_hash16(self.scope.repository_id.as_str(), "repository")
                == envelope.repository_id
            && domain_hash16(self.scope.worktree_id.as_str(), "worktree") == envelope.worktree_id
    }
}

fn domain_hash16(value: &str, domain: &str) -> [u8; 16] {
    let digest = Sha256::digest(format!("{domain}:{value}").as_bytes());
    let mut output = [0; 16];
    output.copy_from_slice(&digest[..16]);
    output
}

pub type Pr13AdvisoryHookNoticeSinkV1 =
    dyn Fn(&Pr13AdvisoryHookLookupNoticeV1) -> HookFeedbackDeliveryOutcomeV1 + Send + Sync;

const MAX_PENDING_ADVISORY_HOOK_NOTICES_V1: usize = 32;

/// Bounded daemon-owned handoff between asynchronous advisory publication and
/// the next synchronous Hook V2 lookup. The queue stores only content-free
/// publication identities and rejects scope drift instead of acknowledging a
/// notice that no host can consume.
pub struct Pr13AdvisoryHookNoticeQueueV1 {
    scope: FeedbackScopeV1,
    pending: Mutex<VecDeque<Pr13AdvisoryHookLookupNoticeV1>>,
    recent_results: Mutex<VecDeque<String>>,
}

impl Pr13AdvisoryHookNoticeQueueV1 {
    pub fn new(scope: FeedbackScopeV1) -> Arc<Self> {
        Arc::new(Self {
            scope,
            pending: Mutex::new(VecDeque::new()),
            recent_results: Mutex::new(VecDeque::new()),
        })
    }

    pub fn sink(self: &Arc<Self>) -> Arc<Pr13AdvisoryHookNoticeSinkV1> {
        let queue = Arc::clone(self);
        Arc::new(move |notice| queue.enqueue(notice))
    }

    fn enqueue(&self, notice: &Pr13AdvisoryHookLookupNoticeV1) -> HookFeedbackDeliveryOutcomeV1 {
        if notice.validate().is_err() || !feedback_scope_matches(&self.scope, &notice.scope) {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        }
        let key = notice.result_id.as_str().to_owned();
        let Ok(mut recent_results) = self.recent_results.lock() else {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        };
        if recent_results.contains(&key) {
            return HookFeedbackDeliveryOutcomeV1::Duplicate;
        }
        let Ok(mut pending) = self.pending.lock() else {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        };
        if pending.len() >= MAX_PENDING_ADVISORY_HOOK_NOTICES_V1 {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        }
        if recent_results.len() >= MAX_PENDING_ADVISORY_HOOK_NOTICES_V1 {
            recent_results.pop_front();
        }
        recent_results.push_back(key);
        pending.push_back(notice.clone());
        HookFeedbackDeliveryOutcomeV1::Delivered
    }

    pub fn peek(&self) -> Option<Pr13AdvisoryHookLookupNoticeV1> {
        self.pending.lock().ok()?.front().cloned()
    }

    pub fn acknowledge(&self, notice: &Pr13AdvisoryHookLookupNoticeV1) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        let Some(index) = pending.iter().position(|pending| pending == notice) else {
            return false;
        };
        pending.remove(index);
        true
    }
}

type Pr13AdvisoryHookNoticeQueueMapV1 =
    BTreeMap<([u8; 16], [u8; 16]), Weak<Pr13AdvisoryHookNoticeQueueV1>>;
type Pr13AdvisoryHookNoticeQueuesLockV1 = Mutex<Pr13AdvisoryHookNoticeQueueMapV1>;

fn registered_hook_notice_queues() -> &'static Pr13AdvisoryHookNoticeQueuesLockV1 {
    static QUEUES: OnceLock<Pr13AdvisoryHookNoticeQueuesLockV1> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn register_pr13_advisory_hook_notice_queue(
    project_id: [u8; 16],
    worktree_id: [u8; 16],
    queue: &Arc<Pr13AdvisoryHookNoticeQueueV1>,
) -> bool {
    if project_id == [0; 16] || worktree_id == [0; 16] {
        return false;
    }
    let Ok(mut queues) = registered_hook_notice_queues().lock() else {
        return false;
    };
    let key = (project_id, worktree_id);
    if let Some(existing) = queues.get(&key).and_then(Weak::upgrade) {
        return Arc::ptr_eq(&existing, queue);
    }
    queues.retain(|_, queue| queue.strong_count() > 0);
    queues.insert(key, Arc::downgrade(queue));
    true
}

pub fn peek_pr13_advisory_hook_notice(
    project_id: [u8; 16],
    worktree_id: [u8; 16],
) -> Option<Pr13AdvisoryHookLookupNoticeV1> {
    let queue = registered_hook_notice_queues()
        .lock()
        .ok()?
        .get(&(project_id, worktree_id))?
        .upgrade()?;
    queue.peek()
}

pub fn acknowledge_pr13_advisory_hook_notice(
    project_id: [u8; 16],
    worktree_id: [u8; 16],
    notice: &Pr13AdvisoryHookLookupNoticeV1,
) -> bool {
    let Some(queue) = registered_hook_notice_queues()
        .lock()
        .ok()
        .and_then(|queues| {
            queues
                .get(&(project_id, worktree_id))
                .and_then(Weak::upgrade)
        })
    else {
        return false;
    };
    queue.acknowledge(notice)
}

/// Concrete Hook V2 delivery port. Both routes delegate to the registered host
/// response sinks after exact scope validation; finding content remains in the
/// canonical PR12 store.
pub struct Pr13AdvisoryHookDeliveryPortV1 {
    scope: FeedbackScopeV1,
    hook_v2: Arc<Pr13AdvisoryHookNoticeSinkV1>,
    legacy: Arc<Pr13AdvisoryHookNoticeSinkV1>,
}

impl HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> for Pr13AdvisoryHookDeliveryPortV1 {
    fn deliver_hook_v2(
        &self,
        notice: &Pr13AdvisoryHookLookupNoticeV1,
    ) -> HookFeedbackDeliveryOutcomeV1 {
        if !feedback_scope_matches(&self.scope, &notice.scope) {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        }
        (self.hook_v2)(notice)
    }

    fn deliver_legacy(
        &self,
        notice: &Pr13AdvisoryHookLookupNoticeV1,
    ) -> HookFeedbackDeliveryOutcomeV1 {
        if !feedback_scope_matches(&self.scope, &notice.scope) {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        }
        (self.legacy)(notice)
    }
}

pub fn new_pr13_advisory_hook_delivery_port(
    scope: FeedbackScopeV1,
    hook_v2: Arc<Pr13AdvisoryHookNoticeSinkV1>,
    legacy: Arc<Pr13AdvisoryHookNoticeSinkV1>,
) -> Arc<dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync> {
    Arc::new(Pr13AdvisoryHookDeliveryPortV1 {
        scope,
        hook_v2,
        legacy,
    })
}

/// Host-visible routes assembled only from checked-in registration evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pr13AdvisoryHostDeliveryPathV1 {
    HookV2,
    CustomLsp,
    CursorNativeDiagnostics,
    McpFeedbackRead,
    CliFeedbackRead,
}

/// One truthful per-host route. `state` remains unavailable or degraded when
/// either the host capability matrix or the registration evidence says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Pr13AdvisoryHostDeliveryRouteV1 {
    pub path: Pr13AdvisoryHostDeliveryPathV1,
    pub capability: HostCapabilityV1,
    pub registration: HostRegistrationRouteV1,
    pub state: HostCapabilityStateV1,
}

/// An existing LSP provider bundle plus its truthful host route state.
///
/// The wrapped PR12 factory owns the established snapshot behavior, including
/// monotone diagnostic clear/re-publish. This glue only selects that existing
/// mount; it never creates an analyzer or a competing diagnostic channel.
pub struct Pr13AdvisoryCompletedDeliveryV1 {
    pub lsp_providers: DaemonLspProviderBundle,
    pub hook: Pr13AdvisoryHookDeliveryV1,
}

/// The result of one Hook V2 lookup-notice attempt. The `Unavailable` branch
/// is reserved for a route the checked-in host matrix actually marks
/// unavailable; there is no retry or synthetic fallback.
pub enum Pr13AdvisoryHookDeliveryV1 {
    Delivered {
        state: HostCapabilityStateV1,
        outcome: HookFeedbackDeliveryOutcomeV1,
    },
    SinkUnavailable,
    Unavailable(HostCapabilityUnavailableReasonV1),
}

#[derive(Debug, Error)]
pub enum Pr13AdvisoryHostDeliveryErrorV1 {
    #[error("PR13 advisory cycle did not complete")]
    AdvisoryNotCompleted,
    #[error("PR13 advisory cycle has no recorded shared-store publication")]
    PublicationNotRecorded,
    #[error("completed advisory cycle does not match the shared publication")]
    PublicationMismatch,
    #[error("shared publication does not match the mounted project scope")]
    ScopeMismatch,
    #[error(transparent)]
    Contract(#[from] ApplicationContractError),
    #[error(transparent)]
    Hook(#[from] HookRuntimeErrorV1),
}

#[derive(Debug, Error)]
pub enum Pr13AdvisoryRunErrorV1 {
    #[error(transparent)]
    Cycle(#[from] ApplicationContractError),
    #[error(transparent)]
    Delivery(#[from] Pr13AdvisoryHostDeliveryErrorV1),
}

pub struct Pr13AdvisoryRunResultV1 {
    pub outcome: AdvisoryCycleOutcome,
    pub delivery: Option<Pr13AdvisoryCompletedDeliveryV1>,
}

#[derive(Debug, Error)]
pub enum Pr13AdvisoryDaemonStartupErrorV1 {
    #[error(transparent)]
    Runtime(#[from] Pr13AdvisoryRuntimeOpenErrorV1),
    #[error(transparent)]
    Contract(#[from] ApplicationContractError),
}

/// A composition-only registration. It retains the one PR12 publication
/// authority and the caller-created LSP factory so daemon and host startup can
/// mount their existing transports from one object.
pub struct Pr13AdvisoryHostDeliveryRegistrationV1 {
    pub scope: ResolvedScope,
    pub feedback_owner: Arc<ConcretePr12FeedbackOwner>,
    pub publication_store: ProjectFeedbackStore,
    pub lsp_session_factory: Arc<DaemonLspSessionFactory>,
    pub hook_delivery_port:
        Arc<dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync>,
    pub source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

/// One daemon-startup bundle for PR13 execution and every existing delivery
/// surface. Both members retain handles to the same PR12 owner/store.
pub struct Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC> {
    pub advisory: Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC>,
    pub host_delivery: Pr13AdvisoryHostDeliveryRegistrationV1,
}

impl<GR, GA, CS, CE, PE, PC> Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC>
where
    GR: super::GitHubCurrentBranchRemapper + Sync,
    GA: super::GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: super::CiReadOnlyProviderArchiveV1 + Sync,
    CE: super::CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: super::CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: crate::configuration::ConfigurationControlStore + Clone + Send + 'static,
{
    pub fn runtime(&self) -> &super::Pr13AdvisoryRuntime<GR, GA, CS, CE, PE, PC> {
        &self.advisory.advisory
    }

    /// Production handoff called with the exact outcome returned by
    /// `Pr13AdvisoryRuntime::run_once`.
    pub fn consume_completed_publication(
        &self,
        host: HostKindV1,
        outcome: &AdvisoryCycleOutcome,
        rollback: HookFeedbackRollbackSwitchV1,
    ) -> Result<Pr13AdvisoryCompletedDeliveryV1, Pr13AdvisoryHostDeliveryErrorV1> {
        self.host_delivery
            .consume_completed_publication(host, outcome, rollback)
    }

    /// Production one-shot execution. Only an atomically recorded publication
    /// reaches the registered Hook and LSP consumers.
    pub async fn run_once(
        &self,
        context: &RequestContext,
        control: AdvisoryCycleControl,
        request: AdvisoryCycleRequest,
        host: HostKindV1,
        rollback: HookFeedbackRollbackSwitchV1,
    ) -> Result<Pr13AdvisoryRunResultV1, Pr13AdvisoryRunErrorV1> {
        let outcome = Box::pin(self.runtime().run_once(context, control, request)).await?;
        let delivery = if outcome.publication().is_some() {
            Some(self.consume_completed_publication(host, &outcome, rollback)?)
        } else {
            None
        };
        Ok(Pr13AdvisoryRunResultV1 { outcome, delivery })
    }
}

impl Pr13AdvisoryHostDeliveryRegistrationV1 {
    /// Returns one host's routes without assuming that an unavailable route is
    /// usable just because another host supports it.
    pub fn host_routes(&self, host: HostKindV1) -> Vec<Pr13AdvisoryHostDeliveryRouteV1> {
        stock_host_registration_evidence(host)
            .into_iter()
            .map(|evidence| {
                let capability = capability_for_registration(evidence.route);
                Pr13AdvisoryHostDeliveryRouteV1 {
                    path: path_for_registration(evidence.route),
                    capability,
                    registration: evidence.route,
                    state: effective_state(capability_state(host, capability), evidence.state),
                }
            })
            .collect()
    }

    /// Validates a completed PR13 advisory outcome against one canonical PR12
    /// publication, then builds the bounded content-free Hook V2 lookup
    /// notice. The outcome exposes the publication only after the shared store
    /// committed it; this method never reconstructs, polls, or persists it.
    pub fn hook_lookup_notice(
        &self,
        outcome: &AdvisoryCycleOutcome,
    ) -> Result<Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryHostDeliveryErrorV1> {
        let AdvisoryCycleOutcome::Completed {
            cycle,
            observation_input,
            ..
        } = outcome
        else {
            return Err(Pr13AdvisoryHostDeliveryErrorV1::AdvisoryNotCompleted);
        };
        let publication = outcome
            .publication()
            .ok_or(Pr13AdvisoryHostDeliveryErrorV1::PublicationNotRecorded)?;
        publication.validate()?;

        if cycle.dedupe_key.as_ref() != Some(&publication.dedupe_key)
            || cycle.cycle.result_id != publication.result.result_id
            || cycle.cycle.cycle_id != publication.result.cycle_id
            || !feedback_scope_matches(&cycle.cycle.scope, &publication.result.scope)
        {
            return Err(Pr13AdvisoryHostDeliveryErrorV1::PublicationMismatch);
        }
        if !resolved_scope_matches_feedback_scope(&self.scope, &publication.result.scope)
            || !resolved_scope_matches_feedback_scope(
                &publication.authorized_scope,
                &publication.result.scope,
            )
        {
            return Err(Pr13AdvisoryHostDeliveryErrorV1::ScopeMismatch);
        }
        let generation_id = publication
            .input
            .target
            .generation_id
            .clone()
            .ok_or(Pr13AdvisoryHostDeliveryErrorV1::PublicationMismatch)?;
        let FeedbackContentIdentityV1::SavedContent {
            generation_digest, ..
        } = &publication.input.request.content
        else {
            return Err(Pr13AdvisoryHostDeliveryErrorV1::PublicationMismatch);
        };
        self.source_observations.observe_source_event(
            observation_input,
            Plan26FeedbackSourceEventV1::Truncation {
                operation: Plan26FeedbackOperationV1::FeedbackCycle,
                returned_count: publication
                    .result
                    .returned_findings
                    .try_into()
                    .unwrap_or(u32::MAX),
                omitted_count: publication
                    .result
                    .omitted_findings
                    .try_into()
                    .unwrap_or(u32::MAX),
            },
        );

        Ok(Pr13AdvisoryHookLookupNoticeV1 {
            scope: publication.result.scope.clone(),
            result_id: publication.result.result_id.clone(),
            cycle_id: publication.result.cycle_id.clone(),
            generation_id,
            generation_digest: generation_digest.clone(),
            returned_findings: publication.result.returned_findings,
            omitted_findings: publication.result.omitted_findings,
        })
    }

    /// Performs exactly one configured Hook V2 delivery. A delivery port owns
    /// the host transport; this composition layer only sends the canonical
    /// lookup notice and propagates its terminal outcome.
    pub fn deliver_hook_lookup_notice<P>(
        &self,
        host: HostKindV1,
        outcome: &AdvisoryCycleOutcome,
        rollback: HookFeedbackRollbackSwitchV1,
        port: &P,
    ) -> Result<Pr13AdvisoryHookDeliveryV1, Pr13AdvisoryHostDeliveryErrorV1>
    where
        P: HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + ?Sized,
    {
        let AdvisoryCycleOutcome::Completed {
            observation_input, ..
        } = outcome
        else {
            return Err(Pr13AdvisoryHostDeliveryErrorV1::AdvisoryNotCompleted);
        };
        let delivery_route = match rollback.route {
            HookFeedbackDeliveryRouteV1::HookV2 => Plan26DeliveryRouteV1::HookV2,
            HookFeedbackDeliveryRouteV1::Legacy => Plan26DeliveryRouteV1::HookLegacy,
        };
        let Some(route) = self
            .host_routes(host)
            .into_iter()
            .find(|route| route.path == Pr13AdvisoryHostDeliveryPathV1::HookV2)
        else {
            self.observe_hook_delivery(
                observation_input,
                delivery_route,
                Plan26HookScoutPhaseV1::Admission,
                Plan26FeedbackOutcomeV1::Unavailable,
                rollback.route == HookFeedbackDeliveryRouteV1::Legacy,
                0,
            );
            return Ok(Pr13AdvisoryHookDeliveryV1::Unavailable(
                HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported,
            ));
        };
        if let HostCapabilityStateV1::Unavailable(reason) = route.state {
            self.observe_hook_delivery(
                observation_input,
                delivery_route,
                Plan26HookScoutPhaseV1::Admission,
                Plan26FeedbackOutcomeV1::Unavailable,
                rollback.route == HookFeedbackDeliveryRouteV1::Legacy,
                0,
            );
            return Ok(Pr13AdvisoryHookDeliveryV1::Unavailable(reason));
        }

        let notice = self.hook_lookup_notice(outcome)?;
        self.observe_hook_delivery(
            observation_input,
            delivery_route,
            Plan26HookScoutPhaseV1::Admission,
            Plan26FeedbackOutcomeV1::Admitted,
            rollback.route == HookFeedbackDeliveryRouteV1::Legacy,
            1,
        );
        let outcome = deliver_feedback_with_rollback(rollback, &notice, port)?;
        let observed_outcome = match outcome {
            HookFeedbackDeliveryOutcomeV1::Delivered => Plan26FeedbackOutcomeV1::Completed,
            HookFeedbackDeliveryOutcomeV1::Duplicate => Plan26FeedbackOutcomeV1::Duplicate,
            HookFeedbackDeliveryOutcomeV1::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
        };
        self.observe_hook_delivery(
            observation_input,
            delivery_route,
            Plan26HookScoutPhaseV1::Delivery,
            observed_outcome,
            rollback.route == HookFeedbackDeliveryRouteV1::Legacy,
            1,
        );
        self.observe_hook_delivery(
            observation_input,
            delivery_route,
            Plan26HookScoutPhaseV1::FeedbackTerminal,
            observed_outcome,
            rollback.route == HookFeedbackDeliveryRouteV1::Legacy,
            1,
        );
        match outcome {
            HookFeedbackDeliveryOutcomeV1::Delivered | HookFeedbackDeliveryOutcomeV1::Duplicate => {
                Ok(Pr13AdvisoryHookDeliveryV1::Delivered {
                    state: route.state,
                    outcome,
                })
            }
            HookFeedbackDeliveryOutcomeV1::Unavailable => {
                Ok(Pr13AdvisoryHookDeliveryV1::SinkUnavailable)
            }
        }
    }

    fn observe_hook_delivery(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        route: Plan26DeliveryRouteV1,
        phase: Plan26HookScoutPhaseV1,
        outcome: Plan26FeedbackOutcomeV1,
        rollback: bool,
        item_count: u32,
    ) {
        self.source_observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::HookScout {
                route,
                phase,
                outcome,
                item_count,
                duration_micros: None,
            },
        );
        if phase == Plan26HookScoutPhaseV1::Delivery {
            self.source_observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::HostDelivery {
                    route,
                    outcome,
                    rollback,
                    item_count,
                    duration_micros: None,
                },
            );
        }
    }

    pub fn deliver_registered_hook_lookup_notice(
        &self,
        host: HostKindV1,
        outcome: &AdvisoryCycleOutcome,
        rollback: HookFeedbackRollbackSwitchV1,
    ) -> Result<Pr13AdvisoryHookDeliveryV1, Pr13AdvisoryHostDeliveryErrorV1> {
        self.deliver_hook_lookup_notice(host, outcome, rollback, self.hook_delivery_port.as_ref())
    }

    /// Consumes one recorded publication through both live delivery paths.
    /// Hook delivery executes immediately; the returned LSP provider bundle
    /// reads the same PR12 publication store through its existing projection
    /// authority and is retained by the caller/session registry.
    pub fn consume_completed_publication(
        &self,
        host: HostKindV1,
        outcome: &AdvisoryCycleOutcome,
        rollback: HookFeedbackRollbackSwitchV1,
    ) -> Result<Pr13AdvisoryCompletedDeliveryV1, Pr13AdvisoryHostDeliveryErrorV1> {
        let hook = self.deliver_registered_hook_lookup_notice(host, outcome, rollback)?;
        Ok(Pr13AdvisoryCompletedDeliveryV1 {
            lsp_providers: self.lsp_session_factory.provider_bundle(),
            hook,
        })
    }
}

/// Builds the concrete host-delivery registration from the PR13 daemon
/// registration's existing read owner/store. `scope` is passed explicitly
/// because it is the startup authority that admitted the daemon registration.
pub fn mount_pr13_advisory_host_delivery<GR, GA, CS, CE, PE, PC>(
    scope: ResolvedScope,
    registration: &Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC>,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
    hook_delivery_port: Arc<
        dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync,
    >,
) -> Pr13AdvisoryHostDeliveryRegistrationV1 {
    Pr13AdvisoryHostDeliveryRegistrationV1 {
        scope,
        feedback_owner: Arc::clone(&registration.feedback_owner),
        publication_store: registration.publication_store.clone(),
        lsp_session_factory,
        hook_delivery_port,
        source_observations: Arc::clone(&registration.source_observations),
    }
}

/// Exact daemon-startup composition call. The provider authorities are real
/// injected owners; this function adds no fallback fixture or duplicate store.
pub fn register_pr13_advisory_daemon_startup<GR, GA, CS, CE, PE, PC>(
    input: Pr13AdvisoryRuntimeOpenV1,
    providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
    hook_delivery_port: Arc<
        dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync,
    >,
) -> Result<
    Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC>,
    Pr13AdvisoryDaemonStartupErrorV1,
>
where
    GR: super::GitHubCurrentBranchRemapper + Sync,
    GA: super::GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: super::CiReadOnlyProviderArchiveV1 + Sync,
    CE: super::CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: super::CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: crate::configuration::ConfigurationControlStore + Clone + Send + 'static,
{
    let scope = input.resolved_scope.clone();
    let advisory = open_pr13_advisory_daemon_registration(input, providers)?;
    let host_delivery = mount_pr13_advisory_host_delivery(
        scope,
        &advisory,
        lsp_session_factory,
        hook_delivery_port,
    );
    Ok(Pr13AdvisoryDaemonStartupRegistrationV1 {
        advisory,
        host_delivery,
    })
}

fn capability_for_registration(route: HostRegistrationRouteV1) -> HostCapabilityV1 {
    match route {
        HostRegistrationRouteV1::ClaudeConfiguredLanguageLsp
        | HostRegistrationRouteV1::OpenCodeCustomLsp => HostCapabilityV1::Lsp,
        HostRegistrationRouteV1::CursorNativeDiagnostics => HostCapabilityV1::NativeDiagnostics,
        HostRegistrationRouteV1::Hook => HostCapabilityV1::Hooks,
        HostRegistrationRouteV1::Mcp => HostCapabilityV1::Mcp,
        HostRegistrationRouteV1::Cli => HostCapabilityV1::Cli,
    }
}

fn path_for_registration(route: HostRegistrationRouteV1) -> Pr13AdvisoryHostDeliveryPathV1 {
    match route {
        HostRegistrationRouteV1::ClaudeConfiguredLanguageLsp
        | HostRegistrationRouteV1::OpenCodeCustomLsp => Pr13AdvisoryHostDeliveryPathV1::CustomLsp,
        HostRegistrationRouteV1::CursorNativeDiagnostics => {
            Pr13AdvisoryHostDeliveryPathV1::CursorNativeDiagnostics
        }
        HostRegistrationRouteV1::Hook => Pr13AdvisoryHostDeliveryPathV1::HookV2,
        HostRegistrationRouteV1::Mcp => Pr13AdvisoryHostDeliveryPathV1::McpFeedbackRead,
        HostRegistrationRouteV1::Cli => Pr13AdvisoryHostDeliveryPathV1::CliFeedbackRead,
    }
}

fn capability_state(host: HostKindV1, capability: HostCapabilityV1) -> HostCapabilityStateV1 {
    stock_host_capabilities(host)
        .into_iter()
        .find(|record| record.capability == capability)
        .map_or(
            HostCapabilityStateV1::Unavailable(
                HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported,
            ),
            |record| record.state,
        )
}

fn effective_state(
    capability: HostCapabilityStateV1,
    registration: HostCapabilityStateV1,
) -> HostCapabilityStateV1 {
    match (capability, registration) {
        (HostCapabilityStateV1::Unavailable(reason), _)
        | (_, HostCapabilityStateV1::Unavailable(reason)) => {
            HostCapabilityStateV1::Unavailable(reason)
        }
        (HostCapabilityStateV1::Degraded(reason), _)
        | (_, HostCapabilityStateV1::Degraded(reason)) => HostCapabilityStateV1::Degraded(reason),
        _ => HostCapabilityStateV1::Supported,
    }
}

fn resolved_scope_matches_feedback_scope(
    scope: &ResolvedScope,
    feedback_scope: &FeedbackScopeV1,
) -> bool {
    scope.project_id == feedback_scope.project_id
        && scope.repository_id == feedback_scope.repository_id
        && scope.worktree_id == feedback_scope.worktree_id
        && scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(feedback_scope.branch_ref.as_str())
}

fn feedback_scope_matches(left: &FeedbackScopeV1, right: &FeedbackScopeV1) -> bool {
    left.project_id == right.project_id
        && left.repository_id == right.repository_id
        && left.worktree_id == right.worktree_id
        && left.branch_ref == right.branch_ref
        && left.head_commit_id == right.head_commit_id
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{CommitId, ProjectId, RepositoryId, WorktreeId};

    use super::*;

    fn scope(branch: &str) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.notice-queue").unwrap(),
            repository_id: RepositoryId::new("repository.notice-queue").unwrap(),
            worktree_id: WorktreeId::new("worktree.notice-queue").unwrap(),
            branch_ref: branch.to_owned(),
            head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
        }
    }

    fn notice(scope: FeedbackScopeV1, suffix: &str) -> Pr13AdvisoryHookLookupNoticeV1 {
        Pr13AdvisoryHookLookupNoticeV1 {
            scope,
            result_id: FeedbackResultId::new(format!("result.{suffix}")).unwrap(),
            cycle_id: FeedbackCycleId::new(format!("cycle.{suffix}")).unwrap(),
            generation_id: CodeGenerationId::new(format!("generation.{suffix}")).unwrap(),
            generation_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            returned_findings: 1,
            omitted_findings: 0,
        }
    }

    #[test]
    fn notice_queue_retries_after_timeout_until_authenticated_acknowledgement() {
        let expected = scope("refs/heads/main");
        let queue = Pr13AdvisoryHookNoticeQueueV1::new(expected.clone());
        let sink = queue.sink();
        let accepted = notice(expected, "accepted");

        assert_eq!(sink(&accepted), HookFeedbackDeliveryOutcomeV1::Delivered);
        assert_eq!(sink(&accepted), HookFeedbackDeliveryOutcomeV1::Duplicate);
        assert_eq!(queue.peek(), Some(accepted.clone()));
        assert_eq!(queue.peek(), Some(accepted.clone()));
        assert!(queue.acknowledge(&accepted));
        assert_eq!(queue.peek(), None);
        assert!(!queue.acknowledge(&accepted));

        let wrong_scope = notice(scope("refs/heads/other"), "wrong-scope");
        assert_eq!(
            sink(&wrong_scope),
            HookFeedbackDeliveryOutcomeV1::Unavailable
        );

        for index in 0..MAX_PENDING_ADVISORY_HOOK_NOTICES_V1 {
            assert_eq!(
                sink(&notice(
                    scope("refs/heads/main"),
                    &format!("bounded-{index}")
                )),
                HookFeedbackDeliveryOutcomeV1::Delivered
            );
        }
        assert_eq!(
            sink(&notice(scope("refs/heads/main"), "over-capacity")),
            HookFeedbackDeliveryOutcomeV1::Unavailable
        );
    }

    #[test]
    fn notice_registry_is_exact_per_worktree_and_rejects_live_rebinding() {
        let project = [1; 16];
        let first_worktree = [2; 16];
        let second_worktree = [3; 16];
        let first = Pr13AdvisoryHookNoticeQueueV1::new(scope("refs/heads/main"));
        let conflicting = Pr13AdvisoryHookNoticeQueueV1::new(scope("refs/heads/main"));
        let second = Pr13AdvisoryHookNoticeQueueV1::new(scope("refs/heads/other"));

        assert!(register_pr13_advisory_hook_notice_queue(
            project,
            first_worktree,
            &first
        ));
        assert!(register_pr13_advisory_hook_notice_queue(
            project,
            first_worktree,
            &first
        ));
        assert!(!register_pr13_advisory_hook_notice_queue(
            project,
            first_worktree,
            &conflicting
        ));
        assert!(register_pr13_advisory_hook_notice_queue(
            project,
            second_worktree,
            &second
        ));

        let first_notice = notice(scope("refs/heads/main"), "first-worktree");
        let second_notice = notice(scope("refs/heads/other"), "second-worktree");
        assert_eq!(
            first.sink()(&first_notice),
            HookFeedbackDeliveryOutcomeV1::Delivered
        );
        assert_eq!(
            second.sink()(&second_notice),
            HookFeedbackDeliveryOutcomeV1::Delivered
        );
        assert_eq!(
            peek_pr13_advisory_hook_notice(project, first_worktree),
            Some(first_notice.clone())
        );
        assert_eq!(
            peek_pr13_advisory_hook_notice(project, second_worktree),
            Some(second_notice.clone())
        );
        assert!(acknowledge_pr13_advisory_hook_notice(
            project,
            first_worktree,
            &first_notice
        ));
        assert_eq!(
            peek_pr13_advisory_hook_notice(project, first_worktree),
            None
        );
        assert_eq!(
            peek_pr13_advisory_hook_notice(project, second_worktree),
            Some(second_notice)
        );
    }
}
