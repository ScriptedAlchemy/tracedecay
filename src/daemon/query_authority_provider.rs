//! Daemon-owned query activation and durable cursor-key authority.
//!
//! This provider never chooses retrieval weights, calibration, diversity, or
//! evaluation identity. It exposes only the exact profile already accepted by
//! [`RetrievalProfileStateV1`] after a successful configuration activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    ComponentRevision, ManifestDigest, PrivacyDomainId, RetrievalAnchorId, RetrieverKind,
};

use super::code_index_scheduler::query_runtime::{
    AcceptedQueryEvaluationV1, QueryAuthorityMaterialV1, QueryAuthorityProviderErrorV1,
    QueryAuthorityProviderV1,
};
use crate::application::semantic_runtime::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticRuntimeFuture,
    prepare_project_semantic_redundancy_authority, project_semantic_production_runtime,
    project_semantic_retained_code_generation,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileAuditOperationV1, RetrievalProfileStateV1,
};
use tracedecay_query::retrieval::QueryAuthorityV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryAuthorityUnavailableReasonV1 {
    ActivationUnavailable,
    ActivationNotCurrent,
    ScopeRequired,
    ScopeMismatch,
    KeyUnavailable,
    InvalidActivatedProfile,
    AmbiguousActivatedProfile,
}

