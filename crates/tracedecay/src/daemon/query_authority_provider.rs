//! Daemon-owned query activation and durable cursor-key authority.
//!
//! This provider never chooses retrieval weights, calibration, diversity, or
//! evaluation identity. It exposes only the exact profile already accepted by
//! [`RetrievalProfileStateV1`] after a successful configuration activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};

use thiserror::Error;
use tokio::task;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    ComponentRevision, ManifestDigest, PrivacyDomainId, RetrievalAnchorId, RetrieverKind,
    configuration::UserProfileId,
};

use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileAuditOperationV1, RetrievalProfileStateV1,
};
use tracedecay_code_index_runtime::code_index_scheduler::query_runtime::{
    AcceptedQueryEvaluationV1, QueryAuthorityMaterialV1, QueryAuthorityProviderErrorV1,
    QueryAuthorityProviderV1,
};
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_usecases::semantic_runtime::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticRuntimeFuture, SemanticSourceCoherenceOutcomeV1,
    prepare_project_semantic_redundancy_authority, project_semantic_production_runtime,
    project_semantic_retained_code_generation, semantic_source_coherence,
};

/// Observation step that refuses a committed activation the serving
/// generation has moved past.
const SUPERSEDED_COMMITTED_ACTIVATION: &str = "superseded_committed_activation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryAuthorityUnavailableReasonV1 {
    ActivationUnavailable,
    ActivationNotCurrent,
    #[cfg(test)]
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
            #[cfg(test)]
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
    profile_id: UserProfileId,
    scope: ResolvedScope,
    state: RetrievalProfileStateV1,
    /// The exact evaluated query profile serving this scope's fallback lanes.
    ///
    /// Activation moves the profile it displaced into the rollback slot, so
    /// the evaluated fallback survives in the state only until the second
    /// activation displaces it out of both slots. It is not superseded by
    /// that: it is pinned here when the state still names it, and carried
    /// forward otherwise.
    query_profile: AcceptedRetrievalProfileV1,
    cursor_keys: Arc<tracedecay_session_temporal_store::GlobalDbCursorKeyProvider>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct QueryAuthorityKeyV1 {
    profile_id: UserProfileId,
    scope_digest: ManifestDigest,
}

pub(crate) struct PreparedQueryActivationV1 {
    profile_id: UserProfileId,
    scope: ResolvedScope,
    activated: RetrievalProfileStateV1,
    query_profile: AcceptedRetrievalProfileV1,
    cursor_keys: Arc<tracedecay_session_temporal_store::GlobalDbCursorKeyProvider>,
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

    pub(crate) fn query_authority(&self) -> &Arc<QueryAuthorityV1> {
        &self.query_authority
    }
}

/// Daemon owner for the current accepted query profile and the
/// durable project cursor-key authority loaded from its registered store.
#[derive(Clone)]
pub(crate) struct DaemonQueryAuthorityProviderV1 {
    activated: Arc<RwLock<BTreeMap<QueryAuthorityKeyV1, ActivatedQueryStateV1>>>,
}

pub(super) struct DaemonProfileQueryAuthorityProviderV1 {
    provider: DaemonQueryAuthorityProviderV1,
    profile_id: UserProfileId,
}

