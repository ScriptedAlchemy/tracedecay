//! Central authorization and immutable identity for multi-root scope sets.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, ManifestDigest, RootGenerationV1, RootScopeOutcomeV1, ScopeOutcome,
    ScopePartialReasonV1, ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1, UtcMicros,
    canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::{OpaqueCursor, RequestAdmission, RequestContext};

pub mod catalog;
mod collection;
mod locator;

pub use catalog::{
    MultiRootApplicationOperation, multi_root_capability_manifest,
    multi_root_executable_binding_registry, multi_root_operation_authority,
};
pub use collection::{
    MultiRootCollectionResolutionV1, MultiRootCollectionSelectorV1,
    MultiRootCollectionUnavailableV1,
};
pub use locator::{
    AuthorizedRoot, AuthorizedRootAdmission, RegisteredRootLocatorV1, RegisteredRootSelectorV1,
    SharedProfileStoreLocatorV1,
};

const AUTHORIZED_SCOPE_SET_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.authorized-scope-set.v1";
const MULTI_ROOT_CONTINUATION_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.multi-root-continuation.v1";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootScopeSetReadRequestV1 {
    pub scope_set_id: ScopeSetId,
}

impl MultiRootScopeSetReadRequestV1 {
    pub fn new(scope_set_id: ScopeSetId) -> Result<Self, MultiRootQueryError> {
        scope_set_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        Ok(Self { scope_set_id })
    }
}

/// Canonical external selector for creating or updating an authorized scope
/// set. Every member names one exact registered root; project-only selection
/// cannot silently widen to an active or first-mounted graph.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootScopeSetCasRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub expected_revision: Option<ScopeSetRevision>,
    pub roots: Vec<RegisteredRootSelectorV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MultiRootScopeSetCasStatusV1 {
    Applied,
    Conflict,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootScopeSetCasResultV1 {
    pub status: MultiRootScopeSetCasStatusV1,
    pub scope_set: Option<AuthorizedScopeSet>,
}

impl MultiRootScopeSetCasRequestV1 {
    pub fn new(
        scope_set_id: ScopeSetId,
        expected_revision: Option<ScopeSetRevision>,
        mut roots: Vec<RegisteredRootSelectorV1>,
    ) -> Result<Self, MultiRootQueryError> {
        scope_set_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        if let Some(revision) = expected_revision {
            revision
                .validate()
                .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        }
        for root in &roots {
            root.validate()?;
        }
        roots.sort_by(|left, right| {
            (&left.project_id, &left.root).cmp(&(&right.project_id, &right.root))
        });
        if roots.is_empty() || roots.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        Ok(Self {
            scope_set_id,
            expected_revision,
            roots,
        })
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        if Self::new(
            self.scope_set_id.clone(),
            self.expected_revision,
            self.roots.clone(),
        )? != *self
        {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        Ok(())
    }
}

/// Closed federated read families. The family is typed while its existing
/// operation-specific request remains the canonical JSON payload owned by that
/// application surface.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MultiRootOperationV1 {
    Work { request: serde_json::Value },
    Git { request: serde_json::Value },
    Feedback { request: serde_json::Value },
    Impact { request: serde_json::Value },
    Query { request: serde_json::Value },
}

/// External federated request bound to one persisted scope-set revision and
/// digest. Query/order digests and root generations are server-derived.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootExecuteRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub operation: MultiRootOperationV1,
    pub page: u64,
    pub continuation: Option<MultiRootContinuationV1>,
}

