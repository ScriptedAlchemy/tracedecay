//! Body-independent remote credential admission.
//!
//! Transports call this boundary after selecting an endpoint capability and
//! before reading or deserializing the request body. The resulting session is
//! deliberately non-serializable and contains no credential bytes. Typed body
//! binding remains a separate required step.

use thiserror::Error;
use tracedecay_domain::{
    BrainId, BrainNodeId, EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1,
    EnrollmentGrantV1, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    UtcMicros,
};

use super::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentCommitReceiptV1,
};
use super::capture_protocol::RemoteCaptureRequestV1;
use super::protocol::{EnrollmentRequestV1, RemoteProtocolBodyV1, RemoteProtocolRequestV1};
use super::query::RemoteQueryRequestV1;
use super::recovery::{BackupRequestV1, PromotionConfirmationV1, StagedRestoreConfirmationV1};
use super::replay::RemoteReplayRequestV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCredentialClassV1 {
    EnrollmentGrant,
    Enrollment,
}

/// Endpoint-owned operation identity. A transport chooses this value from the
/// matched route; no caller-controlled header or body may select it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCredentialUseV1 {
    InitialEnrollment,
    CaptureOffline,
    Replay,
    Query,
    CreateBackup,
    PublishRestore,
    Promote,
}

impl RemoteCredentialUseV1 {
    pub const fn credential_class(self) -> RemoteCredentialClassV1 {
        match self {
            Self::InitialEnrollment => RemoteCredentialClassV1::EnrollmentGrant,
            Self::CaptureOffline
            | Self::Replay
            | Self::Query
            | Self::CreateBackup
            | Self::PublishRestore
            | Self::Promote => RemoteCredentialClassV1::Enrollment,
        }
    }

