//! Versioned, bounded Remote Brain query-composition wire contract.
//!
//! This contract deliberately transports only composition evidence. Concrete
//! product query records remain owned by their established application API;
//! a remote response cannot smuggle an untyped JSON payload around those
//! contracts.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, CurrentRemoteAuthorityStateV1, RemoteCapabilityV1,
    RemoteRepositoryScopeV1, RemoteWriterFenceV1, UtcMicros,
};
use tracedecay_store::StoredObservationRowV1;
use tracedecay_tool_catalog::SchemaId;

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentCredentialLookupPortV1, authenticate_caller,
};
use super::composition::{ExpectedRemoteShardV1, RemoteQueryCompositionV1, ShardCoverageStateV1};
use super::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolBodyV1, RemoteProtocolFailureV1,
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_protocol_problem,
};
use super::replay::{RemoteReplayClockPortV1, SystemRemoteReplayClockV1};
use crate::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, ResolvedScope,
    ResultContractRef,
};

pub const REMOTE_QUERY_SCHEMA_REVISION_V1: u16 = 1;
pub const MAX_REMOTE_QUERY_PAGE_SIZE_V1: u16 = 100;
pub const MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1: usize = 64;
pub const MAX_REMOTE_QUERY_CURSOR_BYTES_V1: usize = 512;
pub const REMOTE_EXACT_OBSERVATION_QUERY_USE_CASE_V1: &str =
    "use-case.remote.query.exact-observation";

pub fn remote_exact_observation_query_result_contract_v1() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("remote.query.exact-observation.result")
            .expect("static exact observation query schema is canonical"),
        1,
    )
    .expect("static exact observation query result contract is canonical")
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum RemoteQueryOperationV1 {
    ExactObservation {
        observation_id: CanonicalObservationIdV1,
    },
}

/// Bounded continuation metadata. The cursor is opaque to the caller and
/// bound by the serving owner to exact identity/generation inventory.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryPageBoundsV1 {
    pub page_size: u16,
    pub cursor: Option<String>,
}

impl RemoteQueryPageBoundsV1 {
    pub fn new(page_size: u16, cursor: Option<String>) -> Result<Self, ApplicationContractError> {
        let value = Self { page_size, cursor };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.page_size == 0 || self.page_size > MAX_REMOTE_QUERY_PAGE_SIZE_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote query page size",
            });
        }
        if let Some(cursor) = &self.cursor
            && (cursor.is_empty()
                || cursor.len() > MAX_REMOTE_QUERY_CURSOR_BYTES_V1
                || cursor.trim() != cursor
                || cursor.chars().any(char::is_control))
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "remote query cursor",
            });
        }
        Ok(())
    }
}

/// Query only for authenticated composition/coverage of one exact repository
/// scope and explicitly expected immutable shard generations.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryRequestV1 {
    pub schema_revision: u16,
    pub scope: RemoteRepositoryScopeV1,
    pub expected_shards: Vec<ExpectedRemoteShardV1>,
    pub expected_authority: RemoteWriterFenceV1,
    pub operation: RemoteQueryOperationV1,
    pub page: RemoteQueryPageBoundsV1,
}

impl RemoteQueryRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.schema_revision != REMOTE_QUERY_SCHEMA_REVISION_V1 {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote query schema revision",
            });
        }
        self.scope.validate()?;
        self.expected_authority
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "remote query expected authority",
            })?;
        self.page.validate()?;
        if self.expected_shards.is_empty()
            || self.expected_shards.len() > MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote query expected shard inventory",
            });
        }
        let mut inventory = BTreeSet::new();
        let mut brain_id = None;
        for shard in &self.expected_shards {
            for (field, value) in [
                ("remote query Brain identity", shard.brain_id.as_str()),
                ("remote query shard identity", shard.shard_id.as_str()),
                (
                    "remote query generation identity",
                    shard.generation_id.as_str(),
                ),
            ] {
                if value.is_empty()
                    || value.len() > 512
                    || value.trim() != value
                    || value.chars().any(char::is_control)
                {
                    return Err(ApplicationContractError::InvalidIdentifier { field });
                }
            }
            if brain_id
                .as_ref()
                .is_some_and(|expected: &String| expected != &shard.brain_id)
                || !inventory.insert(shard.clone())
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "remote query expected shard inventory",
                });
            }
            brain_id.get_or_insert_with(|| shard.brain_id.clone());
        }
        Ok(())
    }

    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        match &self.operation {
            RemoteQueryOperationV1::ExactObservation { observation_id } => observation_id,
        }
    }
}

impl RemoteProtocolBodyV1 for RemoteQueryRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
    }
}

/// A wire-distinct marker that proves an authorized shard supplied a complete
/// query value. It must not collapse to JSON `null`, which is reserved for a
/// denied, partial, or unavailable contribution with no disclosable value.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryCompleteValueV1 {
    pub complete_value_present: bool,
}

