use std::fmt;

use serde::Deserialize;
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, FusionProfileId, PrincipalId, QueryNormalizationRevision,
    RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot, SanitizerRevision,
    TemporalModeV1,
};

use super::ports::{RetrievalPortError, contract_error};

/// Transport-boundary request. Raw query text exists only in this consumed
/// DTO and is converted immediately into request-local sanitized execution
/// state.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRetrievalRequestV1 {
    query: String,
    principal: PrincipalId,
    scope: RetrievalScope,
    temporal_mode: TemporalModeV1,
    snapshot: RetrievalSnapshot,
    profile_id: FusionProfileId,
    budget: RetrievalBudget,
}

impl fmt::Debug for RawRetrievalRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawRetrievalRequestV1")
            .field("query", &format!("<{} bytes redacted>", self.query.len()))
            .field("principal", &self.principal)
            .field("scope", &self.scope)
            .field("temporal_mode", &self.temporal_mode)
            .field("snapshot", &self.snapshot)
            .field("profile_id", &self.profile_id)
            .field("budget", &self.budget)
            .finish()
    }
}

/// Query-free request metadata paired with its non-cloneable, non-serializable
/// request-local query view.
pub struct SanitizedRetrievalRequestV1 {
    request: RetrievalRequest,
    query_view: EphemeralSanitizedQueryViewV1,
}

impl fmt::Debug for SanitizedRetrievalRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SanitizedRetrievalRequestV1")
            .field("request", &self.request)
            .field("query_view", &self.query_view)
            .finish()
    }
}

impl RawRetrievalRequestV1 {
    pub fn new(query: impl Into<String>, request: RetrievalRequest) -> Self {
        Self {
            query: query.into(),
            principal: request.principal,
            scope: request.scope,
            temporal_mode: request.temporal_mode,
            snapshot: request.snapshot,
            profile_id: request.profile_id,
            budget: request.budget,
        }
    }

    pub fn sanitize(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
    ) -> Result<SanitizedRetrievalRequestV1, RetrievalPortError> {
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )
        .map_err(contract_error)?;
        let request = RetrievalRequest {
            principal: self.principal,
            scope: self.scope,
            temporal_mode: self.temporal_mode,
            snapshot: self.snapshot,
            profile_id: self.profile_id,
            budget: self.budget,
        };
        request
            .budget
            .validate()
            .map_err(contract_error)?;
        Ok(SanitizedRetrievalRequestV1 {
            request,
            query_view,
        })
    }
}

impl SanitizedRetrievalRequestV1 {
    pub fn request(&self) -> &RetrievalRequest {
        &self.request
    }

    pub fn query_view(&self) -> &EphemeralSanitizedQueryViewV1 {
        &self.query_view
    }
}