    pub const fn required_capability(self) -> Option<RemoteCapabilityV1> {
        match self {
            Self::InitialEnrollment => None,
            Self::CaptureOffline => Some(RemoteCapabilityV1::CaptureOffline),
            Self::Replay => Some(RemoteCapabilityV1::Replay),
            Self::Query => Some(RemoteCapabilityV1::Query),
            Self::CreateBackup => Some(RemoteCapabilityV1::CreateBackup),
            Self::PublishRestore => Some(RemoteCapabilityV1::PublishRestore),
            Self::Promote => Some(RemoteCapabilityV1::Promote),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteCredentialLookupErrorV1 {
    #[error("remote credential authority is unavailable")]
    Unavailable,
    #[error("remote credential store requires explicit reset")]
    ResetRequired,
    #[error("remote credential was not found")]
    NotFound,
    #[error("remote credential authority is corrupt")]
    Corruption,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteCredentialAdmissionErrorV1 {
    #[error("remote credential was rejected")]
    Rejected,
    #[error("remote credential is not yet valid")]
    NotYetValid,
    #[error("remote credential is expired")]
    Expired,
    #[error("remote credential is revoked")]
    Revoked,
    #[error("remote credential lacks the endpoint capability")]
    InsufficientCapability,
    #[error("remote credential does not match the typed request")]
    BindingMismatch,
    #[error("remote credential authority is unavailable")]
    Unavailable,
    #[error("remote credential store requires explicit reset")]
    ResetRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteCredentialAuthorityRecordV1 {
    Grant {
        grant: Box<EnrollmentGrantV1>,
        admission: Box<RemoteEnrollmentAdmissionEvidenceV1>,
    },
    Enrollment {
        enrollment: Box<EnrollmentCredentialRecordV1>,
        receipt: Box<RemoteEnrollmentCommitReceiptV1>,
    },
}

impl RemoteCredentialAuthorityRecordV1 {
    fn fingerprint(&self) -> &RemoteCredentialFingerprintV1 {
        match self {
            Self::Grant { grant, .. } => &grant.fingerprint,
            Self::Enrollment { enrollment, .. } => &enrollment.fingerprint,
        }
    }

    fn class(&self) -> RemoteCredentialClassV1 {
        match self {
            Self::Grant { .. } => RemoteCredentialClassV1::EnrollmentGrant,
            Self::Enrollment { .. } => RemoteCredentialClassV1::Enrollment,
        }
    }

    fn validate(&self) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        match self {
            Self::Grant { grant, admission } => {
                grant
                    .validate()
                    .map_err(|_| RemoteCredentialAdmissionErrorV1::Rejected)?;
                admission
                    .validate_for(grant)
                    .map_err(|_| RemoteCredentialAdmissionErrorV1::Rejected)
            }
            Self::Enrollment {
                enrollment,
                receipt,
            } => {
                enrollment
                    .validate()
                    .map_err(|_| RemoteCredentialAdmissionErrorV1::Rejected)?;
                receipt
                    .validate()
                    .map_err(|_| RemoteCredentialAdmissionErrorV1::Rejected)?;
                if receipt.enrollment.enrollment_id != enrollment.enrollment_id
                    || receipt.enrollment.brain_id != enrollment.brain_id
                    || receipt.enrollment.node_id != enrollment.node_id
                    || receipt.enrollment.scope != enrollment.scope
                    || receipt.enrollment.capabilities != enrollment.capabilities
                    || receipt.enrollment.revision > enrollment.revision
                {
                    return Err(RemoteCredentialAdmissionErrorV1::Rejected);
                }
                Ok(())
            }
        }
    }

    fn state_at(&self, observed_at: UtcMicros) -> EnrollmentCredentialStateV1 {
        match self {
            Self::Grant { grant, .. } => grant.state_at(observed_at),
            Self::Enrollment { enrollment, .. } => enrollment.state_at(observed_at),
        }
    }

    fn brain_id(&self) -> &BrainId {
        match self {
            Self::Grant { grant, .. } => &grant.brain_id,
            Self::Enrollment { enrollment, .. } => &enrollment.brain_id,
        }
    }

    fn node_id(&self) -> &BrainNodeId {
        match self {
            Self::Grant { grant, .. } => &grant.node_id,
            Self::Enrollment { enrollment, .. } => &enrollment.node_id,
        }
    }

    fn revision(&self) -> u64 {
        match self {
            Self::Grant { grant, .. } => grant.revision,
            Self::Enrollment { enrollment, .. } => enrollment.revision,
        }
    }

    fn capabilities(&self) -> &std::collections::BTreeSet<RemoteCapabilityV1> {
        match self {
            Self::Grant { grant, .. } => &grant.capabilities,
            Self::Enrollment { enrollment, .. } => &enrollment.capabilities,
        }
    }

    fn scope(&self) -> &RemoteRepositoryScopeV1 {
        match self {
            Self::Grant { grant, .. } => &grant.scope,
            Self::Enrollment { enrollment, .. } => &enrollment.scope,
        }
    }
}

/// Durable fingerprint lookup. Implementations must search the exact final
/// credential authority and never open a path supplied by the remote caller.
pub trait RemoteCredentialLookupPortV1: Send + Sync {
    fn credential_by_fingerprint(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1>;
}

/// Secret-free proof that one credential was current for one endpoint use.
///
/// This type intentionally implements neither `Serialize` nor `Deserialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAuthenticatedSessionV1 {
    use_case: RemoteCredentialUseV1,
    fingerprint: RemoteCredentialFingerprintV1,
    record: RemoteCredentialAuthorityRecordV1,
    admitted_at: UtcMicros,
}

impl RemoteAuthenticatedSessionV1 {
    pub const fn use_case(&self) -> RemoteCredentialUseV1 {
        self.use_case
    }

    pub fn brain_id(&self) -> &BrainId {
        self.record.brain_id()
    }

    pub fn node_id(&self) -> &BrainNodeId {
        self.record.node_id()
    }

    pub fn scope(&self) -> &RemoteRepositoryScopeV1 {
        self.record.scope()
    }

    pub const fn admitted_at(&self) -> UtcMicros {
        self.admitted_at
    }

    /// Returns the durable, secret-free enrollment proof that authorized this
    /// request. Grant credentials can never reach recovery operations.
    pub fn enrollment_commit_receipt(&self) -> Option<&RemoteEnrollmentCommitReceiptV1> {
        match &self.record {
            RemoteCredentialAuthorityRecordV1::Enrollment { receipt, .. } => Some(receipt),
            RemoteCredentialAuthorityRecordV1::Grant { .. } => None,
        }
    }

    pub fn enrollment_expires_at(&self) -> Option<UtcMicros> {
        match &self.record {
            RemoteCredentialAuthorityRecordV1::Enrollment { enrollment, .. } => {
                Some(enrollment.expires_at)
            }
            RemoteCredentialAuthorityRecordV1::Grant { .. } => None,
        }
    }

    /// Binds post-deserialization protocol metadata to the identity admitted
    /// before the body was read.
    pub fn bind_protocol<T>(
        &self,
        request: &RemoteProtocolRequestV1<T>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        request
            .validate_metadata()
            .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)?;
        if self.record.class() != RemoteCredentialClassV1::Enrollment
            || &request.brain_id != self.record.brain_id()
            || &request.caller_node_id != self.record.node_id()
            || request.enrollment_revision != self.record.revision()
        {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }

    /// Binds the one-time grant session to an exact initial-enrollment body.
    pub fn bind_initial_enrollment(
        &self,
        request: &RemoteProtocolRequestV1<EnrollmentRequestV1>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        request
            .validate_initial_enrollment_metadata()
            .and_then(|()| request.body.validate(request.sent_at))
            .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)?;
        let RemoteCredentialAuthorityRecordV1::Grant { grant, .. } = &self.record else {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        };
        if self.use_case != RemoteCredentialUseV1::InitialEnrollment
            || request.brain_id != grant.brain_id
            || request.caller_node_id != grant.node_id
            || request.body.grant_id != grant.grant_id
            || request.body.grant_revision != grant.revision
            || request.body.brain_id != grant.brain_id
            || request.body.node_id != grant.node_id
            || request.body.scope != grant.scope
            || request.body.expires_at > grant.expires_at
            || !request.body.capabilities.is_subset(&grant.capabilities)
        {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }

    pub fn bind_scope(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        if self.record.scope() != scope {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }

    pub fn bind_backup(
        &self,
        request: &RemoteProtocolRequestV1<BackupRequestV1>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        self.bind_protocol(request)?;
        request
            .body
            .validate(request.sent_at.0)
            .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)?;
        if self.use_case != RemoteCredentialUseV1::CreateBackup
            || request.body.expected.brain_id != self.record.brain_id().as_str()
            || !request
                .expected_authority
                .as_ref()
                .is_some_and(|writer| request.body.expected.matches_writer(writer))
        {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }

    pub fn bind_restore_publication(
        &self,
        request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        self.bind_protocol(request)?;
        request
            .body
            .validate(request.sent_at.0)
            .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)?;
        if self.use_case != RemoteCredentialUseV1::PublishRestore
            || !request.expected_authority.as_ref().is_some_and(|writer| {
                writer.brain_id == *self.record.brain_id()
                    && writer.authority_epoch.0 == request.body.expected_authority_epoch
                    && writer.placement_revision.get() == request.body.expected_placement_revision
            })
        {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }

    pub fn bind_promotion(
        &self,
        request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        self.bind_protocol(request)?;
        request
            .body
            .validate(request.sent_at.0)
            .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)?;
        if self.use_case != RemoteCredentialUseV1::Promote
            || !request.expected_authority.as_ref().is_some_and(|writer| {
                writer.brain_id == *self.record.brain_id()
                    && writer.authority_epoch.0 == request.body.expected_authority_epoch
                    && writer.placement_revision.get() == request.body.expected_placement_revision
            })
        {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }
}

/// Application-owned binding between a route's typed body and the credential
/// session admitted before any body bytes were read.
///
/// The HTTP adapter selects the concrete request type from the matched route.
/// Caller-controlled JSON cannot select either the credential use or whether
/// current credential state must be re-read before execution.
pub trait RemoteSessionBoundProtocolBodyV1: RemoteProtocolBodyV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1;
    const REAUTHORIZE_BEFORE_EXECUTION: bool = false;

    fn execution_expires_at(&self) -> Option<UtcMicros> {
        None
    }

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1>
    where
        Self: Sized;
}

impl RemoteSessionBoundProtocolBodyV1 for EnrollmentRequestV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::InitialEnrollment;

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        session.bind_initial_enrollment(request)
    }
}

impl RemoteSessionBoundProtocolBodyV1 for RemoteCaptureRequestV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::CaptureOffline;

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        bind_protocol_body(session, request, Self::CREDENTIAL_USE)?;
        session.bind_scope(&request.body.writer.scope)
    }
}