#[derive(Clone)]
pub(crate) struct DaemonQueryActivationRegistrarV1 {
    provider: DaemonQueryAuthorityProviderV1,
    registry: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: std::path::PathBuf,
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

impl DaemonQueryActivationRegistrarV1 {
    pub(crate) fn new(
        provider: DaemonQueryAuthorityProviderV1,
        registry: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        project_root: std::path::PathBuf,
        session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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
        let session_db = self.session_db.clone();
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
            // The committed pair is fenced on the mounted worktree. Project
            // open restores a committed activation before the demand-driven
            // code-index mount (a daemon restart is the common case), and the
            // reconciler may wake on the verified model before it as well.
            // With no worktree there is no serving state to be compatible
            // with yet: that is `Unavailable`, which the caller defers and the
            // reconciler retries once the index seats. It is not a stale or
            // conflicting committed state, and reporting it as one used to
            // fail the whole full-capability upgrade, leaving the index
            // unactivated and every lane unavailable until the next restart.
            if !registry.is_worktree_mounted(&project_root).await {
                tracing::info!(
                    event = "semantic_query_activation",
                    outcome = "deferred",
                    step = "code_index_worktree",
                    project_root = %project_root.display(),
                    semantic_enabled,
                    epoch = committed_epoch,
                    "committed activation awaits the code-index worktree mount"
                );
                return Err(RetrievalProfileActivationObserverErrorV1::Unavailable);
            }
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
            type ObserverError = RetrievalProfileActivationObserverErrorV1;
            let observed = async {
                let redundancy_ready = prepared_redundancy.has_active_authority();
                if semantic_enabled && !redundancy_ready {
                    return Err(("semantic_redundancy_authority", ObserverError::Rejected));
                }
                let serving = registry
                    .serving_code_scope(&project_root)
                    .await
                    .ok_or(("serving_code_scope", ObserverError::Unavailable))?;
                if serving.repository_id != scope.repository_id
                    || serving.worktree_id != scope.worktree_id
                {
                    return Err(("serving_scope_mismatch", ObserverError::Rejected));
                }
                // The "serving state" semantic must be compatible with is the
                // generation queries pin, not the graph-bearing serving slot.
                // A quiet remount of a partitioned (revision-7) generation
                // deliberately leaves that slot empty forever: exact/lexical
                // serve from the text owner and the graph seats from its
                // verified head without ever decoding the full generation.
                // Waiting on the slot here made the restored activation retry
                // `Unavailable` indefinitely after every restart, while every
                // query kept pinning the text-serving generation the whole
                // time. Resolve that generation from the durable publication
                // it names instead; only a mount with neither seat defers.
                let generation = match serving.serving_generation {
                    Some(generation) => generation,
                    None => {
                        resolve_text_serving_generation(&registry, &project_root)
                            .await
                            .ok_or(("serving_generation", ObserverError::Unavailable))?
                    }
                };
                let cursor_keys = Arc::new(
                    session_db
                        .load_session_cursor_key_provider_result()
                        .await
                        .map_err(|_| ("session_cursor_keys", ObserverError::Unavailable))?,
                );
                let prepared = provider
                    .prepare_after_successful_activation(
                        session_db.binding().shard_id.profile_id.clone(),
                        scope.clone(),
                        committed.state.clone(),
                        cursor_keys,
                        &generation.manifest().privacy_domain,
                    )
                    .map_err(|error| {
                        (
                            "prepare_after_successful_activation",
                            map_update_observer_error(error),
                        )
                    })?;
                let semantic_authority = if semantic_enabled {
                    let committed = committed.clone();
                    let query_profile_id =
                        prepared.query_authority().profile().profile_id.clone();
                    let authority = task::spawn_blocking(move || {
                        tracedecay_code_index_runtime::code_index_scheduler::semantic_query_runtime::SemanticQueryAuthorityV1::from_committed(
                            committed,
                            query_profile_id,
                        )
                    })
                    .await
                    .map_err(|_| ("semantic_query_authority_join", ObserverError::Unavailable))?
                    .map_err(|_| ("semantic_query_authority", ObserverError::Rejected))?;
                    Some(Arc::new(authority))
                } else {
                    None
                };
                // Cache observations are an exact CAS over the live semantic
                // pointer and query-runtime binding. All of the preparation
                // above may await or warm shared state, so observe only when
                // the coherent install is ready to consume this snapshot.
                let prepared_cache = if semantic_enabled {
                    let pins = committed
                        .current_activation
                        .as_ref()
                        .map(|activation| &activation.compatibility)
                        .ok_or(("current_activation", ObserverError::Rejected))?;
                    let runtime = project_semantic_production_runtime(&project_root)
                        .ok_or(("semantic_production_runtime", ObserverError::Unavailable))?;
                    let vectors = runtime
                        .active_vector_generation(pins)
                        .await
                        .ok_or(("active_vector_generation", ObserverError::Unavailable))?;
                    // Readiness is source compatibility, not generation
                    // identity. Reinstalling a committed activation whose
                    // vectors were projected from a superseded corpus un-seats
                    // the projection that does serve the live generation, and
                    // then reports Ready for vectors no query can use. Refuse
                    // instead: the activation stays committed and the runtime
                    // keeps the newer pointer until the operator activates it.
                    // `semantic_source_coherence` is the one authority on that
                    // question; this gate never re-derives its own.
                    if let SemanticSourceCoherenceOutcomeV1::Mismatch(mismatch) =
                        semantic_source_coherence(&vectors, &generation)
                    {
                        tracing::warn!(
                            event = "semantic_query_activation",
                            step = SUPERSEDED_COMMITTED_ACTIVATION,
                            project_root = %project_root.display(),
                            serving_generation = %mismatch.serving_generation,
                            serving_content_identity = %mismatch.serving_content_identity,
                            vector_source_generation = %mismatch.vector_source_generation,
                            vector_source_manifest_digest = %mismatch.vector_source_manifest_digest,
                            vector_generation = ?pins.vector_generation_id,
                            "the committed activation projected a superseded source; it is not \
                             reinstalled over the generation now being served"
                        );
                        return Err((SUPERSEDED_COMMITTED_ACTIVATION, ObserverError::Rejected));
                    }
                    let source_generation = vectors.source_generation().clone();
                    // Queries pin the serving publication, so the semantic
                    // cache binds to it whenever the activated vectors carry
                    // that publication's exact source content: either the
                    // serving generation is the evaluated source itself, or it
                    // republished the same sealed chunk corpus under a new
                    // identifier. Only a serving tree with different content
                    // falls back to the exact evaluated source generation,
                    // whose queries then truthfully refuse until reprojection;
                    // the typed mismatch below names both identities so that
                    // refusal is diagnosable without re-deriving either side.
                    let serving_bound = match
                        tracedecay_usecases::semantic_runtime::semantic_source_coherence(
                            &vectors,
                            generation.as_ref(),
                        )
                    {
                        tracedecay_usecases::semantic_runtime::SemanticSourceCoherenceOutcomeV1::Coherent(_) => true,
                        tracedecay_usecases::semantic_runtime::SemanticSourceCoherenceOutcomeV1::Mismatch(mismatch) => {
                            tracing::warn!(
                                event = "semantic_query_activation",
                                step = "source_identity_mismatch",
                                project_root = %project_root.display(),
                                vector_generation = ?pins.vector_generation_id,
                                vector_source_generation = %mismatch.vector_source_generation,
                                vector_source_manifest_digest = mismatch.vector_source_manifest_digest.as_str(),
                                serving_generation = %mismatch.serving_generation,
                                serving_content_identity = mismatch.serving_content_identity.as_str(),
                                "the activated vector generation was evaluated from a different source identity than the serving code generation; semantic stays bound to its exact evaluated source"
                            );
                            false
                        }
                    };
                    let bind_generation = if serving_bound {
                        generation.manifest().generation_id.clone()
                    } else {
                        source_generation.clone()
                    };
                    if !runtime.cache_ready_for(pins, &bind_generation) {
                        let code = if serving_bound {
                            Arc::clone(&generation)
                        } else {
                            match project_semantic_retained_code_generation(
                                &project_root,
                                &source_generation,
                            ) {
                                Some(code) => code,
                                None => match classify_published_generation_lookup(
                                    registry
                                        .published_generation(&project_root, &source_generation)
                                        .await,
                                ) {
                                    Ok(Some(code)) => code,
                                    Ok(None) => {
                                        tracing::warn!(
                                            event = "semantic_query_activation",
                                            step = "retained_code_generation",
                                            project_root = %project_root.display(),
                                            source_generation = %source_generation,
                                            vector_generation = ?pins.vector_generation_id,
                                            "the activated vector generation cites a source code generation that is neither retained in this process nor published in its store"
                                        );
                                        return Err((
                                            "retained_code_generation",
                                            ObserverError::Unavailable,
                                        ));
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            event = "semantic_query_activation",
                                            step = "published_code_generation_read",
                                            error = %error,
                                            project_root = %project_root.display(),
                                            source_generation = %source_generation,
                                            vector_generation = ?pins.vector_generation_id,
                                            "the activated vector generation's durable source code generation could not be read"
                                        );
                                        return Err((
                                            "published_code_generation_read",
                                            ObserverError::Unavailable,
                                        ));
                                    }
                                },
                            }
                        };
                        Some(
                            runtime
                                .prepare_restore_current(&code, &pins.vector_generation_id)
                                .await
                                .map_err(|error| {
                                    tracing::warn!(
                                        event = "semantic_query_activation",
                                        step = "prepare_restore_current",
                                        error = ?error,
                                        source_generation = %source_generation,
                                        vector_generation = ?pins.vector_generation_id,
                                        "the activated vector generation's cache could not be restored"
                                    );
                                    ("prepare_restore_current", ObserverError::Unavailable)
                                })?
                                .ok_or((
                                    "prepare_restore_current_stale",
                                    ObserverError::Unavailable,
                                ))?,
                        )
                    } else {
                        Some(
                            runtime
                                .prepare_current_cache_observation(pins, &bind_generation)
                                .ok_or((
                                    "prepare_current_cache_observation",
                                    ObserverError::Unavailable,
                                ))?,
                        )
                    }
                } else {
                    None
                };
                let prepared_view =
                    tracedecay_code_index_runtime::PreparedQueryActivationViewV1 {
                        scope: prepared.scope().clone(),
                        configuration_revision: prepared.configuration_revision().clone(),
                        query_authority: Arc::clone(prepared.query_authority()),
                    };
                registry
                    .install_committed_query_authorities(
                        &project_root,
                        &scope,
                        || {
                            provider
                                .commit_prepared_activation(&prepared)
                                .map_err(|error| error.to_string())
                        },
                        prepared_view,
                        semantic_authority,
                        prepared_cache,
                        rollback_semantic_generation.as_ref(),
                        prepared_redundancy,
                        &attempt,
                    )
                    .await
                    .map_err(|error| {
                        tracing::warn!(
                            event = "semantic_query_activation",
                            step = "install_committed_query_authorities",
                            error = %error,
                            "query authority installation refused the committed activation"
                        );
                        ("install_committed_query_authorities", ObserverError::Conflict)
                    })?;
                Ok(())
            }
            .await;
            // A superseded activation did not fail: it is simply not
            // applicable to the generation now being served. Tearing down the
            // query activation the previous successful observation installed
            // would strand the still-committed profile, so only real failures
            // reach the clearing path below.
            let superseded = matches!(
                &observed,
                Err((step, _)) if *step == SUPERSEDED_COMMITTED_ACTIVATION
            );
            let observed = observed.map_err(|(step, error)| {
                tracing::warn!(
                    event = "semantic_query_activation",
                    outcome = "failed",
                    step,
                    error = ?error,
                    semantic_enabled,
                    epoch = committed_epoch,
                    "committed activation could not be installed as the query authority"
                );
                error
            });
            if observed.is_err() && !superseded {
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
    fn profile_key(profile_id: &UserProfileId, scope: &ResolvedScope) -> QueryAuthorityKeyV1 {
        QueryAuthorityKeyV1 {
            profile_id: profile_id.clone(),
            scope_digest: scope.scope_digest.clone(),
        }
    }

    pub(super) fn for_profile(
        &self,
        profile_id: UserProfileId,
    ) -> DaemonProfileQueryAuthorityProviderV1 {
        DaemonProfileQueryAuthorityProviderV1 {
            provider: self.clone(),
            profile_id,
        }
    }

    pub(crate) fn retire_project(
        &self,
        profile_id: &UserProfileId,
        project_id: &tracedecay_domain::ProjectId,
    ) {
        let mut activated = match self.activated.write() {
            Ok(activated) => activated,
            Err(poisoned) => poisoned.into_inner(),
        };
        activated.retain(|key, activated| {
            &key.profile_id != profile_id || &activated.scope.project_id != project_id
        });
    }

    #[hotpath::measure(label = "daemon.query.prepare_activation")]
    pub(crate) fn prepare_after_successful_activation(
        &self,
        profile_id: UserProfileId,
        scope: ResolvedScope,
        activated: RetrievalProfileStateV1,
        cursor_keys: Arc<tracedecay_session_temporal_store::GlobalDbCursorKeyProvider>,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<PreparedQueryActivationV1, QueryAuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidScope)?;
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(&profile_id, &scope);
        let installed = current.get(&key);
        if !installed.is_some_and(|installed| {
            installed.profile_id == profile_id
                && installed.scope == scope
                && installed.state == activated
        }) {
            validate_successful_activation_update(&current, &key, &scope, &activated)?;
        }
        let query_profile =
            resolved_query_profile(installed, &activated).map_err(map_unavailable_update_error)?;
        let candidate = ActivatedQueryStateV1 {
            profile_id: profile_id.clone(),
            scope: scope.clone(),
            state: activated.clone(),
            query_profile: query_profile.clone(),
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
            profile_id,
            scope,
            activated,
            query_profile,
            cursor_keys,
            query_authority,
        })
    }

