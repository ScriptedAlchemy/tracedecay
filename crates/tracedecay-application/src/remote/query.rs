//! Versioned, bounded Remote Brain query-composition wire contract.
//!
//! This contract deliberately transports only composition evidence. Concrete
//! product query records remain owned by their established application API;
//! a remote response cannot smuggle an untyped JSON payload around those
//! contracts.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, CurrentRemoteAuthorityStateV1, DurableObservationV1,
    EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1, ObservationScopeV1,
    ObservationSourceCursorV1, ProjectionGenerationId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_tool_catalog::SchemaId;

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentCredentialLookupPortV1, authenticate_caller,
};
use super::composition::{ExpectedRemoteShardV1, RemoteQueryCompositionV1, ShardCoverageStateV1};
use super::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolBodyV1, RemoteProtocolFailureV1,
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_protocol_problem,
};
use crate::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, AuthorityReceipt, Deadline,
    RequestId, ResolvedScope, ResultContractRef,
};

pub const REMOTE_QUERY_SCHEMA_REVISION_V1: u16 = 1;
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
        if self.expected_shards.len() != 1 {
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
        let expected = &self.expected_shards[0];
        if expected.brain_id != self.expected_authority.brain_id.as_str()
            || expected.shard_id != self.expected_authority.shard_id.as_str()
            || expected.generation_id != self.expected_authority.generation_id.as_str()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote query authority inventory binding",
            });
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
    pub returned_observations: u8,
}

impl RemoteQueryCompleteValueV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.returned_observations > 1 {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote complete query observation count",
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
    Found(Box<RemoteSanitizedObservationV1>),
    NotFound,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSanitizedObservationV1 {
    pub sequence: u64,
    pub observation: DurableObservationV1,
    pub committed_cursor: ObservationSourceCursorV1,
    pub retrieval_anchor: RetrievalAnchorRecordV2,
    pub projection_generation: ProjectionGenerationId,
    pub repository_provenance: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
    pub repository_anchor: Option<RetrievalAnchorRecordV2>,
    pub projection_queued: bool,
}

impl RemoteSanitizedObservationV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.retrieval_anchor
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "remote observation retrieval anchor",
            })?;
        if let Some(provenance) = self.repository_provenance.value() {
            provenance
                .validate()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "remote observation repository provenance",
                })?;
        }
        if let Some(anchor) = &self.repository_anchor {
            anchor
                .validate()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "remote observation repository anchor",
                })?;
        }
        if self.sequence == 0
            || self.observation.source() != self.committed_cursor.source()
            || self.observation.scope() != self.committed_cursor.scope()
            || self.observation.identity().generation() != self.committed_cursor.generation()
            || self.observation.identity().ordering_domain()
                != self.committed_cursor.ordering_domain()
            || self.repository_provenance.value().is_some() != self.repository_anchor.is_some()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote sanitized observation evidence",
            });
        }
        Ok(())
    }
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
        if let RemoteExactObservationResultV1::Found(row) = &self.observation {
            row.validate()?;
        }
        Ok(())
    }
}

pub struct RemoteExactObservationQueryOutcomeV1 {
    pub authority: CurrentRemoteAuthorityStateV1,
    pub result: ApplicationEnvelope<RemoteQueryResultV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteExactObservationQueryBudgetV1 {
    pub maximum_units: u64,
    pub maximum_bytes: u64,
    pub maximum_elapsed_micros: u64,
}

impl RemoteExactObservationQueryBudgetV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.maximum_units == 0 || self.maximum_bytes == 0 || self.maximum_elapsed_micros == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote exact observation query budget",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteQueryAuthorizationDecisionV1 {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryPolicyRecordV1 {
    pub repository_scope: RemoteRepositoryScopeV1,
    pub scope: ResolvedScope,
    pub policy_revision: u64,
    pub decision: RemoteQueryAuthorizationDecisionV1,
    pub authority: AuthorityReceipt,
    pub revalidated_at: UtcMicros,
}