impl RemoteSessionBoundProtocolBodyV1 for RemoteReplayRequestV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::Replay;

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        bind_protocol_body(session, request, Self::CREDENTIAL_USE)
    }
}

impl RemoteSessionBoundProtocolBodyV1 for RemoteQueryRequestV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::Query;

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        bind_protocol_body(session, request, Self::CREDENTIAL_USE)?;
        session.bind_scope(&request.body.scope)?;
        if request.expected_authority.as_ref() != Some(&request.body.expected_authority) {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        Ok(())
    }
}

impl RemoteSessionBoundProtocolBodyV1 for BackupRequestV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::CreateBackup;

    fn execution_expires_at(&self) -> Option<UtcMicros> {
        Some(UtcMicros(self.expires_at_micros))
    }

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        session.bind_backup(request)
    }
}

impl RemoteSessionBoundProtocolBodyV1 for StagedRestoreConfirmationV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::PublishRestore;
    const REAUTHORIZE_BEFORE_EXECUTION: bool = true;

    fn execution_expires_at(&self) -> Option<UtcMicros> {
        Some(UtcMicros(self.expires_at_micros))
    }

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        session.bind_restore_publication(request)
    }
}

impl RemoteSessionBoundProtocolBodyV1 for PromotionConfirmationV1 {
    const CREDENTIAL_USE: RemoteCredentialUseV1 = RemoteCredentialUseV1::Promote;
    const REAUTHORIZE_BEFORE_EXECUTION: bool = true;

