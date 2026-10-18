//! Current-authority discovery: everything a joined node needs to capture
//! offline for its enrolled scope, read from the authority's own durable rows.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, RemoteCredentialFingerprintV1, canonical_sha256,
};
use tracedecay_tool_catalog::SchemaId;

use crate::{ApplicationContractError, ResultContractRef};

use super::auth::RemoteEnrollmentCommitReceiptV1;
use super::capture::RemoteWriterAuthorityV1;
use super::protocol::{CurrentAuthorityRequestV1, RemoteProtocolRequestV1};
use super::replay::RemoteReplayPolicyEvidenceV1;

/// The authority's answer to an enrolled node. It carries no credential
/// bytes, store paths, or bindings: the node adopts the enrollment record and
/// commit receipt verbatim, so both sides agree on enrollment identity and
/// revision by construction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteAuthorityDiscoveryV1 {
    pub writer: RemoteWriterAuthorityV1,
    pub policy: RemoteReplayPolicyEvidenceV1,
    pub enrollment: RemoteEnrollmentCommitReceiptV1,
}

impl RemoteAuthorityDiscoveryV1 {
    /// Accepts an authenticated answer only for the exact enrollment the node
    /// asked about and only when it binds the credential the node presented,
    /// so an authority can never hand a node someone else's identity.
    pub fn verify_for(
        &self,
        request: &RemoteProtocolRequestV1<CurrentAuthorityRequestV1>,
        presented: &RemoteCredentialFingerprintV1,
        authority: &CurrentRemoteAuthorityStateV1,
    ) -> Result<(), ApplicationContractError> {
        let inconsistent = ApplicationContractError::Inconsistent {
            field: "remote authority discovery",
        };
        self.enrollment
            .validate()
            .map_err(|_| inconsistent.clone())?;
        self.writer.validate().map_err(|_| inconsistent.clone())?;
        self.policy.validate().map_err(|_| inconsistent.clone())?;
        let record = &self.enrollment.enrollment;
        let committed = canonical_sha256(record).map_err(|_| inconsistent.clone())?;
        if record.brain_id != request.brain_id
            || record.node_id != request.caller_node_id
            || record.revision != request.enrollment_revision
            || record.scope != request.body.scope
            || &record.fingerprint != presented
            || committed != self.enrollment.committed_state_digest
            || self.writer.scope != record.scope
            || self.writer.authority.fence.brain_id != record.brain_id
            || self.policy.repository_scope != record.scope
            || *authority != CurrentRemoteAuthorityStateV1::Available(self.writer.authority.clone())
        {
            return Err(inconsistent);
        }
        Ok(())
    }
}

pub fn remote_authority_result_contract_v1() -> Result<ResultContractRef, ApplicationContractError>
{
    let schema = SchemaId::new("remote.authority.result").map_err(|_| {
        ApplicationContractError::InvalidIdentifier {
            field: "remote authority result schema",
        }
    })?;
    ResultContractRef::new(schema, 1)
}