impl RemoteQueryPolicyRecordV1 {
    pub fn validate(&self) -> Result<(), RemoteExactObservationQueryErrorV1> {
        self.repository_scope
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.scope
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.authority
            .validate_for(&self.scope)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        if !resolved_scope_matches(&self.scope, &self.repository_scope)
            || self.policy_revision == 0
            || self.authority.policy.revision != self.policy_revision
        {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryAuthorizationEvidenceV1 {
    pub repository_scope: RemoteRepositoryScopeV1,
    pub observation_id: CanonicalObservationIdV1,
    pub expected_authority: RemoteWriterFenceV1,
    pub policy_revision: u64,
    pub decision: RemoteQueryAuthorizationDecisionV1,
    pub authority: AuthorityReceipt,
    pub revalidated_at: UtcMicros,
}

impl RemoteQueryAuthorizationEvidenceV1 {
    pub fn validate_for(
        &self,
        scope: &ResolvedScope,
        repository_scope: &RemoteRepositoryScopeV1,
        observation_id: &CanonicalObservationIdV1,
        expected_authority: &RemoteWriterFenceV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteExactObservationQueryErrorV1> {
        self.repository_scope
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.expected_authority
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.authority
            .validate_for(scope)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        if self.repository_scope != *repository_scope
            || self.observation_id != *observation_id
            || self.expected_authority != *expected_authority
            || self.policy_revision == 0
            || self.authority.policy.revision != self.policy_revision
            || self.revalidated_at > observed_at
        {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        if self.decision == RemoteQueryAuthorizationDecisionV1::Deny {
            return Err(RemoteExactObservationQueryErrorV1::PolicyDenied);
        }
        Ok(())
    }
}

pub trait RemoteQueryAuthorizationPortV1: Send + Sync {
    fn authorize(
        &self,
        scope: &ResolvedScope,
        repository_scope: &RemoteRepositoryScopeV1,
        observation_id: &CanonicalObservationIdV1,
        expected_authority: &RemoteWriterFenceV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthorizationEvidenceV1, RemoteExactObservationQueryErrorV1>;
}

#[derive(Clone, Debug)]
pub struct RemoteExactObservationQueryCommandV1 {
    pub request_id: RequestId,
    pub observation_id: CanonicalObservationIdV1,
    pub scope: ResolvedScope,
    pub repository_scope: RemoteRepositoryScopeV1,
    pub expected_authority: RemoteWriterFenceV1,
    pub expected_shard: ExpectedRemoteShardV1,
    pub caller_admission: RemoteEnrollmentCommitReceiptV1,
    pub query_authorization: RemoteQueryAuthorizationEvidenceV1,
    pub effective_deadline: Deadline,
    pub budget: RemoteExactObservationQueryBudgetV1,
    pub observed_at: UtcMicros,
}

pub trait RemoteExactObservationQueryReadPortV1: Send + Sync {
    fn read_exact_observation(
        &self,
        command: &RemoteExactObservationQueryCommandV1,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1>;
}

pub trait RemoteQueryClockPortV1: Send + Sync {
    fn now(&self) -> Result<UtcMicros, RemoteExactObservationQueryErrorV1>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRemoteQueryClockV1;

impl RemoteQueryClockPortV1 for SystemRemoteQueryClockV1 {
    fn now(&self) -> Result<UtcMicros, RemoteExactObservationQueryErrorV1> {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?
            .as_micros();
        i64::try_from(micros)
            .map(UtcMicros)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)
    }
}

pub struct RemoteExactObservationQueryServiceV1 {
    credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
    authorization: Arc<dyn RemoteQueryAuthorizationPortV1>,
    read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
    clock: Arc<dyn RemoteQueryClockPortV1>,
}

impl RemoteExactObservationQueryServiceV1 {
    pub fn new(
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        authorization: Arc<dyn RemoteQueryAuthorizationPortV1>,
        read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
    ) -> Self {
        Self::new_with_clock(
            credentials,
            authorization,
            read,
            Arc::new(SystemRemoteQueryClockV1),
        )
    }

    pub fn new_with_clock(
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        authorization: Arc<dyn RemoteQueryAuthorizationPortV1>,
        read: Arc<dyn RemoteExactObservationQueryReadPortV1>,
        clock: Arc<dyn RemoteQueryClockPortV1>,
    ) -> Self {
        Self {
            credentials,
            authorization,
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
        validate_protocol_authority_binding(request)?;
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
        let caller_admission = self
            .credentials
            .enrollment_commit_receipt(&caller.enrollment_id)
            .map_err(RemoteExactObservationQueryErrorV1::Credential)?;
        caller_admission
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        if caller_admission.enrollment != caller {
            return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
        }
        let scope = caller_admission.admission.scope().clone();
        if !resolved_scope_matches(&scope, &request.body.scope) {
            return Err(RemoteExactObservationQueryErrorV1::ScopeMismatch);
        }
        let budget = RemoteExactObservationQueryBudgetV1 {
            maximum_units: 1,
            maximum_bytes: 1024 * 1024,
            maximum_elapsed_micros: 5_000_000,
        };
        budget
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        let budget_expires_at = observed_at
            .0
            .checked_add(
                i64::try_from(budget.maximum_elapsed_micros)
                    .map_err(|_| RemoteExactObservationQueryErrorV1::BudgetExceeded)?,
            )
            .map(UtcMicros)
            .ok_or(RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        let effective_deadline = Deadline::new(std::cmp::min(caller.expires_at, budget_expires_at))
            .map_err(|_| RemoteExactObservationQueryErrorV1::DeadlineElapsed)?;
        if effective_deadline.is_elapsed_at(observed_at) {
            return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed);
        }
        let query_authorization = self.authorization.authorize(
            &scope,
            &request.body.scope,
            request.body.observation_id(),
            &request.body.expected_authority,
            observed_at,
        )?;
        query_authorization.validate_for(
            &scope,
            &request.body.scope,
            request.body.observation_id(),
            &request.body.expected_authority,
            observed_at,
        )?;
        let command = RemoteExactObservationQueryCommandV1 {
            request_id: request.request_id.clone(),
            observation_id: request.body.observation_id().clone(),
            scope,
            repository_scope: request.body.scope.clone(),
            expected_authority: request.body.expected_authority.clone(),
            expected_shard: request.body.expected_shards[0].clone(),
            caller_admission,
            query_authorization,
            effective_deadline,
            budget,
            observed_at,
        };
        let outcome = self.read.read_exact_observation(&command)?;
        validate_returned_authority(&outcome.authority, &command.expected_authority)?;
        let publication_observed_at = self.clock.now()?;
        if publication_observed_at > command.effective_deadline.expires_at {
            return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed);
        }
        let publication_authorization = self.authorization.authorize(
            &command.scope,
            &command.repository_scope,
            &command.observation_id,
            &command.expected_authority,
            publication_observed_at,
        )?;
        publication_authorization.validate_for(
            &command.scope,
            &command.repository_scope,
            &command.observation_id,
            &command.expected_authority,
            publication_observed_at,
        )?;
        if publication_authorization != command.query_authorization {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        validate_result_identity(
            &outcome.result.contract,
            &outcome.result.request_id,
            &outcome.result.scope,
            &request.request_id,
            &request.body.scope,
        )?;
        validate_query_evidence(&outcome.result)?;
        let ApplicationOutcome::Evidence(packet) = &outcome.result.outcome else {
            unreachable!("query evidence validation rejects non-evidence outcomes");
        };
        if packet.execution.started_at < command.observed_at
            || packet.execution.ended_at > command.effective_deadline.expires_at
            || packet.execution.budget.units_consumed > command.budget.maximum_units
            || packet.execution.budget.bytes_consumed > command.budget.maximum_bytes
            || packet.execution.budget.elapsed_micros > command.budget.maximum_elapsed_micros
        {
            return Err(RemoteExactObservationQueryErrorV1::BudgetExceeded);
        }
        query_payload(&outcome.result)
            .ok_or(RemoteExactObservationQueryErrorV1::ReceiptMismatch)?
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        if let Some(row) = exact_observation_row(&outcome.result) {
            validate_returned_observation_identity(
                row.observation.observation_id(),
                &row.projection_generation,
                row.observation.scope(),
                request.body.observation_id(),
                &command.expected_authority.generation_id,
                &request.body.scope,
            )?;
            validate_returned_provenance(
                &row.repository_provenance,
                row.repository_anchor
                    .as_ref()
                    .map(RetrievalAnchorRecordV2::projection_generation),
                request.body.observation_id(),
                &command.expected_authority.generation_id,
                &request.body.scope,
            )?;
        }
        validate_composition(
            query_payload(&outcome.result)
                .ok_or(RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
            &command.expected_shard,
            &command.expected_authority,
        )?;
        Ok(outcome)
    }

    fn now(&self) -> Result<UtcMicros, RemoteExactObservationQueryErrorV1> {
        self.clock.now()
    }
}

pub(super) fn validate_returned_provenance(
    repository_provenance: &EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
    repository_anchor_generation: Option<&ProjectionGenerationId>,
    expected_observation_id: &CanonicalObservationIdV1,
    expected_generation: &ProjectionGenerationId,
    expected_scope: &RemoteRepositoryScopeV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    let provenance = repository_provenance
        .value()
        .ok_or(RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
    let capture = provenance.capture();
    if provenance.generation_id() != expected_generation
        || provenance.source_observation() != Some(expected_observation_id)
        || capture.repository_id() != &expected_scope.repository_id
        || capture.project_id() != Some(&expected_scope.project_id)
        || capture.worktree_id() != Some(&expected_scope.worktree_id)
        || capture.evidence().attached_ref().value() != expected_scope.reference.as_ref()
        || repository_anchor_generation != Some(expected_generation)
    {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    Ok(())
}

pub(super) fn validate_returned_authority(
    state: &CurrentRemoteAuthorityStateV1,
    expected: &RemoteWriterFenceV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    match state {
        CurrentRemoteAuthorityStateV1::Available(authority)
            if authority.validate().is_ok() && authority.fence == *expected =>
        {
            Ok(())
        }
        CurrentRemoteAuthorityStateV1::Available(_) => {
            Err(RemoteExactObservationQueryErrorV1::StaleFence)
        }
        CurrentRemoteAuthorityStateV1::Partial { .. }
        | CurrentRemoteAuthorityStateV1::Unavailable { .. } => {
            Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable)
        }
    }
}

pub(super) fn validate_result_identity(
    contract: &ResultContractRef,
    actual_request_id: &RequestId,
    actual_scope: &ResolvedScope,
    expected_request_id: &RequestId,
    expected_scope: &RemoteRepositoryScopeV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    if contract != &remote_exact_observation_query_result_contract_v1()
        || actual_request_id != expected_request_id
        || !resolved_scope_matches(actual_scope, expected_scope)
    {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    Ok(())
}

pub(super) fn validate_returned_observation_identity(
    actual_observation_id: &CanonicalObservationIdV1,
    actual_generation: &ProjectionGenerationId,
    actual_scope: &ObservationScopeV1,
    expected_observation_id: &CanonicalObservationIdV1,
    expected_generation: &ProjectionGenerationId,
    expected_scope: &RemoteRepositoryScopeV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    if actual_observation_id != expected_observation_id
        || actual_generation != expected_generation
        || !matches!(
            actual_scope,
            ObservationScopeV1::Project { project_id }
                if project_id == &expected_scope.project_id
        )
    {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    Ok(())
}

pub(super) fn validate_protocol_authority_binding(
    request: &RemoteProtocolRequestV1<RemoteQueryRequestV1>,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    if request.expected_authority.as_ref() != Some(&request.body.expected_authority) {
        return Err(RemoteExactObservationQueryErrorV1::StaleFence);
    }
    Ok(())
}

fn validate_query_evidence(
    envelope: &ApplicationEnvelope<RemoteQueryResultV1>,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    let ApplicationOutcome::Evidence(packet) = &envelope.outcome else {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    };
    packet
        .authority
        .validate_for(&envelope.scope)
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
    packet
        .coverage
        .validate()
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
    packet
        .execution
        .validate()
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
    if packet.execution.ended_at < packet.execution.started_at {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    Ok(())
}

fn resolved_scope_matches(resolved: &ResolvedScope, scope: &RemoteRepositoryScopeV1) -> bool {
    resolved.project_id == scope.project_id
        && resolved.repository_id == scope.repository_id
        && resolved.worktree_id == scope.worktree_id
        && resolved.reference == scope.reference
}

pub(super) fn validate_composition(
    result: &RemoteQueryResultV1,
    expected: &ExpectedRemoteShardV1,
    fence: &RemoteWriterFenceV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    if result.composition.contributions.len() != 1 || !result.composition.is_complete() {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    let manifest = &result.composition.contributions[0].manifest;
    if manifest.brain_id != expected.brain_id
        || manifest.shard_id != expected.shard_id
        || manifest.generation_id != expected.generation_id
        || manifest.placement_revision != fence.placement_revision.get()
        || manifest.authority_epoch != fence.authority_epoch.0
    {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    Ok(())
}

fn exact_observation_row(
    envelope: &ApplicationEnvelope<RemoteQueryResultV1>,
) -> Option<&RemoteSanitizedObservationV1> {
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
        let observed_at = self.service.now().unwrap_or(UtcMicros(0));
        let fallback_authority = CurrentRemoteAuthorityStateV1::Partial {
            known_fence: Some(request.body.expected_authority.clone()),
            missing: BTreeSet::from([
                tracedecay_domain::RemoteAuthorityUnavailableReasonV1::FenceUnverified,
            ]),
            observed_at,
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
                        observed_at,
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

pub(super) fn query_protocol_failure(
    error: RemoteExactObservationQueryErrorV1,
) -> RemoteProtocolFailureV1 {
    match error {
        RemoteExactObservationQueryErrorV1::UnsupportedVersion => {
            RemoteProtocolFailureV1::UnsupportedVersion
        }
        RemoteExactObservationQueryErrorV1::InvalidRequest
        | RemoteExactObservationQueryErrorV1::ScopeMismatch => {
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
        | RemoteExactObservationQueryErrorV1::AuthorityUnavailable
        | RemoteExactObservationQueryErrorV1::ReceiptMismatch
        | RemoteExactObservationQueryErrorV1::BudgetExceeded
        | RemoteExactObservationQueryErrorV1::DeadlineElapsed => {
            RemoteProtocolFailureV1::AuthorityUnavailable
        }
        RemoteExactObservationQueryErrorV1::StaleFence => {
            RemoteProtocolFailureV1::StaleAuthorityFence
        }
        RemoteExactObservationQueryErrorV1::PolicyDenied => {
            RemoteProtocolFailureV1::InsufficientCapability
        }
    }
}