impl MultiRootExecuteRequestV1 {
    pub fn new(
        scope_set_id: ScopeSetId,
        scope_set_revision: ScopeSetRevision,
        scope_set_digest: ManifestDigest,
        operation: MultiRootOperationV1,
        page: u64,
        continuation: Option<MultiRootContinuationV1>,
    ) -> Result<Self, MultiRootQueryError> {
        scope_set_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        scope_set_revision
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        scope_set_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        if let Some(cursor) = &continuation {
            cursor.validate()?;
            if page != cursor.next_page() {
                return Err(MultiRootQueryError::CursorMismatch { field: "page" });
            }
        } else if page != 0 {
            return Err(MultiRootQueryError::CursorMismatch { field: "page" });
        }
        Ok(Self {
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
            operation,
            page,
            continuation,
        })
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        Self::new(
            self.scope_set_id.clone(),
            self.scope_set_revision,
            self.scope_set_digest.clone(),
            self.operation.clone(),
            self.page,
            self.continuation.clone(),
        )
        .map(|_| ())
    }
}

/// Failures from centralized multi-root authorization and validation.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AuthorizedScopeSetError {
    #[error("an authorized scope set must contain at least one root")]
    Empty,
    #[error("all roots in an authorized scope set must belong to one actor")]
    MixedActor,
    #[error("a requested root was not admitted for the required capability and use case")]
    Denied,
    #[error("an authorized scope set contains a duplicate exact root")]
    DuplicateRoot,
    #[error("authorized scope-set contract is invalid: {0}")]
    Invalid(String),
}

/// Immutable canonical set of exact roots admitted by their existing request
/// contexts. A registered locator participates only as frozen reopening
/// evidence; its paired [`ResolvedScope`] remains the root identity authority.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedScopeSet {
    scope_set_id: ScopeSetId,
    revision: ScopeSetRevision,
    actor_id: ActorId,
    roots: Vec<AuthorizedRoot>,
    digest: ManifestDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizedScopeSetWire {
    scope_set_id: ScopeSetId,
    revision: ScopeSetRevision,
    actor_id: ActorId,
    roots: Vec<AuthorizedRoot>,
    digest: ManifestDigest,
}

impl<'de> Deserialize<'de> for AuthorizedScopeSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AuthorizedScopeSetWire::deserialize(deserializer)?;
        let set = Self::from_authorized_roots(
            wire.scope_set_id,
            wire.revision,
            wire.actor_id,
            wire.roots,
        )
        .map_err(serde::de::Error::custom)?;
        if set.digest != wire.digest {
            return Err(serde::de::Error::custom(
                "authorized scope-set digest does not match its exact roots",
            ));
        }
        Ok(set)
    }
}

impl AuthorizedScopeSet {
    fn from_authorized_roots(
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        actor_id: ActorId,
        mut roots: Vec<AuthorizedRoot>,
    ) -> Result<Self, AuthorizedScopeSetError> {
        scope_set_id
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        revision
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        actor_id
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        if roots.is_empty() {
            return Err(AuthorizedScopeSetError::Empty);
        }
        for root in &roots {
            match &root.locator {
                Some(locator) => {
                    AuthorizedRoot::registered(root.scope.clone(), locator.clone())?;
                }
                None => {
                    AuthorizedRoot::resolved(root.scope.clone())?;
                }
            }
        }
        roots.sort_by(|left, right| {
            (
                left.scope.project_id.as_str(),
                left.scope.repository_id.as_str(),
                left.scope.worktree_id.as_str(),
                left.scope
                    .reference
                    .as_ref()
                    .map(|reference| reference.as_str()),
                left.locator.as_ref().map(|locator| &locator.canonical_root),
            )
                .cmp(&(
                    right.scope.project_id.as_str(),
                    right.scope.repository_id.as_str(),
                    right.scope.worktree_id.as_str(),
                    right
                        .scope
                        .reference
                        .as_ref()
                        .map(|reference| reference.as_str()),
                    right
                        .locator
                        .as_ref()
                        .map(|locator| &locator.canonical_root),
                ))
        });
        if roots
            .windows(2)
            .any(|pair| pair[0].scope.scope_digest == pair[1].scope.scope_digest)
        {
            return Err(AuthorizedScopeSetError::DuplicateRoot);
        }
        let profile = roots[0].locator.as_ref().map(|locator| &locator.profile);
        if roots
            .iter()
            .any(|root| root.locator.as_ref().map(|locator| &locator.profile) != profile)
        {
            return Err(AuthorizedScopeSetError::Invalid(
                "authorized roots must either all be registered under one profile store locator or all be pre-resolved"
                    .to_owned(),
            ));
        }
        let digest = canonical_sha256(&(
            AUTHORIZED_SCOPE_SET_DIGEST_DOMAIN_V1,
            &scope_set_id,
            revision,
            &actor_id,
            &roots,
        ))
        .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        Ok(Self {
            scope_set_id,
            revision,
            actor_id,
            roots,
            digest,
        })
    }