impl RemoteQueryCompleteValueV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if !self.complete_value_present {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote complete query value marker",
            });
        }
        Ok(())
    }
}

/// Canonical Remote Brain composition response. Per-shard `null` means no
/// disclosed value; a complete value is the explicit object above.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryResultV1 {
    pub composition: RemoteQueryCompositionV1<RemoteQueryCompleteValueV1>,
    pub observation: RemoteExactObservationResultV1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum RemoteExactObservationResultV1 {
    Found(Box<StoredObservationRowV1>),
    NotFound,
}

impl RemoteQueryResultV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for contribution in &self.composition.contributions {
            contribution.validate()?;
            if let Some(value) = &contribution.value {
                value.validate()?;
            }
            if contribution.coverage == ShardCoverageStateV1::Complete
                && contribution.value.is_none()
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "remote complete query value",
                });
            }
        }
        Ok(())
    }
}

pub struct RemoteExactObservationQueryOutcomeV1 {
    pub authority: CurrentRemoteAuthorityStateV1,
    pub result: ApplicationEnvelope<RemoteQueryResultV1>,
}

pub trait RemoteExactObservationQueryReadPortV1: Send + Sync {
    fn read_exact_observation(
        &self,
        request: &RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        caller: &tracedecay_domain::EnrollmentCredentialRecordV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1>;
}

pub struct RemoteExactObservationQueryServiceV1 {
    credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
    read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
    clock: Arc<dyn RemoteReplayClockPortV1>,
}

impl RemoteExactObservationQueryServiceV1 {
    pub fn new(
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
    ) -> Self {
        Self::new_with_clock(credentials, read, Arc::new(SystemRemoteReplayClockV1))
    }

    pub fn new_with_clock(
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
        clock: Arc<dyn RemoteReplayClockPortV1>,
    ) -> Self {
        Self {
            credentials,
            read,
            clock,
        }
    }

    pub fn query(
        &self,
        request: &RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: &OpaqueRemoteCredential,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1> {
        if request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteExactObservationQueryErrorV1::UnsupportedVersion);
        }
        request
            .validate_metadata()
            .and_then(|()| request.body.validate())
            .map_err(|_| RemoteExactObservationQueryErrorV1::InvalidRequest)?;
        let observed_at = self
            .clock
            .now()
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        let caller = self
            .credentials
            .authority_enrollment(
                &request.brain_id,
                &request.caller_node_id,
                request.enrollment_revision,
            )
            .map_err(RemoteExactObservationQueryErrorV1::Credential)?;
        authenticate_caller(
            &caller,
            credential,
            &request.brain_id,
            RemoteCapabilityV1::Query,
            &request.body.scope,
            observed_at,
        )
        .map_err(RemoteExactObservationQueryErrorV1::Authentication)?;
        let outcome = self
            .read
            .read_exact_observation(request, &caller, observed_at)?;
        if outcome.result.contract != remote_exact_observation_query_result_contract_v1()
            || outcome.result.request_id != request.request_id
            || !resolved_scope_matches(&outcome.result.scope, &request.body.scope)
        {
            return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
        }
        query_payload(&outcome.result)
            .ok_or(RemoteExactObservationQueryErrorV1::ReceiptMismatch)?
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        if let Some(row) = exact_observation_row(&outcome.result)
            && row.observation.observation_id() != request.body.observation_id()
        {
            return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
        }
        Ok(outcome)
    }
}

fn resolved_scope_matches(resolved: &ResolvedScope, scope: &RemoteRepositoryScopeV1) -> bool {
    resolved.project_id == scope.project_id
        && resolved.repository_id == scope.repository_id
        && resolved.worktree_id == scope.worktree_id
        && resolved.reference == scope.reference
}

fn exact_observation_row(
    envelope: &ApplicationEnvelope<RemoteQueryResultV1>,
) -> Option<&StoredObservationRowV1> {
    let payload = query_payload(envelope)?;
    match &payload.observation {
        RemoteExactObservationResultV1::Found(row) => Some(row),
        RemoteExactObservationResultV1::NotFound => None,
    }
}

fn query_payload(
    envelope: &ApplicationEnvelope<RemoteQueryResultV1>,
) -> Option<&RemoteQueryResultV1> {
    match &envelope.outcome {
        ApplicationOutcome::Evidence(packet) => packet.payload.as_ref(),
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => None,
    }
}

pub struct RemoteExactObservationQueryProtocolAdapterV1 {
    service: RemoteExactObservationQueryServiceV1,
}

impl RemoteExactObservationQueryProtocolAdapterV1 {
    pub fn new(service: RemoteExactObservationQueryServiceV1) -> Self {
        Self { service }
    }
}