    #[hotpath::measure(label = "daemon.query.commit_activation")]
    pub(crate) fn commit_prepared_activation(
        &self,
        prepared: &PreparedQueryActivationV1,
    ) -> Result<(), QueryAuthorityUpdateErrorV1> {
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(&prepared.profile_id, &prepared.scope);
        if current.get(&key).is_some_and(|installed| {
            installed.profile_id == prepared.profile_id
                && installed.scope == prepared.scope
                && installed.state == prepared.activated
        }) {
            return Ok(());
        }
        validate_successful_activation_update(
            &current,
            &key,
            &prepared.scope,
            &prepared.activated,
        )?;
        current.insert(
            key,
            ActivatedQueryStateV1 {
                profile_id: prepared.profile_id.clone(),
                scope: prepared.scope.clone(),
                state: prepared.activated.clone(),
                query_profile: prepared.query_profile.clone(),
                cursor_keys: Arc::clone(&prepared.cursor_keys),
            },
        );
        Ok(())
    }

    /// Restore the evaluated fallback installed as the configuration store's
    /// initial state. Initial installation has no mutation audit event, so it
    /// is admitted only while the exact query profile is active with no rollback
    /// slot or audit history.
    #[hotpath::measure(label = "daemon.query.install_initial")]
    pub(crate) fn install_evaluated_initial_state(
        &self,
        profile_id: UserProfileId,
        scope: ResolvedScope,
        initial: RetrievalProfileStateV1,
        cursor_keys: Arc<tracedecay_session_temporal_store::GlobalDbCursorKeyProvider>,
    ) -> Result<QueryAuthorityProviderStatusV1, QueryAuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidScope)?;
        if !initial.audit().is_empty() || initial.rollback_profile().is_some() {
            return Err(QueryAuthorityUpdateErrorV1::InvalidInitialState);
        }
        let query_profile = exact_query_profile(&initial)
            .map_err(|_| QueryAuthorityUpdateErrorV1::InvalidInitialState)?
            .clone();
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(&profile_id, &scope);
        if let Some(prior) = current.get(&key) {
            if prior.scope != scope {
                return Err(QueryAuthorityUpdateErrorV1::ScopeMismatch);
            }
            if prior.state != initial {
                return Err(QueryAuthorityUpdateErrorV1::CasConflict);
            }
        }
        current.insert(
            key,
            ActivatedQueryStateV1 {
                profile_id: profile_id.clone(),
                scope: scope.clone(),
                state: initial,
                query_profile,
                cursor_keys,
            },
        );
        drop(current);
        Ok(self.status_for(&profile_id, &scope))
    }

    fn status_for(
        &self,
        profile_id: &UserProfileId,
        scope: &ResolvedScope,
    ) -> QueryAuthorityProviderStatusV1 {
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(profile_id, scope);
        let Some(activated) = current.get(&key) else {
            return unavailable(QueryAuthorityUnavailableReasonV1::ActivationUnavailable);
        };
        status_for_activated(scope, activated)
    }

    #[cfg(test)]
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
        let mut matches = current
            .values()
            .filter(|activated| &activated.scope == scope);
        let Some(activated) = matches.next() else {
            return unavailable(QueryAuthorityUnavailableReasonV1::ActivationUnavailable);
        };
        if matches.next().is_some() {
            return unavailable(QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile);
        }
        status_for_activated(scope, activated)
    }

    fn material_for_profile(
        &self,
        profile_id: &UserProfileId,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<QueryAuthorityMaterialV1, QueryAuthorityUnavailableReasonV1> {
        match self.status_for(profile_id, scope) {
            QueryAuthorityProviderStatusV1::Available { .. } => {}
            QueryAuthorityProviderStatusV1::Unavailable { reason } => return Err(reason),
        }
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(profile_id, scope);
        let activated = current
            .get(&key)
            .ok_or(QueryAuthorityUnavailableReasonV1::ActivationUnavailable)?;
        if &activated.scope != scope {
            return Err(QueryAuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if !has_current_query_authority(&activated.state) {
            return Err(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        query_material_for_activated(activated, privacy_domain)
    }

    /// Resolve the active evaluated all-lane profile for owning-source
    /// `TaskSession` selection. The fallback/rollback profile never satisfies
    /// this boundary, and key material remains inside the returned authority.
    fn federated_authority_for_profile(
        &self,
        profile_id: &UserProfileId,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Arc<QueryAuthorityV1>, QueryAuthorityUnavailableReasonV1> {
        scope
            .validate()
            .map_err(|_| QueryAuthorityUnavailableReasonV1::ScopeMismatch)?;
        privacy_domain
            .validate()
            .map_err(|_| QueryAuthorityUnavailableReasonV1::KeyUnavailable)?;
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::profile_key(profile_id, scope);
        let activated = current
            .get(&key)
            .ok_or(QueryAuthorityUnavailableReasonV1::ActivationUnavailable)?;
        if &activated.scope != scope {
            return Err(QueryAuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if current_transition(&activated.state).is_none() {
            return Err(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        let accepted = activated.state.active();
        if !is_federated_profile(accepted) {
            return Err(QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile);
        }
        let keyring = activated
            .cursor_keys
            .retrieval_keyring(privacy_domain.clone())
            .map_err(|_| QueryAuthorityUnavailableReasonV1::KeyUnavailable)?;
        let ranking_revision =
            ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
                .map_err(|_| QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile)?;
        QueryAuthorityV1::new_federated(
            accepted.profile().clone(),
            accepted.diversity().clone(),
            ranking_revision,
            keyring,
        )
        .map(Arc::new)
        .map_err(|_| QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile)
    }

    #[hotpath::measure(label = "daemon.query.federated_authority")]
    pub(crate) fn federated_authority_for(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Arc<QueryAuthorityV1>, QueryAuthorityUnavailableReasonV1> {
        let profile_id = {
            let current = self
                .activated
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut matches = current
                .keys()
                .filter(|key| key.scope_digest == scope.scope_digest);
            let Some(profile_id) = matches.next().map(|key| key.profile_id.clone()) else {
                return Err(QueryAuthorityUnavailableReasonV1::ActivationUnavailable);
            };
            if matches.next().is_some() {
                return Err(QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile);
            }
            profile_id
        };
        self.federated_authority_for_profile(&profile_id, scope, privacy_domain)
    }
}

fn validate_successful_activation_update(
    current: &BTreeMap<QueryAuthorityKeyV1, ActivatedQueryStateV1>,
    key: &QueryAuthorityKeyV1,
    scope: &ResolvedScope,
    activated: &RetrievalProfileStateV1,
) -> Result<(), QueryAuthorityUpdateErrorV1> {
    let event =
        current_transition(activated).ok_or(QueryAuthorityUpdateErrorV1::ActivationNotCurrent)?;
    if activated.configuration_revision() != &event.result_revision {
        return Err(QueryAuthorityUpdateErrorV1::ActivationNotCurrent);
    }
    if let Some(prior) = current.get(key) {
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

/// Resolve the exact evaluated query profile a committed activation serves.
///
/// The state names it while it still occupies the active or rollback slot.
/// Once a second activation displaces it out of both, the profile already
/// pinned for this scope is still the one the fallback lanes execute, so it is
/// carried forward rather than treated as a broken activation.
fn resolved_query_profile(
    installed: Option<&ActivatedQueryStateV1>,
    state: &RetrievalProfileStateV1,
) -> Result<AcceptedRetrievalProfileV1, QueryAuthorityUnavailableReasonV1> {
    match exact_query_profile(state) {
        Ok(profile) => Ok(profile.clone()),
        Err(QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile) => installed
            .map(|installed| installed.query_profile.clone())
            .ok_or(QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile),
        Err(reason) => Err(reason),
    }
}

fn query_material_for_activated(
    activated: &ActivatedQueryStateV1,
    privacy_domain: &PrivacyDomainId,
) -> Result<QueryAuthorityMaterialV1, QueryAuthorityUnavailableReasonV1> {
    let query = &activated.query_profile;
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
            status: tracedecay_query::search_quality::DirectEvaluationStatusV1::Pass,
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

fn status_for_activated(
    scope: &ResolvedScope,
    activated: &ActivatedQueryStateV1,
) -> QueryAuthorityProviderStatusV1 {
    if scope != &activated.scope {
        return unavailable(QueryAuthorityUnavailableReasonV1::ScopeMismatch);
    }
    if !has_current_query_authority(&activated.state) {
        return unavailable(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
    }
    let profile = &activated.query_profile;
    QueryAuthorityProviderStatusV1::Available {
        scope_digest: activated.scope.scope_digest.clone(),
        profile_id: profile.profile().profile_id.clone(),
        evaluation_anchor: profile.profile().evaluation_result_anchor.clone(),
    }
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
        | QueryAuthorityUnavailableReasonV1::KeyUnavailable
        | QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile
        | QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile => {
            QueryAuthorityUpdateErrorV1::ActivationNotCurrent
        }
        #[cfg(test)]
        QueryAuthorityUnavailableReasonV1::ScopeRequired => {
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
        scope.validate().map_err(|_| {
            QueryAuthorityProviderErrorV1::Unavailable(
                QueryAuthorityUnavailableReasonV1::ScopeMismatch
                    .as_str()
                    .to_owned(),
            )
        })?;
        privacy_domain.validate().map_err(|_| {
            QueryAuthorityProviderErrorV1::Unavailable(
                QueryAuthorityUnavailableReasonV1::KeyUnavailable
                    .as_str()
                    .to_owned(),
            )
        })?;
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let materials = current
            .values()
            .filter(|activated| &activated.scope == scope)
            .map(|activated| {
                if !has_current_query_authority(&activated.state) {
                    return Err(QueryAuthorityUnavailableReasonV1::ActivationNotCurrent);
                }
                query_material_for_activated(activated, privacy_domain)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|reason| {
                QueryAuthorityProviderErrorV1::Unavailable(reason.as_str().to_owned())
            })?;
        if materials.is_empty() {
            return Err(QueryAuthorityProviderErrorV1::Unavailable(
                QueryAuthorityUnavailableReasonV1::ActivationUnavailable
                    .as_str()
                    .to_owned(),
            ));
        }
        Ok(materials)
    }
}

impl QueryAuthorityProviderV1 for DaemonProfileQueryAuthorityProviderV1 {
    fn accepted_authorities(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Vec<QueryAuthorityMaterialV1>, QueryAuthorityProviderErrorV1> {
        self.provider
            .material_for_profile(&self.profile_id, scope, privacy_domain)
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

fn is_federated_profile(active: &AcceptedRetrievalProfileV1) -> bool {
    let expected = BTreeSet::from(RetrieverKind::ALL_LANES);
    active
        .profile()
        .calibrations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        == expected
        && active
            .profile()
            .weights_micros
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            == expected
        && active.profile().rerank_policy_id.is_none()
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

/// The sealed generation the text-serving slot names, read from the durable
/// publication store. This is the generation `generation_for` hands a query
/// whose serving slot is empty, so binding semantic to it keeps activation and
/// query admission on the same identity.
async fn resolve_text_serving_generation(
    registry: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &std::path::Path,
) -> Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>> {
    let text = registry.latest_text_serving_for_root(project_root).await?;
    let generation_id = text.metadata().manifest().generation_id.clone();
    match classify_published_generation_lookup(
        registry
            .published_generation(project_root, &generation_id)
            .await,
    ) {
        Ok(Some(generation)) => Some(generation),
        Ok(None) => {
            tracing::warn!(
                event = "semantic_query_activation",
                step = "text_serving_generation",
                project_root = %project_root.display(),
                generation = %generation_id,
                "the text-serving generation is not published in the durable store"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                event = "semantic_query_activation",
                step = "text_serving_generation_read",
                error = %error,
                project_root = %project_root.display(),
                generation = %generation_id,
                "the text-serving generation could not be read from the durable store"
            );
            None
        }
    }
}

fn classify_published_generation_lookup(
    lookup: Option<
        Result<
            Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>>,
            tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerErrorV1,
        >,
    >,
) -> Result<
    Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>>,
    tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerErrorV1,
> {
    lookup.unwrap_or(Ok(None))
}

#[cfg(test)]
#[path = "query_authority_provider_tests.rs"]
pub(crate) mod tests;