    pub fn scope_set_id(&self) -> &ScopeSetId {
        &self.scope_set_id
    }

    #[hotpath::skip]
    pub const fn revision(&self) -> ScopeSetRevision {
        self.revision
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn roots(&self) -> &[AuthorizedRoot] {
        &self.roots
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, AuthorizedScopeSetError> {
        canonical_sha256(&(
            AUTHORIZED_SCOPE_SET_DIGEST_DOMAIN_V1,
            &self.scope_set_id,
            self.revision,
            &self.actor_id,
            &self.roots,
        ))
        .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), AuthorizedScopeSetError> {
        let canonical = Self::from_authorized_roots(
            self.scope_set_id.clone(),
            self.revision,
            self.actor_id.clone(),
            self.roots.clone(),
        )?;
        if canonical.roots != self.roots || canonical.digest != self.digest {
            return Err(AuthorizedScopeSetError::Invalid(
                "scope-set canonical roots or digest changed".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Sole constructor for an [`AuthorizedScopeSet`]. It narrows existing
/// single-root [`RequestContext`] grants and never resolves paths itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct AuthorizedScopeSetAuthority;

impl AuthorizedScopeSetAuthority {
    #[allow(clippy::too_many_arguments)]
    pub fn authorize(
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        contexts: Vec<RequestContext>,
        capability_id: &CapabilityId,
        use_case_id: &UseCaseId,
        observed_at: UtcMicros,
    ) -> Result<AuthorizedScopeSet, AuthorizedScopeSetError> {
        let actor = contexts
            .first()
            .map(RequestContext::actor)
            .cloned()
            .ok_or(AuthorizedScopeSetError::Empty)?;
        if contexts.iter().any(|context| context.actor() != &actor) {
            return Err(AuthorizedScopeSetError::MixedActor);
        }
        if contexts.iter().any(|context| {
            context.admission_at(observed_at) != RequestAdmission::Admitted
                || !context.allows(capability_id, use_case_id)
        }) {
            return Err(AuthorizedScopeSetError::Denied);
        }
        Self::authorize_resolved(
            scope_set_id,
            revision,
            actor,
            contexts
                .into_iter()
                .map(|context| AuthorizedRoot::resolved(context.scope().clone()))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn authorize_registered(
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        admissions: Vec<AuthorizedRootAdmission>,
        capability_id: &CapabilityId,
        use_case_id: &UseCaseId,
        observed_at: UtcMicros,
    ) -> Result<AuthorizedScopeSet, AuthorizedScopeSetError> {
        let actor = admissions
            .first()
            .map(|admission| admission.context.actor())
            .cloned()
            .ok_or(AuthorizedScopeSetError::Empty)?;
        if admissions
            .iter()
            .any(|admission| admission.context.actor() != &actor)
        {
            return Err(AuthorizedScopeSetError::MixedActor);
        }
        if admissions.iter().any(|admission| {
            admission.context.admission_at(observed_at) != RequestAdmission::Admitted
                || !admission.context.allows(capability_id, use_case_id)
        }) {
            return Err(AuthorizedScopeSetError::Denied);
        }
        Self::authorize_resolved(
            scope_set_id,
            revision,
            actor,
            admissions
                .into_iter()
                .map(|admission| {
                    AuthorizedRoot::registered(admission.context.scope().clone(), admission.locator)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    fn authorize_resolved(
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        actor: ActorId,
        roots: Vec<AuthorizedRoot>,
    ) -> Result<AuthorizedScopeSet, AuthorizedScopeSetError> {
        actor
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        AuthorizedScopeSet::from_authorized_roots(scope_set_id, revision, actor, roots)
    }
}

/// Failures that reject a federated query before it can widen or drift scope.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MultiRootQueryError {
    #[error("multi-root request does not contain exactly the authorized roots")]
    RootSetMismatch,
    #[error("multi-root request authorization was denied")]
    Denied,
    #[error("multi-root continuation binding changed: {field}")]
    CursorMismatch { field: &'static str },
    #[error("multi-root query contract is invalid: {0}")]
    Invalid(String),
}

/// Frozen continuation identity shared by all participating roots.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootContinuationV1 {
    scope_set_digest: ManifestDigest,
    root_generations: Vec<RootScopeOutcomeV1<Option<RootGenerationV1>>>,
    #[schemars(with = "Vec<RootScopeOutcomeV1<Option<String>>>")]
    root_cursors: Vec<RootScopeOutcomeV1<Option<OpaqueCursor>>>,
    query_digest: ManifestDigest,
    order_digest: ManifestDigest,
    #[schemars(range(min = 1))]
    next_page: u64,
    digest: ManifestDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultiRootContinuationWireV1 {
    scope_set_digest: ManifestDigest,
    root_generations: Vec<RootScopeOutcomeV1<Option<RootGenerationV1>>>,
    root_cursors: Vec<RootScopeOutcomeV1<Option<OpaqueCursor>>>,
    query_digest: ManifestDigest,
    order_digest: ManifestDigest,
    next_page: u64,
    digest: ManifestDigest,
}

impl<'de> Deserialize<'de> for MultiRootContinuationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MultiRootContinuationWireV1::deserialize(deserializer)?;
        let continuation = Self::new(
            wire.scope_set_digest,
            wire.root_generations,
            wire.root_cursors,
            wire.query_digest,
            wire.order_digest,
            wire.next_page,
        )
        .map_err(serde::de::Error::custom)?;
        if continuation.digest != wire.digest {
            return Err(serde::de::Error::custom(
                "multi-root continuation digest does not match its frozen identity",
            ));
        }
        Ok(continuation)
    }
}

impl MultiRootContinuationV1 {
    pub fn new(
        scope_set_digest: ManifestDigest,
        mut root_generations: Vec<RootScopeOutcomeV1<Option<RootGenerationV1>>>,
        mut root_cursors: Vec<RootScopeOutcomeV1<Option<OpaqueCursor>>>,
        query_digest: ManifestDigest,
        order_digest: ManifestDigest,
        next_page: u64,
    ) -> Result<Self, MultiRootQueryError> {
        scope_set_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        query_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        order_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        if root_generations.is_empty() {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        if next_page == 0 {
            return Err(MultiRootQueryError::Invalid(
                "continuation page must be nonzero".to_owned(),
            ));
        }
        for root in &root_generations {
            root.scope_digest
                .validate()
                .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
            match &root.outcome {
                ScopeOutcome::Exact(Some(generation))
                | ScopeOutcome::Partial {
                    value: Some(generation),
                    ..
                } => RootScopeOutcomeV1::new(
                    root.scope_digest.clone(),
                    ScopeOutcome::Exact(generation.clone()),
                )
                .and_then(|root| root.validate_generation())
                .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?,
                ScopeOutcome::Exact(None)
                | ScopeOutcome::Partial { value: None, .. }
                | ScopeOutcome::Denied
                | ScopeOutcome::Unavailable { .. } => {}
            }
        }
        root_generations.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
        root_cursors.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
        if root_generations
            .windows(2)
            .any(|pair| pair[0].scope_digest == pair[1].scope_digest)
        {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        if root_cursors.len() != root_generations.len()
            || root_cursors
                .iter()
                .zip(&root_generations)
                .any(|(cursor, generation)| cursor.scope_digest != generation.scope_digest)
        {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        let digest = canonical_sha256(&(
            MULTI_ROOT_CONTINUATION_DIGEST_DOMAIN_V1,
            &scope_set_digest,
            &root_generations,
            &root_cursors,
            &query_digest,
            &order_digest,
            next_page,
        ))
        .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        Ok(Self {
            scope_set_digest,
            root_generations,
            root_cursors,
            query_digest,
            order_digest,
            next_page,
            digest,
        })
    }

    pub fn scope_set_digest(&self) -> &ManifestDigest {
        &self.scope_set_digest
    }

    pub fn query_digest(&self) -> &ManifestDigest {
        &self.query_digest
    }

    pub fn order_digest(&self) -> &ManifestDigest {
        &self.order_digest
    }

    pub fn root_generations(&self) -> &[RootScopeOutcomeV1<Option<RootGenerationV1>>] {
        &self.root_generations
    }

    pub fn root_generation(
        &self,
        scope_digest: &ManifestDigest,
    ) -> Option<&ScopeOutcome<Option<RootGenerationV1>>> {
        self.root_generations
            .iter()
            .find(|root| &root.scope_digest == scope_digest)
            .map(|root| &root.outcome)
    }

    pub fn root_cursors(&self) -> &[RootScopeOutcomeV1<Option<OpaqueCursor>>] {
        &self.root_cursors
    }

    pub fn root_cursor(
        &self,
        scope_digest: &ManifestDigest,
    ) -> Option<&ScopeOutcome<Option<OpaqueCursor>>> {
        self.root_cursors
            .iter()
            .find(|root| &root.scope_digest == scope_digest)
            .map(|root| &root.outcome)
    }

    #[hotpath::skip]
    pub const fn next_page(&self) -> u64 {
        self.next_page
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        let canonical = Self::new(
            self.scope_set_digest.clone(),
            self.root_generations.clone(),
            self.root_cursors.clone(),
            self.query_digest.clone(),
            self.order_digest.clone(),
            self.next_page,
        )?;
        if canonical != *self {
            return Err(MultiRootQueryError::CursorMismatch {
                field: "continuation digest",
            });
        }
        Ok(())
    }
}

/// Internal application request after transport admission.
pub struct MultiRootQueryRequestV1<Q> {
    pub scope_set: AuthorizedScopeSet,
    pub contexts: Vec<RequestContext>,
    pub root_generations: Vec<RootScopeOutcomeV1<Option<RootGenerationV1>>>,
    pub capability_id: CapabilityId,
    pub use_case_id: UseCaseId,
    pub observed_at: UtcMicros,
    pub query: Q,
    pub query_digest: ManifestDigest,
    pub order_digest: ManifestDigest,
    pub page: u64,
    pub continuation: Option<MultiRootContinuationV1>,
}

/// One root-local query adapter. It receives only the exact admitted context
/// and frozen generation for that root.
pub trait MultiRootQueryPort<Q, T> {
    fn query_root(
        &self,
        context: &RequestContext,
        generation: Option<&RootGenerationV1>,
        query: &Q,
        cursor: Option<&OpaqueCursor>,
    ) -> ScopeOutcome<MultiRootRootPageV1<T>>;
}

/// One root's actual child page and its owner-minted continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiRootRootPageV1<T> {
    pub value: Vec<T>,
    pub next_cursor: Option<OpaqueCursor>,
}

/// Federated page preserving each root outcome and aggregate partial truth.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "MultiRootQueryPageV1_for_{T}")]
pub struct MultiRootQueryPageV1<T> {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub roots: Vec<RootScopeOutcomeV1<Vec<T>>>,
    pub aggregate: ScopeOutcome<Vec<T>>,
    pub continuation: Option<MultiRootContinuationV1>,
}

pub struct AuthorizedMultiRootQueryService<P> {
    port: P,
}

impl<P> AuthorizedMultiRootQueryService<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }

    pub fn execute<Q, T>(
        &self,
        request: MultiRootQueryRequestV1<Q>,
    ) -> Result<MultiRootQueryPageV1<T>, MultiRootQueryError>
    where
        P: MultiRootQueryPort<Q, T>,
        T: Clone,
    {
        request
            .scope_set
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        let contexts = validate_contexts(&request)?;
        let generations = validate_generations(&request)?;
        validate_continuation(&request)?;

        let mut roots = Vec::with_capacity(request.scope_set.roots().len());
        let mut root_cursors = Vec::with_capacity(request.scope_set.roots().len());
        for root in request.scope_set.roots() {
            let scope = root.scope();
            let resume = request
                .continuation
                .as_ref()
                .map(|continuation| {
                    continuation
                        .root_cursor(&scope.scope_digest)
                        .ok_or(MultiRootQueryError::RootSetMismatch)
                })
                .transpose()?;
            let child_cursor = match resume {
                Some(ScopeOutcome::Exact(cursor))
                | Some(ScopeOutcome::Partial { value: cursor, .. }) => cursor.as_ref(),
                Some(ScopeOutcome::Denied | ScopeOutcome::Unavailable { .. }) | None => None,
            };
            let snapshot = generations
                .get(&scope.scope_digest)
                .copied()
                .ok_or(MultiRootQueryError::RootSetMismatch)?;
            let exhausted = match resume {
                Some(ScopeOutcome::Exact(None)) => Some(ScopeOutcome::Exact(MultiRootRootPageV1 {
                    value: Vec::new(),
                    next_cursor: None,
                })),
                Some(ScopeOutcome::Partial {
                    value: None,
                    reason,
                }) => Some(ScopeOutcome::Partial {
                    value: MultiRootRootPageV1 {
                        value: Vec::new(),
                        next_cursor: None,
                    },
                    reason: *reason,
                }),
                Some(ScopeOutcome::Denied) => Some(ScopeOutcome::Denied),
                Some(ScopeOutcome::Unavailable { reason }) => {
                    Some(ScopeOutcome::Unavailable { reason: *reason })
                }
                Some(ScopeOutcome::Exact(Some(_)))
                | Some(ScopeOutcome::Partial { value: Some(_), .. })
                | None => None,
            };
            let page = if let Some(exhausted) = exhausted {
                exhausted
            } else {
                match &snapshot.outcome {
                    ScopeOutcome::Exact(generation) => {
                        let context = contexts
                            .get(&scope.scope_digest)
                            .copied()
                            .ok_or(MultiRootQueryError::RootSetMismatch)?;
                        self.port.query_root(
                            context,
                            generation.as_ref(),
                            &request.query,
                            child_cursor,
                        )
                    }
                    ScopeOutcome::Partial {
                        value: generation,
                        reason,
                    } => {
                        let context = contexts
                            .get(&scope.scope_digest)
                            .copied()
                            .ok_or(MultiRootQueryError::RootSetMismatch)?;
                        match self.port.query_root(
                            context,
                            generation.as_ref(),
                            &request.query,
                            child_cursor,
                        ) {
                            ScopeOutcome::Exact(value) => ScopeOutcome::Partial {
                                value,
                                reason: *reason,
                            },
                            outcome => outcome,
                        }
                    }
                    ScopeOutcome::Denied => ScopeOutcome::Denied,
                    ScopeOutcome::Unavailable { reason } => {
                        ScopeOutcome::Unavailable { reason: *reason }
                    }
                }
            };
            let (outcome, cursor_outcome) = match page {
                ScopeOutcome::Exact(page) => (
                    ScopeOutcome::Exact(page.value),
                    ScopeOutcome::Exact(page.next_cursor),
                ),
                ScopeOutcome::Partial {
                    value: page,
                    reason,
                } => (
                    ScopeOutcome::Partial {
                        value: page.value,
                        reason,
                    },
                    ScopeOutcome::Partial {
                        value: page.next_cursor,
                        reason,
                    },
                ),
                ScopeOutcome::Denied => (ScopeOutcome::Denied, ScopeOutcome::Denied),
                ScopeOutcome::Unavailable { reason } => (
                    ScopeOutcome::Unavailable { reason },
                    ScopeOutcome::Unavailable { reason },
                ),
            };
            roots.push(
                RootScopeOutcomeV1::new(scope.scope_digest.clone(), outcome)
                    .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?,
            );
            root_cursors.push(
                RootScopeOutcomeV1::new(scope.scope_digest.clone(), cursor_outcome)
                    .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?,
            );
        }

        let aggregate = aggregate_outcomes(&roots);
        let has_next = root_cursors.iter().any(|root| match &root.outcome {
            ScopeOutcome::Exact(cursor) | ScopeOutcome::Partial { value: cursor, .. } => {
                cursor.is_some()
            }
            ScopeOutcome::Denied | ScopeOutcome::Unavailable { .. } => false,
        });
        let continuation = if has_next {
            let next_page = request
                .page
                .checked_add(1)
                .ok_or_else(|| MultiRootQueryError::Invalid("page overflow".to_owned()))?;
            Some(MultiRootContinuationV1::new(
                request.scope_set.digest().clone(),
                request.root_generations,
                root_cursors,
                request.query_digest,
                request.order_digest,
                next_page,
            )?)
        } else {
            None
        };
        Ok(MultiRootQueryPageV1 {
            scope_set_id: request.scope_set.scope_set_id().clone(),
            scope_set_revision: request.scope_set.revision(),
            scope_set_digest: request.scope_set.digest().clone(),
            roots,
            aggregate,
            continuation,
        })
    }
}

fn validate_contexts<Q>(
    request: &MultiRootQueryRequestV1<Q>,
) -> Result<BTreeMap<ManifestDigest, &RequestContext>, MultiRootQueryError> {
    let admitted_count = request
        .root_generations
        .iter()
        .filter(|generation| {
            matches!(
                generation.outcome,
                ScopeOutcome::Exact(_) | ScopeOutcome::Partial { .. }
            )
        })
        .count();
    if request.contexts.len() != admitted_count {
        return Err(MultiRootQueryError::RootSetMismatch);
    }
    let mut contexts = BTreeMap::new();
    for context in &request.contexts {
        context
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        if context.actor() != request.scope_set.actor_id()
            || context.admission_at(request.observed_at) != RequestAdmission::Admitted
            || !context.allows(&request.capability_id, &request.use_case_id)
            || contexts
                .insert(context.scope().scope_digest.clone(), context)
                .is_some()
        {
            return Err(MultiRootQueryError::Denied);
        }
    }
    if request.scope_set.roots().iter().any(|root| {
        let scope = root.scope();
        request
            .root_generations
            .iter()
            .find(|generation| generation.scope_digest == scope.scope_digest)
            .is_some_and(|generation| {
                matches!(
                    generation.outcome,
                    ScopeOutcome::Exact(_) | ScopeOutcome::Partial { .. }
                ) && contexts
                    .get(&scope.scope_digest)
                    .is_none_or(|context| context.scope() != scope)
            })
    }) {
        return Err(MultiRootQueryError::RootSetMismatch);
    }
    Ok(contexts)
}

fn validate_generations<Q>(
    request: &MultiRootQueryRequestV1<Q>,
) -> Result<
    BTreeMap<ManifestDigest, &RootScopeOutcomeV1<Option<RootGenerationV1>>>,
    MultiRootQueryError,
> {
    if request.root_generations.len() != request.scope_set.roots().len() {
        return Err(MultiRootQueryError::RootSetMismatch);
    }
    let mut generations = BTreeMap::new();
    for generation in &request.root_generations {
        generation
            .scope_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        match &generation.outcome {
            ScopeOutcome::Exact(Some(value))
            | ScopeOutcome::Partial {
                value: Some(value), ..
            } => RootScopeOutcomeV1::new(
                generation.scope_digest.clone(),
                ScopeOutcome::Exact(value.clone()),
            )
            .and_then(|root| root.validate_generation())
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?,
            ScopeOutcome::Exact(None)
            | ScopeOutcome::Partial { value: None, .. }
            | ScopeOutcome::Denied
            | ScopeOutcome::Unavailable { .. } => {}
        }
        if generations
            .insert(generation.scope_digest.clone(), generation)
            .is_some()
        {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
    }
    if request
        .scope_set
        .roots()
        .iter()
        .any(|root| !generations.contains_key(&root.scope().scope_digest))
    {
        return Err(MultiRootQueryError::RootSetMismatch);
    }
    Ok(generations)
}

fn validate_continuation<Q>(
    request: &MultiRootQueryRequestV1<Q>,
) -> Result<(), MultiRootQueryError> {
    let Some(continuation) = &request.continuation else {
        return if request.page == 0 {
            Ok(())
        } else {
            Err(MultiRootQueryError::CursorMismatch { field: "page" })
        };
    };
    continuation.validate()?;
    if continuation.scope_set_digest != *request.scope_set.digest() {
        return Err(MultiRootQueryError::CursorMismatch {
            field: "scope set digest",
        });
    }
    let mut generations = request.root_generations.clone();
    generations.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
    if continuation.root_generations != generations {
        return Err(MultiRootQueryError::CursorMismatch {
            field: "root generations",
        });
    }
    if continuation.query_digest != request.query_digest {
        return Err(MultiRootQueryError::CursorMismatch {
            field: "query digest",
        });
    }
    if continuation.order_digest != request.order_digest {
        return Err(MultiRootQueryError::CursorMismatch {
            field: "order digest",
        });
    }
    if continuation.next_page != request.page {
        return Err(MultiRootQueryError::CursorMismatch { field: "page" });
    }
    Ok(())
}

fn aggregate_outcomes<T: Clone>(roots: &[RootScopeOutcomeV1<Vec<T>>]) -> ScopeOutcome<Vec<T>> {
    let mut values = Vec::new();
    let mut value_outcomes = 0_usize;
    let mut partial_reason = None;
    let mut denied = false;
    let mut unavailable = None;
    for root in roots {
        match &root.outcome {
            ScopeOutcome::Exact(root_values) => {
                value_outcomes += 1;
                values.extend(root_values.iter().cloned());
            }
            ScopeOutcome::Partial {
                value: root_values,
                reason,
            } => {
                value_outcomes += 1;
                partial_reason.get_or_insert(*reason);
                values.extend(root_values.iter().cloned());
            }
            ScopeOutcome::Denied => denied = true,
            ScopeOutcome::Unavailable { reason } => {
                unavailable.get_or_insert(*reason);
            }
        }
    }
    if value_outcomes == roots.len() && partial_reason.is_none() {
        ScopeOutcome::Exact(values)
    } else if value_outcomes > 0 {
        ScopeOutcome::Partial {
            value: values,
            reason: partial_reason.unwrap_or(if denied {
                ScopePartialReasonV1::RootDenied
            } else {
                ScopePartialReasonV1::RootUnavailable
            }),
        }
    } else if let Some(reason) = unavailable {
        ScopeOutcome::Unavailable { reason }
    } else if denied {
        ScopeOutcome::Denied
    } else {
        ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
        }
    }
}