impl RemoteProtocolPortV1<RemoteQueryRequestV1> for RemoteExactObservationQueryProtocolAdapterV1 {
    type Output = RemoteQueryResultV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        let request_id = request.request_id.clone();
        let fallback_authority = CurrentRemoteAuthorityStateV1::Partial {
            known_fence: Some(request.body.expected_authority.clone()),
            missing: BTreeSet::from([
                tracedecay_domain::RemoteAuthorityUnavailableReasonV1::FenceUnverified,
            ]),
            observed_at: request.sent_at,
        };
        match self.service.query(&request, &credential) {
            Ok(outcome) => {
                RemoteProtocolResponseV1::new(request_id, outcome.authority, Ok(outcome.result))
                    .expect("query owner preserves response identities")
            }
            Err(error) => {
                let authority = if matches!(
                    &error,
                    RemoteExactObservationQueryErrorV1::Authentication(_)
                        | RemoteExactObservationQueryErrorV1::Credential(
                            RemoteEnrollmentAuthorityErrorV1::GrantNotFound
                        )
                ) {
                    CurrentRemoteAuthorityStateV1::Unavailable {
                        reason:
                            tracedecay_domain::RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                        observed_at: request.sent_at,
                    }
                } else {
                    fallback_authority
                };
                let failure = query_protocol_failure(error);
                RemoteProtocolResponseV1::new(
                    request_id.clone(),
                    authority,
                    Err(remote_protocol_problem(
                        remote_exact_observation_query_result_contract_v1(),
                        request_id,
                        failure,
                    )),
                )
                .expect("query owner preserves problem identities")
            }
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteExactObservationQueryErrorV1 {
    #[error("remote exact observation query version is unsupported")]
    UnsupportedVersion,
    #[error("remote exact observation query request is invalid")]
    InvalidRequest,
    #[error("remote exact observation query caller authentication failed")]
    Authentication(RemoteAuthenticationError),
    #[error("remote exact observation query credential authority failed")]
    Credential(RemoteEnrollmentAuthorityErrorV1),
    #[error("remote exact observation query scope is mismatched")]
    ScopeMismatch,
    #[error("remote exact observation query authority fence is stale")]
    StaleFence,
    #[error("remote exact observation query policy denied access")]
    PolicyDenied,
    #[error("remote exact observation query policy is unavailable")]
    PolicyUnavailable,
    #[error("remote exact observation query authority is unavailable")]
    AuthorityUnavailable,
    #[error("remote exact observation query budget was exceeded")]
    BudgetExceeded,
    #[error("remote exact observation query deadline elapsed")]
    DeadlineElapsed,
    #[error("remote exact observation query receipt is mismatched")]
    ReceiptMismatch,
}

fn query_protocol_failure(error: RemoteExactObservationQueryErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteExactObservationQueryErrorV1::UnsupportedVersion => {
            RemoteProtocolFailureV1::UnsupportedVersion
        }
        RemoteExactObservationQueryErrorV1::InvalidRequest
        | RemoteExactObservationQueryErrorV1::ScopeMismatch
        | RemoteExactObservationQueryErrorV1::ReceiptMismatch => {
            RemoteProtocolFailureV1::ScopeMismatch
        }
        RemoteExactObservationQueryErrorV1::Authentication(authentication) => {
            match authentication {
                RemoteAuthenticationError::Expired => RemoteProtocolFailureV1::EnrollmentExpired,
                RemoteAuthenticationError::Revoked => RemoteProtocolFailureV1::EnrollmentRevoked,
                RemoteAuthenticationError::InsufficientCapability => {
                    RemoteProtocolFailureV1::InsufficientCapability
                }
                RemoteAuthenticationError::StaleRevision
                | RemoteAuthenticationError::RevisionOverflow => {
                    RemoteProtocolFailureV1::StaleCredentialRevision
                }
                _ => RemoteProtocolFailureV1::CallerAuthenticationFailed,
            }
        }
        RemoteExactObservationQueryErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::GrantConsumed,
        ) => RemoteProtocolFailureV1::StaleCredentialRevision,
        RemoteExactObservationQueryErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::GrantNotFound,
        ) => RemoteProtocolFailureV1::CallerAuthenticationFailed,
        RemoteExactObservationQueryErrorV1::Credential(_)
        | RemoteExactObservationQueryErrorV1::PolicyUnavailable
        | RemoteExactObservationQueryErrorV1::AuthorityUnavailable => {
            RemoteProtocolFailureV1::AuthorityUnavailable
        }
        RemoteExactObservationQueryErrorV1::StaleFence => {
            RemoteProtocolFailureV1::StaleAuthorityFence
        }
        RemoteExactObservationQueryErrorV1::PolicyDenied => {
            RemoteProtocolFailureV1::InsufficientCapability
        }
        RemoteExactObservationQueryErrorV1::BudgetExceeded
        | RemoteExactObservationQueryErrorV1::DeadlineElapsed => {
            RemoteProtocolFailureV1::AuthorityUnavailable
        }
    }
}
