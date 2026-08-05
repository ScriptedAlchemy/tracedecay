//! Opaque authenticated continuation contracts for federated reads.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    AuthorityEpoch, ManifestDigest, RootGenerationV1, RootScopeOutcomeV1, ScopeSetId,
    ScopeSetRevision, UtcMicros, WorkProjectionResumeCursorV1, canonical_sha256,
};

use super::MultiRootQueryError;
use crate::{OpaqueCursor, RequestContext};

const MULTI_ROOT_AUTHORIZATION_BINDING_DOMAIN_V1: &str =
    "tracedecay.application.multi-root-authorization-binding.v1";
const MAX_MULTI_ROOT_CONTINUATION_BYTES: usize = 262_144;

/// Opaque authenticated continuation. Only the daemon-owned cursor authority
/// can open the frozen state carried by this value.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct MultiRootContinuationV1(#[schemars(length(min = 1, max = 262_144))] String);

impl<'de> Deserialize<'de> for MultiRootContinuationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_opaque(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl MultiRootContinuationV1 {
    pub fn from_opaque(value: impl Into<String>) -> Result<Self, MultiRootQueryError> {
        let value = value.into();
        if !value.starts_with("mr1.")
            || value.len() > MAX_MULTI_ROOT_CONTINUATION_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(MultiRootQueryError::Invalid(
                "multi-root continuation is not a canonical opaque token".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        Self::from_opaque(self.0.clone()).map(|_| ())
    }
}

/// Total-order checkpoint for the last item emitted by one fused page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootTotalOrderKeyV1 {
    pub fused_rank: u64,
    pub root_ordinal: u32,
    pub evidence_digest: ManifestDigest,
}

impl MultiRootTotalOrderKeyV1 {
    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        self.evidence_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))
    }
}

/// Per-root continuation owned by the root's canonical application surface.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MultiRootRootContinuationV1 {
    Page(OpaqueCursor),
    Work(WorkProjectionResumeCursorV1),
    Complete,
}

/// Per-root continuation owned by the root's canonical application surface.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootRootCursorV1 {
    pub scope_digest: ManifestDigest,
    pub cursor: Option<MultiRootRootContinuationV1>,
}

impl MultiRootRootCursorV1 {
    pub fn new(
        scope_digest: ManifestDigest,
        cursor: Option<MultiRootRootContinuationV1>,
    ) -> Result<Self, MultiRootQueryError> {
        scope_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        Ok(Self {
            scope_digest,
            cursor,
        })
    }
}

/// Current authorization identity revalidated on every continuation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootAuthorizationBindingV1 {
    pub epoch: AuthorityEpoch,
    pub digest: ManifestDigest,
    pub expires_at: UtcMicros,
}

impl MultiRootAuthorizationBindingV1 {
    pub fn from_contexts(contexts: &[RequestContext]) -> Result<Self, MultiRootQueryError> {
        if contexts.is_empty() {
            return Err(MultiRootQueryError::Denied);
        }
        let epoch = contexts
            .iter()
            .map(|context| context.grant().revision)
            .max()
            .filter(|epoch| *epoch > 0)
            .ok_or(MultiRootQueryError::Denied)?;
        let expires_at = contexts
            .iter()
            .map(|context| context.grant().expires_at)
            .min()
            .ok_or(MultiRootQueryError::Denied)?;
        let mut grants = contexts
            .iter()
            .map(|context| {
                (
                    context.scope().scope_digest.clone(),
                    context.grant().grant_id.clone(),
                    context.grant().revision,
                    context.grant().digest.clone(),
                    context.grant().expires_at,
                )
            })
            .collect::<Vec<_>>();
        grants.sort_by(|left, right| left.0.cmp(&right.0));
        let digest = canonical_sha256(&(MULTI_ROOT_AUTHORIZATION_BINDING_DOMAIN_V1, &grants))
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        Ok(Self {
            epoch: AuthorityEpoch(epoch),
            digest,
            expires_at,
        })
    }
}

/// Authenticated payload carried inside [`MultiRootContinuationV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootContinuationStateV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub root_generations: Vec<RootScopeOutcomeV1<RootGenerationV1>>,
    pub root_cursors: Vec<MultiRootRootCursorV1>,
    pub query_digest: ManifestDigest,
    pub order_digest: ManifestDigest,
    pub authorization: MultiRootAuthorizationBindingV1,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub next_page: u64,
    pub last_order_key: Option<MultiRootTotalOrderKeyV1>,
}

impl MultiRootContinuationStateV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope_set_id: ScopeSetId,
        scope_set_revision: ScopeSetRevision,
        scope_set_digest: ManifestDigest,
        root_generations: Vec<RootScopeOutcomeV1<RootGenerationV1>>,
        root_cursors: Vec<MultiRootRootCursorV1>,
        query_digest: ManifestDigest,
        order_digest: ManifestDigest,
        authorization: MultiRootAuthorizationBindingV1,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
        next_page: u64,
        last_order_key: Option<MultiRootTotalOrderKeyV1>,
    ) -> Result<Self, MultiRootQueryError> {
        let state = Self {
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
            root_generations,
            root_cursors,
            query_digest,
            order_digest,
            authorization,
            issued_at,
            expires_at,
            next_page,
            last_order_key,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        self.scope_set_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.scope_set_revision
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.scope_set_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.query_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.order_digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.authorization
            .digest
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        if self.authorization.epoch.0 == 0
            || self.root_generations.is_empty()
            || self.next_page == 0
            || self.expires_at <= self.issued_at
            || self.expires_at > self.authorization.expires_at
        {
            return Err(MultiRootQueryError::Invalid(
                "multi-root continuation state is not canonical".to_owned(),
            ));
        }
        if self.root_cursors.len() != self.root_generations.len() {
            return Err(MultiRootQueryError::RootSetMismatch);
        }
        let mut scopes = BTreeMap::new();
        for (generation, cursor) in self.root_generations.iter().zip(&self.root_cursors) {
            generation
                .validate_generation()
                .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
            cursor
                .scope_digest
                .validate()
                .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
            if cursor.scope_digest != generation.scope_digest
                || scopes.insert(&generation.scope_digest, ()).is_some()
            {
                return Err(MultiRootQueryError::RootSetMismatch);
            }
        }
        if let Some(key) = &self.last_order_key {
            key.validate()?;
        }
        Ok(())
    }
}