impl QueryAuthorityUnavailableReasonV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::ActivationUnavailable => "activation_unavailable",
            Self::ActivationNotCurrent => "activation_not_current",
            Self::ScopeRequired => "scope_required",
            Self::ScopeMismatch => "scope_mismatch",
            Self::KeyUnavailable => "key_unavailable",
            Self::InvalidActivatedProfile => "invalid_activated_profile",
            Self::AmbiguousActivatedProfile => "ambiguous_activated_profile",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryAuthorityProviderStatusV1 {
    Available {
        scope_digest: ManifestDigest,
        profile_id: tracedecay_domain::FusionProfileId,
        evaluation_anchor: RetrievalAnchorId,
    },
    Unavailable {
        reason: QueryAuthorityUnavailableReasonV1,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum QueryAuthorityUpdateErrorV1 {
    #[error("query activated scope is invalid")]
    InvalidScope,
    #[error("query initial profile state is not the exact evaluated fallback")]
    InvalidInitialState,
    #[error("query profile state does not contain a successful current activation")]
    ActivationNotCurrent,
    #[error("query activation does not match the provider's exact current scope")]
    ScopeMismatch,
    #[error("query activation compare-and-swap state is stale")]
    CasConflict,
}

#[derive(Clone)]
struct ActivatedQueryStateV1 {
    scope: ResolvedScope,
    state: RetrievalProfileStateV1,
    cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
}

pub(crate) struct PreparedQueryActivationV1 {
    scope: ResolvedScope,
    activated: RetrievalProfileStateV1,
    cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    query_authority: Arc<QueryAuthorityV1>,
}

impl PreparedQueryActivationV1 {
    pub(crate) fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub(crate) fn configuration_revision(
        &self,
    ) -> &tracedecay_domain::configuration::ConfigurationRevisionId {
        self.activated.configuration_revision()
    }

    pub(crate) fn base_configuration_revision(
        &self,
    ) -> Option<&tracedecay_domain::configuration::ConfigurationRevisionId> {
        current_transition(&self.activated).map(|event| &event.base_revision)
    }

    pub(crate) fn query_authority(&self) -> &Arc<QueryAuthorityV1> {
        &self.query_authority
    }
}

/// Daemon owner for the current accepted query profile and the
/// durable project cursor-key authority loaded from its registered store.
#[derive(Clone)]
pub(crate) struct DaemonQueryAuthorityProviderV1 {
    activated: Arc<RwLock<BTreeMap<ManifestDigest, ActivatedQueryStateV1>>>,
}

#[derive(Clone)]
pub(crate) struct DaemonQueryActivationRegistrarV1 {
    provider: DaemonQueryAuthorityProviderV1,
    registry: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: std::path::PathBuf,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
}

impl DaemonQueryActivationRegistrarV1 {
    pub(crate) fn new(
        provider: DaemonQueryAuthorityProviderV1,
        registry: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        project_root: std::path::PathBuf,
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> Self {
        Self {
            provider,
            registry,
            project_root,
            session_db,
        }
    }
}

impl RetrievalProfileActivationObserverV1 for DaemonQueryActivationRegistrarV1 {
    fn activation_committed(
        &self,
        committed: CommittedRetrievalProfileStateV1,
    ) -> SemanticRuntimeFuture<'_, Result<(), RetrievalProfileActivationObserverErrorV1>> {
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        let project_root = self.project_root.clone();
        let session_db = Arc::clone(&self.session_db);
        Box::pin(async move {
            let scope = committed.scope.clone();
            let semantic_enabled = committed.state.active().compatibility().semantic.is_some();
            let committed_epoch = committed.epoch;
            let transition_digest = committed.transition_digest.clone();
            let result_revision = committed.state.configuration_revision().clone();
            let active_semantic_generation = committed
                .state
                .active()
                .compatibility()
                .semantic
                .as_ref()
                .map(|pins| pins.vector_generation_id.clone());
            let rollback_semantic_generation = committed
                .state
                .rollback_profile()
                .and_then(|profile| profile.compatibility().semantic.as_ref())
                .map(|pins| pins.vector_generation_id.clone());
            let prepared_redundancy = prepare_project_semantic_redundancy_authority(&committed);
            let failed_redundancy = prepared_redundancy.clone();
            let attempt = registry
                .begin_committed_query_activation(
                    &project_root,
                    &scope,
                    committed_epoch,
                    &result_revision,
                    &transition_digest,
                    &prepared_redundancy,
                )
                .await
                .map_err(|_| RetrievalProfileActivationObserverErrorV1::Conflict)?;
            let observed = async {
                let redundancy_ready = prepared_redundancy.has_active_authority();
                let prepared_cache = if semantic_enabled {
                    let pins = committed
                        .current_activation
                        .as_ref()
                        .map(|activation| &activation.compatibility)
                        .ok_or(RetrievalProfileActivationObserverErrorV1::Rejected)?;
                    if !redundancy_ready {
                        return Err(RetrievalProfileActivationObserverErrorV1::Rejected);
                    }
                    let runtime = project_semantic_production_runtime(&project_root)
                        .ok_or(RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                    let vectors = runtime
                        .active_vector_generation(pins)
                        .await
                        .ok_or(RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                    let source_generation = vectors.source_generation().clone();
                    if !runtime.cache_ready_for(pins, &source_generation) {
                        let code = project_semantic_retained_code_generation(
                            &project_root,
                            &source_generation,
                        )
                        .ok_or(RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                        Some(
                            runtime
                                .prepare_restore_current(&code, &pins.vector_generation_id)
                                .await
                                .map_err(|_| {
                                    RetrievalProfileActivationObserverErrorV1::Unavailable
                                })?
                                .ok_or(
                                    RetrievalProfileActivationObserverErrorV1::Unavailable,
                                )?,
                        )
                    } else {
                        Some(
                            runtime
                                .prepare_current_cache_observation(pins, &source_generation)
                                .ok_or(
                                    RetrievalProfileActivationObserverErrorV1::Unavailable,
                                )?,
                        )
                    }
                } else {
                    None
                };
                let serving = registry
                    .serving_code_scope(&project_root)
                    .await
                    .ok_or(RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                if serving.repository_id != scope.repository_id
                    || serving.worktree_id != scope.worktree_id
                {
                    return Err(RetrievalProfileActivationObserverErrorV1::Rejected);
                }
                let generation = serving
                    .serving_generation
                    .ok_or(RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                let cursor_keys = Arc::new(
                    session_db
                        .load_session_cursor_key_provider_result()
                        .await
                        .map_err(|_| RetrievalProfileActivationObserverErrorV1::Unavailable)?,
                );
                let prepared = provider
                    .prepare_after_successful_activation(
                        scope.clone(),
                        committed.state.clone(),
                        cursor_keys,
                        &generation.manifest().privacy_domain,
                    )
                    .map_err(map_update_observer_error)?;
                let semantic_authority = semantic_enabled
                    .then(|| {
                        super::code_index_scheduler::semantic_query_runtime::SemanticQueryAuthorityV1::from_committed(
                            committed.clone(),
                        )
                    })
                    .transpose()
                    .map_err(|_| RetrievalProfileActivationObserverErrorV1::Rejected)?
                    .map(Arc::new);
                registry
                    .install_committed_query_authorities(
                        &project_root,
                        &scope,
                        &provider,
                        prepared,
                        semantic_authority,
                        prepared_cache,
                        rollback_semantic_generation.as_ref(),
                        prepared_redundancy,
                        &attempt,
                    )
                    .await
                    .map_err(|_| RetrievalProfileActivationObserverErrorV1::Conflict)?;
                Ok(())
            }
            .await;
            if observed.is_err() {
                let cache_generation = active_semantic_generation
                    .as_ref()
                    .or(rollback_semantic_generation.as_ref());
                registry
                    .clear_failed_query_activation(
                        &project_root,
                        &scope,
                        cache_generation,
                        failed_redundancy,
                        &attempt,
                    )
                    .await
                    .map_err(|_| RetrievalProfileActivationObserverErrorV1::Conflict)?;
            }
            observed
        })
    }
}

impl Default for DaemonQueryAuthorityProviderV1 {
    fn default() -> Self {
        Self {
            activated: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl fmt::Debug for DaemonQueryAuthorityProviderV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonQueryAuthorityProviderV1")
            .field(
                "activated_scope_count",
                &self
                    .activated
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
            )
            .field("key_material", &"REDACTED")
            .finish()
    }
}

impl DaemonQueryAuthorityProviderV1 {
    pub(crate) fn retire_project(&self, project_id: &tracedecay_domain::ProjectId) {
        let mut activated = match self.activated.write() {
            Ok(activated) => activated,
            Err(poisoned) => poisoned.into_inner(),
        };
        activated.retain(|_, activated| &activated.scope.project_id != project_id);
    }

    pub(crate) fn prepare_after_successful_activation(
        &self,
        scope: ResolvedScope,
        activated: RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<PreparedQueryActivationV1, QueryAuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidScope)?;
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !current
            .get(&scope.scope_digest)
            .is_some_and(|installed| installed.scope == scope && installed.state == activated)
        {
            validate_successful_activation_update(&current, &scope, &activated)?;
        }
        let candidate = ActivatedQueryStateV1 {
            scope: scope.clone(),
            state: activated.clone(),
            cursor_keys: Arc::clone(&cursor_keys),
        };
        let material = query_material_for_activated(&candidate, privacy_domain)
            .map_err(map_unavailable_update_error)?;
        let query_authority = Arc::new(
            QueryAuthorityV1::new(
                material.profile,
                material.diversity,
                material.ranking_revision,
                material
                    .keyring
                    .ok_or(QueryAuthorityUpdateErrorV1::ActivationNotCurrent)?,
            )
            .map_err(|_| QueryAuthorityUpdateErrorV1::ActivationNotCurrent)?,
        );
        Ok(PreparedQueryActivationV1 {
            scope,
            activated,
            cursor_keys,
            query_authority,
        })
    }

    pub(crate) fn commit_prepared_activation(
        &self,
        prepared: &PreparedQueryActivationV1,
    ) -> Result<(), QueryAuthorityUpdateErrorV1> {
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current
            .get(&prepared.scope.scope_digest)
            .is_some_and(|installed| {
                installed.scope == prepared.scope && installed.state == prepared.activated
            })
        {
            return Ok(());
        }
        validate_successful_activation_update(&current, &prepared.scope, &prepared.activated)?;
        current.insert(
            prepared.scope.scope_digest.clone(),
            ActivatedQueryStateV1 {
                scope: prepared.scope.clone(),
                state: prepared.activated.clone(),
                cursor_keys: Arc::clone(&prepared.cursor_keys),
            },
        );
        Ok(())
    }

    /// Restore the evaluated fallback installed as the configuration store's
    /// initial state. Initial installation has no mutation audit event, so it
    /// is admitted only while the exact query profile is active with no rollback
    /// slot or audit history.
    pub(crate) fn install_evaluated_initial_state(
        &self,
        scope: ResolvedScope,
        initial: RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    ) -> Result<QueryAuthorityProviderStatusV1, QueryAuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidScope)?;
        if !initial.audit().is_empty()
            || initial.rollback_profile().is_some()
            || exact_query_profile(&initial).is_err()
        {
            return Err(QueryAuthorityUpdateErrorV1::InvalidInitialState);
        }
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prior) = current.get(&scope.scope_digest) {
            if prior.scope != scope {
                return Err(QueryAuthorityUpdateErrorV1::ScopeMismatch);
            }
            if prior.state != initial {
                return Err(QueryAuthorityUpdateErrorV1::CasConflict);
            }
        }
        current.insert(
            scope.scope_digest.clone(),
            ActivatedQueryStateV1 {
                scope: scope.clone(),
                state: initial,
                cursor_keys,
            },
        );
        drop(current);
        Ok(self.status(Some(&scope)))
    }

    /// Publish a state only after its configuration activation succeeded.
    ///
    /// Subsequent publications are compare-and-swapped against the previous
    /// active profile digest and exact scope captured by the activation event.
    pub(crate) fn update_after_successful_activation(
        &self,
        scope: ResolvedScope,
        activated: RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    ) -> Result<QueryAuthorityProviderStatusV1, QueryAuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidScope)?;
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_successful_activation_update(&current, &scope, &activated)?;
        current.insert(
            scope.scope_digest.clone(),
            ActivatedQueryStateV1 {
                scope: scope.clone(),
                state: activated,
                cursor_keys,
            },
        );
        drop(current);
        Ok(self.status(Some(&scope)))
    }

    pub(crate) fn status(&self, scope: Option<&ResolvedScope>) -> QueryAuthorityProviderStatusV1 {
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope) = scope else {
            return if current.is_empty() {
                unavailable(QueryAuthorityUnavailableReasonV1::ActivationUnavailable)
            } else {
                unavailable(QueryAuthorityUnavailableReasonV1::ScopeRequired)
            };
        };
        let Some(activated) = current.get(&scope.scope_digest) else {
            return unavailable(QueryAuthorityUnavailableReasonV1::ActivationUnavailable);
        };
        if scope != &activated.scope {
            return unavailable(QueryAuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if !has_current_query_authority(&activated.state) {
            return unavailable(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        let profile = match exact_query_profile(&activated.state) {
            Ok(profile) => profile,
            Err(reason) => return unavailable(reason),
        };
        QueryAuthorityProviderStatusV1::Available {
            scope_digest: activated.scope.scope_digest.clone(),
            profile_id: profile.profile().profile_id.clone(),
            evaluation_anchor: profile.profile().evaluation_result_anchor.clone(),
        }
    }

    fn material_for(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<QueryAuthorityMaterialV1, QueryAuthorityUnavailableReasonV1> {
        match self.status(Some(scope)) {
            QueryAuthorityProviderStatusV1::Available { .. } => {}
            QueryAuthorityProviderStatusV1::Unavailable { reason } => return Err(reason),
        }
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let activated = current
            .get(&scope.scope_digest)
            .ok_or(QueryAuthorityUnavailableReasonV1::ActivationUnavailable)?;
        if &activated.scope != scope {
            return Err(QueryAuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if !has_current_query_authority(&activated.state) {
            return Err(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        query_material_for_activated(activated, privacy_domain)
    }
}

fn validate_successful_activation_update(
    current: &BTreeMap<ManifestDigest, ActivatedQueryStateV1>,
    scope: &ResolvedScope,
    activated: &RetrievalProfileStateV1,
) -> Result<(), QueryAuthorityUpdateErrorV1> {
    let event =
        current_transition(activated).ok_or(QueryAuthorityUpdateErrorV1::ActivationNotCurrent)?;
    if activated.configuration_revision() != &event.result_revision {
        return Err(QueryAuthorityUpdateErrorV1::ActivationNotCurrent);
    }
    if let Some(prior) = current.get(&scope.scope_digest) {
        if prior.scope != *scope {
            return Err(QueryAuthorityUpdateErrorV1::ScopeMismatch);
        }
        if prior.state.configuration_revision() != &event.base_revision
            || prior.state.active().profile_digest() != &event.prior_active_digest
        {
            return Err(QueryAuthorityUpdateErrorV1::CasConflict);
        }
    }
    Ok(())
}

fn query_material_for_activated(
    activated: &ActivatedQueryStateV1,
    privacy_domain: &PrivacyDomainId,
) -> Result<QueryAuthorityMaterialV1, QueryAuthorityUnavailableReasonV1> {
    let query = exact_query_profile(&activated.state)?;
    let ranking_revision =
        ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
            .map_err(|_| QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile)?;
    let keyring = activated
        .cursor_keys
        .retrieval_keyring(privacy_domain.clone())
        .map_err(|_| QueryAuthorityUnavailableReasonV1::KeyUnavailable)?;
    Ok(QueryAuthorityMaterialV1 {
        scope: activated.scope.clone(),
        evaluation: AcceptedQueryEvaluationV1 {
            status: crate::search_eval::DirectEvaluationStatusV1::Pass,
            scope_digest: activated.scope.scope_digest.clone(),
            profile_id: query.profile().profile_id.clone(),
            evaluation_result_anchor: query.profile().evaluation_result_anchor.clone(),
        },
        profile: query.profile().clone(),
        diversity: query.diversity().clone(),
        ranking_revision,
        keyring: Some(keyring),
    })
}

fn map_unavailable_update_error(
    reason: QueryAuthorityUnavailableReasonV1,
) -> QueryAuthorityUpdateErrorV1 {
    match reason {
        QueryAuthorityUnavailableReasonV1::ScopeMismatch => {
            QueryAuthorityUpdateErrorV1::ScopeMismatch
        }
        QueryAuthorityUnavailableReasonV1::ActivationUnavailable
        | QueryAuthorityUnavailableReasonV1::ActivationNotCurrent
        | QueryAuthorityUnavailableReasonV1::ScopeRequired
        | QueryAuthorityUnavailableReasonV1::KeyUnavailable
        | QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile
        | QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile => {
            QueryAuthorityUpdateErrorV1::ActivationNotCurrent
        }
    }
}

impl QueryAuthorityProviderV1 for DaemonQueryAuthorityProviderV1 {
    fn accepted_authorities(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Vec<QueryAuthorityMaterialV1>, QueryAuthorityProviderErrorV1> {
        self.material_for(scope, privacy_domain)
            .map(|material| vec![material])
            .map_err(|reason| {
                QueryAuthorityProviderErrorV1::Unavailable(reason.as_str().to_owned())
            })
    }
}

fn current_transition(
    state: &RetrievalProfileStateV1,
) -> Option<&crate::config::retrieval::RetrievalProfileAuditEventV1> {
    let event = state.audit().last()?;
    if !matches!(
        &event.operation,
        RetrievalProfileAuditOperationV1::Activate
            | RetrievalProfileAuditOperationV1::Rollback { .. }
    ) || event.resulting_active_profile_id.as_str()
        != state.active().profile().profile_id.as_str()
        || event.resulting_active_digest.as_str() != state.active().profile_digest().as_str()
        || event.evaluation_anchor.as_str()
            != state.active().profile().evaluation_result_anchor.as_str()
    {
        return None;
    }
    Some(event)
}

fn has_current_query_authority(state: &RetrievalProfileStateV1) -> bool {
    current_transition(state).is_some()
        || (state.audit().is_empty()
            && state.rollback_profile().is_none()
            && exact_query_profile(state).is_ok())
}

fn exact_query_profile(
    state: &RetrievalProfileStateV1,
) -> Result<&AcceptedRetrievalProfileV1, QueryAuthorityUnavailableReasonV1> {
    exact_query_profile_from_slots(state.active(), state.rollback_profile())
}

fn exact_query_profile_from_slots<'a>(
    active: &'a AcceptedRetrievalProfileV1,
    rollback: Option<&'a AcceptedRetrievalProfileV1>,
) -> Result<&'a AcceptedRetrievalProfileV1, QueryAuthorityUnavailableReasonV1> {
    let mut matches = [Some(active), rollback]
        .into_iter()
        .flatten()
        .filter(|profile| is_exact_query_profile(profile));
    let profile = matches
        .next()
        .ok_or(QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile)?;
    if matches.next().is_some() {
        return Err(QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile);
    }
    Ok(profile)
}

fn is_exact_query_profile(active: &AcceptedRetrievalProfileV1) -> bool {
    let profile = active.profile();
    let expected = BTreeSet::from(RetrieverKind::QUERY_FALLBACK_LANES);
    profile
        .calibrations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        == expected
        && profile
            .weights_micros
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            == expected
        && profile.rerank_policy_id.is_none()
        && active.compatibility().semantic.is_none()
        && active.compatibility().rerank.is_none()
}

fn unavailable(reason: QueryAuthorityUnavailableReasonV1) -> QueryAuthorityProviderStatusV1 {
    QueryAuthorityProviderStatusV1::Unavailable { reason }
}

fn map_update_observer_error(
    error: QueryAuthorityUpdateErrorV1,
) -> RetrievalProfileActivationObserverErrorV1 {
    match error {
        QueryAuthorityUpdateErrorV1::InvalidScope
        | QueryAuthorityUpdateErrorV1::InvalidInitialState
        | QueryAuthorityUpdateErrorV1::ActivationNotCurrent => {
            RetrievalProfileActivationObserverErrorV1::Rejected
        }
        QueryAuthorityUpdateErrorV1::ScopeMismatch | QueryAuthorityUpdateErrorV1::CasConflict => {
            RetrievalProfileActivationObserverErrorV1::Conflict
        }
    }
}

#[cfg(test)]
#[path = "query_authority_provider_tests.rs"]
pub(crate) mod tests;