    fn execution_expires_at(&self) -> Option<UtcMicros> {
        Some(UtcMicros(self.expires_at_micros))
    }

    fn bind_authenticated_session(
        session: &RemoteAuthenticatedSessionV1,
        request: &RemoteProtocolRequestV1<Self>,
    ) -> Result<(), RemoteCredentialAdmissionErrorV1> {
        session.bind_promotion(request)
    }
}

fn bind_protocol_body<Request>(
    session: &RemoteAuthenticatedSessionV1,
    request: &RemoteProtocolRequestV1<Request>,
    use_case: RemoteCredentialUseV1,
) -> Result<(), RemoteCredentialAdmissionErrorV1>
where
    Request: RemoteProtocolBodyV1,
{
    if session.use_case() != use_case {
        return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
    }
    session.bind_protocol(request)?;
    request
        .body
        .validate_remote_protocol_body(request.sent_at)
        .map_err(|_| RemoteCredentialAdmissionErrorV1::BindingMismatch)
}

pub trait RemoteCredentialAdmissionPortV1: Send + Sync {
    fn admit_before_body(
        &self,
        presented: &OpaqueRemoteCredential,
        use_case: RemoteCredentialUseV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1>;

    /// Re-reads current credential state immediately before a durable recovery
    /// publication or promotion. A session admitted before a revocation can
    /// therefore never publish afterward.
    fn reauthorize_publication(
        &self,
        session: &RemoteAuthenticatedSessionV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1>;
}

pub struct RemoteCredentialAdmissionServiceV1<S> {
    store: S,
}

impl<S> RemoteCredentialAdmissionServiceV1<S> {
    pub const fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> RemoteCredentialAdmissionServiceV1<S>
where
    S: RemoteCredentialLookupPortV1,
{
    fn admit_fingerprint(
        &self,
        fingerprint: RemoteCredentialFingerprintV1,
        use_case: RemoteCredentialUseV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1> {
        let record = self
            .store
            .credential_by_fingerprint(use_case.credential_class(), &fingerprint)
            .map_err(map_lookup_error)?;
        record.validate()?;
        if record.fingerprint() != &fingerprint {
            return Err(RemoteCredentialAdmissionErrorV1::Rejected);
        }
        validate_state(&record, observed_at)?;
        if use_case
            .required_capability()
            .is_some_and(|capability| !record.capabilities().contains(&capability))
        {
            return Err(RemoteCredentialAdmissionErrorV1::InsufficientCapability);
        }
        Ok(RemoteAuthenticatedSessionV1 {
            use_case,
            fingerprint,
            record,
            admitted_at: observed_at,
        })
    }
}

impl<S> RemoteCredentialAdmissionPortV1 for RemoteCredentialAdmissionServiceV1<S>
where
    S: RemoteCredentialLookupPortV1,
{
    fn admit_before_body(
        &self,
        presented: &OpaqueRemoteCredential,
        use_case: RemoteCredentialUseV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1> {
        let fingerprint =
            RemoteCredentialFingerprintV1::from_secret(presented.expose_for_authentication())
                .map_err(|_| RemoteCredentialAdmissionErrorV1::Rejected)?;
        self.admit_fingerprint(fingerprint, use_case, observed_at)
    }

    fn reauthorize_publication(
        &self,
        session: &RemoteAuthenticatedSessionV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1> {
        if !matches!(
            session.use_case,
            RemoteCredentialUseV1::PublishRestore | RemoteCredentialUseV1::Promote
        ) {
            return Err(RemoteCredentialAdmissionErrorV1::BindingMismatch);
        }
        let current =
            self.admit_fingerprint(session.fingerprint.clone(), session.use_case, observed_at)?;
        if current.record != session.record {
            return Err(match current.record.state_at(observed_at) {
                EnrollmentCredentialStateV1::Revoked => RemoteCredentialAdmissionErrorV1::Revoked,
                EnrollmentCredentialStateV1::Expired => RemoteCredentialAdmissionErrorV1::Expired,
                _ => RemoteCredentialAdmissionErrorV1::BindingMismatch,
            });
        }
        Ok(current)
    }
}

fn validate_state(
    record: &RemoteCredentialAuthorityRecordV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteCredentialAdmissionErrorV1> {
    match record.state_at(observed_at) {
        EnrollmentCredentialStateV1::Active => Ok(()),
        EnrollmentCredentialStateV1::NotYetValid => {
            Err(RemoteCredentialAdmissionErrorV1::NotYetValid)
        }
        EnrollmentCredentialStateV1::Expired => Err(RemoteCredentialAdmissionErrorV1::Expired),
        EnrollmentCredentialStateV1::Revoked => Err(RemoteCredentialAdmissionErrorV1::Revoked),
    }
}

fn map_lookup_error(error: RemoteCredentialLookupErrorV1) -> RemoteCredentialAdmissionErrorV1 {
    match error {
        RemoteCredentialLookupErrorV1::Unavailable => RemoteCredentialAdmissionErrorV1::Unavailable,
        RemoteCredentialLookupErrorV1::ResetRequired => {
            RemoteCredentialAdmissionErrorV1::ResetRequired
        }
        RemoteCredentialLookupErrorV1::NotFound | RemoteCredentialLookupErrorV1::Corruption => {
            RemoteCredentialAdmissionErrorV1::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    use tracedecay_domain::{
        ActorId, AuthorityEpoch, BrainId, BrainNodeId, CanonicalObservationIdV1, ComponentVersion,
        EnrollmentCredentialRecordV1, EnrollmentGrantV1, EntityId, ManifestDigest,
        ProjectionGenerationId, RefId, RemoteCredentialFingerprintV1, RemotePlacementRevisionV1,
        RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, ShardId, WorktreeId,
        canonical_sha256,
    };

    use crate::{
        AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, OperationBudgetUsage,
        PolicyDecisionRef, ResolvedScope,
        remote::{
            composition::ExpectedRemoteShardV1,
            query::{
                REMOTE_QUERY_SCHEMA_REVISION_V1, RemoteQueryOperationV1, RemoteQueryRequestV1,
            },
        },
    };

    use super::*;

    struct FakeStore {
        records: Mutex<BTreeMap<RemoteCredentialFingerprintV1, RemoteCredentialAuthorityRecordV1>>,
    }

    impl RemoteCredentialLookupPortV1 for FakeStore {
        fn credential_by_fingerprint(
            &self,
            class: RemoteCredentialClassV1,
            fingerprint: &RemoteCredentialFingerprintV1,
        ) -> Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1> {
            self.records
                .lock()
                .unwrap()
                .get(fingerprint)
                .filter(|record| record.class() == class)
                .cloned()
                .ok_or(RemoteCredentialLookupErrorV1::NotFound)
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn scope() -> RemoteRepositoryScopeV1 {
        RemoteRepositoryScopeV1 {
            project_id: id("project.remote"),
            repository_id: id::<RepositoryId>("repository.remote"),
            worktree_id: id::<WorktreeId>("worktree.remote"),
            reference: Some(id::<RefId>("refs/heads/main")),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
        }
    }

    fn enrollment(secret: &[u8]) -> EnrollmentCredentialRecordV1 {
        EnrollmentCredentialRecordV1 {
            enrollment_id: id::<EntityId>("enrollment.remote"),
            brain_id: id::<BrainId>("brain.remote"),
            node_id: id::<BrainNodeId>("node.remote"),
            fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
            revision: 4,
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: BTreeSet::from([
                RemoteCapabilityV1::Replay,
                RemoteCapabilityV1::Query,
                RemoteCapabilityV1::PublishRestore,
            ]),
            scope: scope(),
        }
    }

    fn fake_record(secret: &[u8]) -> RemoteCredentialAuthorityRecordV1 {
        let enrollment = enrollment(secret);
        let grant = EnrollmentGrantV1 {
            grant_id: id("grant.remote"),
            brain_id: enrollment.brain_id.clone(),
            node_id: enrollment.node_id.clone(),
            fingerprint: RemoteCredentialFingerprintV1::from_secret(&[3_u8; 32]).unwrap(),
            revision: 1,
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: enrollment.capabilities.clone(),
            scope: enrollment.scope.clone(),
        };
        let resolved_scope = ResolvedScope::new(
            grant.scope.project_id.clone(),
            grant.scope.repository_id.clone(),
            grant.scope.worktree_id.clone(),
            grant.scope.reference.clone(),
        )
        .unwrap();
        let grant_digest = canonical_sha256(&grant).unwrap();
        let admission = RemoteEnrollmentAdmissionEvidenceV1::new(
            &grant,
            resolved_scope.clone(),
            AuthorityReceipt {
                grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
                grant_revision: grant.revision,
                grant_digest: grant_digest.clone(),
                authorized_scope_digest: resolved_scope.scope_digest,
                disclosure: DisclosureClass::Evidence,
                policy: PolicyDecisionRef::new(
                    "policy.remote.enrollment",
                    1,
                    grant_digest.clone(),
                    ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
                )
                .unwrap(),
                revalidated_at: UtcMicros(9),
            },
            ActorId::new("actor.remote").unwrap(),
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
            Deadline::new(UtcMicros(100)).unwrap(),
        )
        .unwrap();
        let receipt = RemoteEnrollmentCommitReceiptV1 {
            admission,
            prior_grant_digest: grant_digest,
            input_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
            committed_state_digest: canonical_sha256(&enrollment).unwrap(),
            consumed_at: enrollment.issued_at,
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: 1,
                elapsed_micros: 0,
            },
            enrollment,
        };
        receipt.validate().unwrap();
        RemoteCredentialAuthorityRecordV1::Enrollment {
            enrollment: Box::new(receipt.enrollment.clone()),
            receipt: Box::new(receipt),
        }
    }

    #[test]
    fn admission_precedes_body_and_typed_metadata_binding_is_exact() {
        let secret = [7_u8; 32];
        let record = fake_record(&secret);
        let fingerprint = record.fingerprint().clone();
        let service = RemoteCredentialAdmissionServiceV1::new(FakeStore {
            records: Mutex::new(BTreeMap::from([(fingerprint, record)])),
        });
        let presented = OpaqueRemoteCredential::new(secret).unwrap();
        let session = service
            .admit_before_body(&presented, RemoteCredentialUseV1::Replay, UtcMicros(20))
            .unwrap();
        let request = RemoteProtocolRequestV1::new(
            crate::RequestId::new("request.remote").unwrap(),
            id("brain.remote"),
            id("node.remote"),
            4,
            None,
            UtcMicros(20),
            super::super::replay::RemoteReplayRequestV1 {
                event_id: format!("remote.event.{}", "a".repeat(64)),
            },
        )
        .unwrap();
        <RemoteReplayRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .unwrap();

        let mut wrong_revision = request.clone();
        wrong_revision.enrollment_revision = 5;
        assert_eq!(
            <RemoteReplayRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
                &session,
                &wrong_revision,
            ),
            Err(RemoteCredentialAdmissionErrorV1::BindingMismatch)
        );

        let wrong_route_session = service
            .admit_before_body(
                &presented,
                RemoteCredentialUseV1::PublishRestore,
                UtcMicros(20),
            )
            .unwrap();
        assert_eq!(
            <RemoteReplayRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
                &wrong_route_session,
                &request,
            ),
            Err(RemoteCredentialAdmissionErrorV1::BindingMismatch)
        );
    }

    #[test]
    fn query_binding_requires_the_admitted_scope_and_authority_identity() {
        let secret = [8_u8; 32];
        let record = fake_record(&secret);
        let fingerprint = record.fingerprint().clone();
        let service = RemoteCredentialAdmissionServiceV1::new(FakeStore {
            records: Mutex::new(BTreeMap::from([(fingerprint, record)])),
        });
        let presented = OpaqueRemoteCredential::new(secret).unwrap();
        let session = service
            .admit_before_body(&presented, RemoteCredentialUseV1::Query, UtcMicros(20))
            .unwrap();
        let expected_authority = RemoteWriterFenceV1 {
            brain_id: id("brain.remote"),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            authority_epoch: AuthorityEpoch(1),
            authority_node_id: id("node.authority"),
        };
        let request = RemoteProtocolRequestV1::new(
            crate::RequestId::new("request.remote.query").unwrap(),
            id("brain.remote"),
            id("node.remote"),
            4,
            Some(expected_authority.clone()),
            UtcMicros(20),
            RemoteQueryRequestV1 {
                schema_revision: REMOTE_QUERY_SCHEMA_REVISION_V1,
                scope: scope(),
                expected_shards: vec![ExpectedRemoteShardV1 {
                    brain_id: "brain.remote".to_owned(),
                    shard_id: "shard.remote".to_owned(),
                    generation_id: "generation.remote".to_owned(),
                }],
                expected_authority,
                operation: RemoteQueryOperationV1::ExactObservation {
                    observation_id: CanonicalObservationIdV1::new(format!(
                        "sha256:{}",
                        "a".repeat(64)
                    ))
                    .unwrap(),
                },
            },
        )
        .unwrap();
        <RemoteQueryRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .unwrap();

        let mut foreign_scope = request.clone();
        foreign_scope.body.scope.snapshot_id =
            RepositoryStateSnapshotId::new("snapshot.foreign").unwrap();
        assert_eq!(
            <RemoteQueryRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
                &session,
                &foreign_scope,
            ),
            Err(RemoteCredentialAdmissionErrorV1::BindingMismatch)
        );

        let mut missing_authority = request;
        missing_authority.expected_authority = None;
        assert_eq!(
            <RemoteQueryRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
                &session,
                &missing_authority,
            ),
            Err(RemoteCredentialAdmissionErrorV1::BindingMismatch)
        );
    }

    #[test]
    fn capability_and_revocation_are_rechecked_before_publication() {
        let secret = [9_u8; 32];
        let record = fake_record(&secret);
        let fingerprint = record.fingerprint().clone();
        let service = RemoteCredentialAdmissionServiceV1::new(FakeStore {
            records: Mutex::new(BTreeMap::from([(fingerprint.clone(), record)])),
        });
        let presented = OpaqueRemoteCredential::new(secret).unwrap();
        assert_eq!(
            service.admit_before_body(&presented, RemoteCredentialUseV1::Promote, UtcMicros(20)),
            Err(RemoteCredentialAdmissionErrorV1::InsufficientCapability)
        );
        let session = service
            .admit_before_body(
                &presented,
                RemoteCredentialUseV1::PublishRestore,
                UtcMicros(20),
            )
            .unwrap();
        let revoked = match fake_record(&secret) {
            RemoteCredentialAuthorityRecordV1::Enrollment {
                mut enrollment,
                receipt,
            } => {
                enrollment.revoked_at = Some(UtcMicros(21));
                RemoteCredentialAuthorityRecordV1::Enrollment {
                    enrollment,
                    receipt,
                }
            }
            RemoteCredentialAuthorityRecordV1::Grant { .. } => unreachable!(),
        };
        service
            .store
            .records
            .lock()
            .unwrap()
            .insert(fingerprint, revoked);
        assert_eq!(
            service.reauthorize_publication(&session, UtcMicros(21)),
            Err(RemoteCredentialAdmissionErrorV1::Revoked)
        );
    }
}
