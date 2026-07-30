//! Secret-safe remote enrollment and mutual-authentication application logic.

use std::collections::BTreeSet;
use std::fmt;
use std::hint::black_box;

use thiserror::Error;
use tracedecay_domain::{
    BrainId, BrainNodeId, CredentialRevocationReceiptV1, CredentialRotationReceiptV1,
    CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1,
    EnrollmentGrantV1, EntityId, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RemoteRepositoryScopeV1, UtcMicros, validate_remote_secret_length,
};

use super::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};

/// Opaque credential accepted only at an application boundary.
///
/// It is intentionally neither `Clone` nor `Serialize`; debug output is always
/// redacted, and owned bytes are overwritten before release.
pub struct OpaqueRemoteCredential {
    bytes: Box<[u8]>,
}

impl OpaqueRemoteCredential {
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Result<Self, RemoteAuthenticationError> {
        let mut bytes = bytes.into();
        if validate_remote_secret_length(&bytes).is_err() {
            bytes.fill(0);
            black_box(&bytes);
            return Err(RemoteAuthenticationError::InvalidCredential);
        }
        Ok(Self { bytes })
    }

    pub(crate) fn expose_for_authentication(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for OpaqueRemoteCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueRemoteCredential([REDACTED])")
    }
}

impl Drop for OpaqueRemoteCredential {
    fn drop(&mut self) {
        self.bytes.fill(0);
        black_box(&self.bytes);
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteAuthenticationError {
    #[error("remote credential is invalid")]
    InvalidCredential,
    #[error("remote enrollment record is invalid")]
    InvalidEnrollment,
    #[error("remote enrollment is expired")]
    Expired,
    #[error("remote enrollment is revoked")]
    Revoked,
    #[error("remote enrollment identity does not match the request")]
    IdentityMismatch,
    #[error("remote enrollment does not authorize the requested capability")]
    InsufficientCapability,
    #[error("remote enrollment does not authorize the requested repository scope")]
    ScopeMismatch,
    #[error("remote authority authentication failed")]
    AuthorityAuthenticationFailed,
    #[error("remote authority credential is stale or not authorized to serve")]
    InvalidAuthorityCredential,
    #[error("remote credential revision overflowed")]
    RevisionOverflow,
    #[error("remote credential revision is stale")]
    StaleRevision,
    #[error("remote credential validity interval is invalid")]
    InvalidValidity,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteEnrollmentAuthorityErrorV1 {
    #[error("remote enrollment authority is unavailable")]
    Unavailable,
    #[error("remote enrollment grant was not found")]
    GrantNotFound,
    #[error("remote enrollment grant was already consumed")]
    GrantConsumed,
    #[error("remote enrollment identity conflicts with durable state")]
    IdentityConflict,
}

/// Durable grant and enrollment authority. Implementations must atomically
/// consume the exact loaded grant while persisting the issued fingerprint-only
/// enrollment record.
pub trait RemoteEnrollmentAuthorityPortV1: Send + Sync {
    fn load_grant(
        &self,
        grant_id: &EntityId,
    ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1>;

    fn commit_enrollment(
        &self,
        grant: &EnrollmentGrantV1,
        enrollment: &EnrollmentCredentialRecordV1,
        consumed_at: UtcMicros,
    ) -> Result<(), RemoteEnrollmentAuthorityErrorV1>;
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteEnrollmentServiceErrorV1 {
    #[error("remote enrollment request is invalid")]
    InvalidRequest,
    #[error(transparent)]
    Authentication(#[from] RemoteAuthenticationError),
    #[error(transparent)]
    Authority(#[from] RemoteEnrollmentAuthorityErrorV1),
}

pub struct RemoteEnrollmentServiceV1<A> {
    authority: A,
}

impl<A> RemoteEnrollmentServiceV1<A>
where
    A: RemoteEnrollmentAuthorityPortV1,
{
    pub const fn new(authority: A) -> Self {
        Self { authority }
    }

    pub fn enroll(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: &OpaqueRemoteCredential,
        enrollment_credential: &OpaqueRemoteCredential,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentServiceErrorV1> {
        request
            .validate_initial_enrollment_metadata()
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        request
            .body
            .validate(request.sent_at)
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        if request.brain_id != request.body.brain_id
            || request.caller_node_id != request.body.node_id
        {
            return Err(RemoteEnrollmentServiceErrorV1::InvalidRequest);
        }
        let grant = self.authority.load_grant(&request.body.grant_id)?;
        let issue = EnrollmentIssueRequestV1 {
            grant_id: request.body.grant_id,
            grant_revision: request.body.grant_revision,
            enrollment_id: request.body.enrollment_id,
            brain_id: request.body.brain_id,
            node_id: request.body.node_id,
            issued_at: request.sent_at,
            expires_at: request.body.expires_at,
            capabilities: request.body.capabilities,
            scope: request.body.scope,
        };
        let enrollment = issue_enrollment(&grant, grant_credential, issue, enrollment_credential)?;
        self.authority
            .commit_enrollment(&grant, &enrollment, request.sent_at)?;
        Ok(enrollment)
    }
}

/// Metadata used to issue an enrollment. The secret remains a separate opaque
/// argument and never becomes part of this serializable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentIssueRequestV1 {
    pub grant_id: EntityId,
    pub grant_revision: u64,
    pub enrollment_id: EntityId,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub capabilities: BTreeSet<RemoteCapabilityV1>,
    pub scope: RemoteRepositoryScopeV1,
}

pub fn issue_enrollment(
    grant: &EnrollmentGrantV1,
    presented_grant: &OpaqueRemoteCredential,
    request: EnrollmentIssueRequestV1,
    credential: &OpaqueRemoteCredential,
) -> Result<EnrollmentCredentialRecordV1, RemoteAuthenticationError> {
    grant
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    match grant.state_at(request.issued_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::NotYetValid => {
            return Err(RemoteAuthenticationError::InvalidEnrollment);
        }
        EnrollmentCredentialStateV1::Expired => return Err(RemoteAuthenticationError::Expired),
        EnrollmentCredentialStateV1::Revoked => return Err(RemoteAuthenticationError::Revoked),
    }
    if !fingerprints_equal(&grant.fingerprint, &fingerprint(presented_grant)?) {
        return Err(RemoteAuthenticationError::InvalidCredential);
    }
    if request.grant_id != grant.grant_id || request.grant_revision != grant.revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    if request.brain_id != grant.brain_id
        || request.node_id != grant.node_id
        || request.scope != grant.scope
        || request.expires_at > grant.expires_at
    {
        return Err(RemoteAuthenticationError::IdentityMismatch);
    }
    if !request.capabilities.is_subset(&grant.capabilities) {
        return Err(RemoteAuthenticationError::InsufficientCapability);
    }
    let record = EnrollmentCredentialRecordV1 {
        enrollment_id: request.enrollment_id,
        brain_id: request.brain_id,
        node_id: request.node_id,
        fingerprint: fingerprint(credential)?,
        revision: 1,
        issued_at: request.issued_at,
        expires_at: request.expires_at,
        revoked_at: None,
        capabilities: request.capabilities,
        scope: request.scope,
    };
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    Ok(record)
}

/// Authority authentication is delegated to the concrete network boundary.
/// An HTTP/rustls adapter must verify the connected authority peer; the
/// application never accepts a caller-supplied boolean or trust-root claim.
pub trait RemoteAuthorityAuthenticationPort {
    fn authenticate_connected_authority(
        &self,
        expected_authority: &CurrentRemoteAuthorityV1,
        expected_credential: &EnrollmentCredentialRecordV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteAuthenticationError>;
}

/// Authenticate both sides of a remote request and reauthorize exact scope.
#[allow(clippy::too_many_arguments)]
pub fn authenticate_remote_request(
    authority_port: &dyn RemoteAuthorityAuthenticationPort,
    expected_authority: &CurrentRemoteAuthorityV1,
    authority_credential: &EnrollmentCredentialRecordV1,
    caller_credential: &EnrollmentCredentialRecordV1,
    presented_caller_credential: &OpaqueRemoteCredential,
    requested_capability: RemoteCapabilityV1,
    requested_scope: &RemoteRepositoryScopeV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    expected_authority
        .validate()
        .map_err(|_| RemoteAuthenticationError::AuthorityAuthenticationFailed)?;
    validate_authority_credential(expected_authority, authority_credential, observed_at)?;
    authority_port.authenticate_connected_authority(
        expected_authority,
        authority_credential,
        observed_at,
    )?;
    authenticate_caller(
        caller_credential,
        presented_caller_credential,
        &expected_authority.fence.brain_id,
        requested_capability,
        requested_scope,
        observed_at,
    )
}

pub fn authenticate_caller(
    record: &EnrollmentCredentialRecordV1,
    presented: &OpaqueRemoteCredential,
    expected_brain: &BrainId,
    requested_capability: RemoteCapabilityV1,
    requested_scope: &RemoteRepositoryScopeV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    match record.state_at(observed_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::NotYetValid => {
            return Err(RemoteAuthenticationError::InvalidEnrollment);
        }
        EnrollmentCredentialStateV1::Expired => return Err(RemoteAuthenticationError::Expired),
        EnrollmentCredentialStateV1::Revoked => return Err(RemoteAuthenticationError::Revoked),
    }
    if &record.brain_id != expected_brain {
        return Err(RemoteAuthenticationError::IdentityMismatch);
    }
    if !fingerprints_equal(&record.fingerprint, &fingerprint(presented)?) {
        return Err(RemoteAuthenticationError::InvalidCredential);
    }
    if &record.scope != requested_scope {
        return Err(RemoteAuthenticationError::ScopeMismatch);
    }
    if !record.capabilities.contains(&requested_capability) {
        return Err(RemoteAuthenticationError::InsufficientCapability);
    }
    Ok(())
}

fn validate_authority_credential(
    authority: &CurrentRemoteAuthorityV1,
    record: &EnrollmentCredentialRecordV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidAuthorityCredential)?;
    if record.brain_id != authority.fence.brain_id
        || record.node_id != authority.fence.authority_node_id
        || record.revision != authority.credential_revision
        || !record
            .capabilities
            .contains(&RemoteCapabilityV1::ServeAuthority)
        || record.state_at(observed_at) != EnrollmentCredentialStateV1::Active
    {
        return Err(RemoteAuthenticationError::InvalidAuthorityCredential);
    }
    Ok(())
}

pub fn rotate_credential(
    current: &EnrollmentCredentialRecordV1,
    expected_revision: u64,
    presented_current: &OpaqueRemoteCredential,
    replacement: &OpaqueRemoteCredential,
    rotated_at: UtcMicros,
    expires_at: UtcMicros,
) -> Result<(EnrollmentCredentialRecordV1, CredentialRotationReceiptV1), RemoteAuthenticationError>
{
    if current.revision != expected_revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    authenticate_caller(
        current,
        presented_current,
        &current.brain_id,
        RemoteCapabilityV1::RotateCredential,
        &current.scope,
        rotated_at,
    )?;
    if expires_at <= rotated_at {
        return Err(RemoteAuthenticationError::InvalidValidity);
    }
    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or(RemoteAuthenticationError::RevisionOverflow)?;
    let next = EnrollmentCredentialRecordV1 {
        fingerprint: fingerprint(replacement)?,
        revision: next_revision,
        issued_at: rotated_at,
        expires_at,
        revoked_at: None,
        ..current.clone()
    };
    next.validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    let receipt = CredentialRotationReceiptV1 {
        enrollment_id: next.enrollment_id.clone(),
        node_id: next.node_id.clone(),
        prior_revision: current.revision,
        current_revision: next.revision,
        rotated_at,
        expires_at,
    };
    Ok((next, receipt))
}

/// Revoke a credential after the surrounding authority command has been
/// authenticated and authorized. Revocation is monotone and idempotent at an
/// already-revoked timestamp.
pub fn revoke_credential(
    current: &EnrollmentCredentialRecordV1,
    expected_revision: u64,
    revoked_at: UtcMicros,
) -> Result<(EnrollmentCredentialRecordV1, CredentialRevocationReceiptV1), RemoteAuthenticationError>
{
    current
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    if current.revision != expected_revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    if revoked_at < current.issued_at {
        return Err(RemoteAuthenticationError::InvalidValidity);
    }
    if current.revoked_at == Some(revoked_at) {
        return Ok((
            current.clone(),
            CredentialRevocationReceiptV1 {
                enrollment_id: current.enrollment_id.clone(),
                node_id: current.node_id.clone(),
                prior_revision: current.revision.saturating_sub(1),
                current_revision: current.revision,
                revoked_at,
            },
        ));
    }
    if current.revoked_at.is_some() {
        return Err(RemoteAuthenticationError::Revoked);
    }
    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or(RemoteAuthenticationError::RevisionOverflow)?;
    let next = EnrollmentCredentialRecordV1 {
        revision: next_revision,
        revoked_at: Some(revoked_at),
        ..current.clone()
    };
    let receipt = CredentialRevocationReceiptV1 {
        enrollment_id: next.enrollment_id.clone(),
        node_id: next.node_id.clone(),
        prior_revision: current.revision,
        current_revision: next.revision,
        revoked_at,
    };
    Ok((next, receipt))
}

fn fingerprint(
    credential: &OpaqueRemoteCredential,
) -> Result<RemoteCredentialFingerprintV1, RemoteAuthenticationError> {
    RemoteCredentialFingerprintV1::from_secret(credential.expose_for_authentication())
        .map_err(|_| RemoteAuthenticationError::InvalidCredential)
}

fn fingerprints_equal(
    left: &RemoteCredentialFingerprintV1,
    right: &RemoteCredentialFingerprintV1,
) -> bool {
    let left = left.digest().as_str().as_bytes();
    let right = right.digest().as_str().as_bytes();
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::RequestId;

    use super::*;
    use tracedecay_domain::{
        AuthorityEpoch, ProjectId, ProjectionGenerationId, RefId, RemotePlacementRevisionV1,
        RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, ShardId, WorktreeId,
    };

    fn credential(value: u8) -> OpaqueRemoteCredential {
        OpaqueRemoteCredential::new(vec![value; 32].into_boxed_slice()).unwrap()
    }

    fn scope() -> RemoteRepositoryScopeV1 {
        RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote").unwrap(),
            repository_id: RepositoryId::new("repository.remote").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote").unwrap(),
            reference: Some(RefId::new("refs/heads/main").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("repository.state.remote").unwrap(),
        }
    }

    fn request(capabilities: BTreeSet<RemoteCapabilityV1>) -> EnrollmentIssueRequestV1 {
        EnrollmentIssueRequestV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(100),
            capabilities,
            scope: scope(),
        }
    }

    fn enroll(
        enrollment_credential: &OpaqueRemoteCredential,
        capabilities: BTreeSet<RemoteCapabilityV1>,
    ) -> EnrollmentCredentialRecordV1 {
        let grant_credential = credential(b'g');
        let grant = EnrollmentGrantV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            fingerprint: fingerprint(&grant_credential).unwrap(),
            revision: 1,
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: capabilities.clone(),
            scope: scope(),
        };
        issue_enrollment(
            &grant,
            &grant_credential,
            request(capabilities),
            enrollment_credential,
        )
        .unwrap()
    }

    struct TestEnrollmentAuthority {
        grant: EnrollmentGrantV1,
        committed: Mutex<Option<EnrollmentCredentialRecordV1>>,
    }

    impl RemoteEnrollmentAuthorityPortV1 for TestEnrollmentAuthority {
        fn load_grant(
            &self,
            grant_id: &EntityId,
        ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1> {
            if &self.grant.grant_id != grant_id {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantNotFound);
            }
            if self.committed.lock().unwrap().is_some() {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
            }
            Ok(self.grant.clone())
        }

        fn commit_enrollment(
            &self,
            grant: &EnrollmentGrantV1,
            enrollment: &EnrollmentCredentialRecordV1,
            _consumed_at: UtcMicros,
        ) -> Result<(), RemoteEnrollmentAuthorityErrorV1> {
            if grant != &self.grant {
                return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
            }
            let mut committed = self.committed.lock().unwrap();
            if committed.is_some() {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
            }
            *committed = Some(enrollment.clone());
            Ok(())
        }
    }

    fn enrollment_service(
        grant_credential: &OpaqueRemoteCredential,
    ) -> RemoteEnrollmentServiceV1<TestEnrollmentAuthority> {
        RemoteEnrollmentServiceV1::new(TestEnrollmentAuthority {
            grant: EnrollmentGrantV1 {
                grant_id: EntityId::new("grant.remote").unwrap(),
                brain_id: BrainId::new("brain.remote").unwrap(),
                node_id: BrainNodeId::new("node.remote").unwrap(),
                fingerprint: fingerprint(grant_credential).unwrap(),
                revision: 1,
                issued_at: UtcMicros(1),
                expires_at: UtcMicros(100),
                revoked_at: None,
                capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
                scope: scope(),
            },
            committed: Mutex::new(None),
        })
    }

    fn protocol_enrollment_request(node_id: &str) -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
        let brain_id = BrainId::new("brain.remote").unwrap();
        RemoteProtocolRequestV1::new_initial_enrollment(
            RequestId::new("request.remote.enrollment").unwrap(),
            brain_id.clone(),
            BrainNodeId::new(node_id).unwrap(),
            UtcMicros(10),
            EnrollmentRequestV1 {
                grant_id: EntityId::new("grant.remote").unwrap(),
                grant_revision: 1,
                enrollment_id: EntityId::new("enrollment.remote").unwrap(),
                brain_id,
                node_id: BrainNodeId::new(node_id).unwrap(),
                expires_at: UtcMicros(90),
                capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
                scope: scope(),
            },
        )
        .unwrap()
    }

    #[test]
    fn enrollment_service_consumes_grant_once_and_persists_only_fingerprint() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let service = enrollment_service(&grant_credential);

        let enrollment = service
            .enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            )
            .unwrap();
        assert_eq!(
            enrollment.fingerprint,
            fingerprint(&enrollment_credential).unwrap()
        );
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authority(
                RemoteEnrollmentAuthorityErrorV1::GrantConsumed
            ))
        );
        let persisted = service.authority.committed.lock().unwrap();
        assert_eq!(persisted.as_ref(), Some(&enrollment));
        assert!(!format!("{persisted:?}").contains(&"e".repeat(32)));
    }

    #[test]
    fn enrollment_service_rejects_wrong_secret_and_node_identity() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let service = enrollment_service(&grant_credential);
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.remote"),
                &credential(b'x'),
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::InvalidCredential
            ))
        );
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.other"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::IdentityMismatch
            ))
        );
    }

    #[test]
    fn enrollment_service_rejects_expired_and_revoked_grants() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let mut expired = enrollment_service(&grant_credential);
        expired.authority.grant.expires_at = UtcMicros(10);
        assert_eq!(
            expired.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::Expired
            ))
        );

        let mut revoked = enrollment_service(&grant_credential);
        revoked.authority.grant.revoked_at = Some(UtcMicros(5));
        assert_eq!(
            revoked.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::Revoked
            ))
        );
    }

    #[test]
    fn opaque_credential_debug_is_redacted() {
        let secret = credential(b'x');
        assert_eq!(format!("{secret:?}"), "OpaqueRemoteCredential([REDACTED])");
    }

    #[test]
    fn wrong_expired_and_revoked_credentials_fail_closed() {
        let secret = credential(b'a');
        let mut record = enroll(&secret, BTreeSet::from([RemoteCapabilityV1::Query]));

        assert_eq!(
            authenticate_caller(
                &record,
                &credential(b'b'),
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::InvalidCredential)
        );
        assert_eq!(
            authenticate_caller(
                &record,
                &secret,
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(100),
            ),
            Err(RemoteAuthenticationError::Expired)
        );
        record.revoked_at = Some(UtcMicros(40));
        assert_eq!(
            authenticate_caller(
                &record,
                &secret,
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::Revoked)
        );
    }

    #[test]
    fn rotation_invalidates_old_secret_and_advances_revision() {
        let current_secret = credential(b'a');
        let replacement = credential(b'b');
        let record = enroll(
            &current_secret,
            BTreeSet::from([
                RemoteCapabilityV1::DiscoverAuthority,
                RemoteCapabilityV1::RotateCredential,
                RemoteCapabilityV1::Query,
            ]),
        );
        let (rotated, receipt) = rotate_credential(
            &record,
            1,
            &current_secret,
            &replacement,
            UtcMicros(20),
            UtcMicros(200),
        )
        .unwrap();

        assert_eq!(receipt.prior_revision, 1);
        assert_eq!(receipt.current_revision, 2);
        assert_eq!(
            authenticate_caller(
                &rotated,
                &current_secret,
                &rotated.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(30),
            ),
            Err(RemoteAuthenticationError::InvalidCredential)
        );
        authenticate_caller(
            &rotated,
            &replacement,
            &rotated.brain_id,
            RemoteCapabilityV1::Query,
            &scope(),
            UtcMicros(30),
        )
        .unwrap();
    }

    struct RejectAuthority;

    impl RemoteAuthorityAuthenticationPort for RejectAuthority {
        fn authenticate_connected_authority(
            &self,
            _expected_authority: &CurrentRemoteAuthorityV1,
            _expected_credential: &EnrollmentCredentialRecordV1,
            _observed_at: UtcMicros,
        ) -> Result<(), RemoteAuthenticationError> {
            Err(RemoteAuthenticationError::AuthorityAuthenticationFailed)
        }
    }

    #[test]
    fn authority_authentication_cannot_be_skipped() {
        let secret = credential(b'a');
        let authority = enroll(
            &secret,
            BTreeSet::from([
                RemoteCapabilityV1::DiscoverAuthority,
                RemoteCapabilityV1::RotateCredential,
                RemoteCapabilityV1::Query,
                RemoteCapabilityV1::ServeAuthority,
            ]),
        );
        let fence = RemoteWriterFenceV1 {
            brain_id: authority.brain_id.clone(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            authority_epoch: AuthorityEpoch(1),
            authority_node_id: authority.node_id.clone(),
        };
        let current_authority = CurrentRemoteAuthorityV1 {
            fence,
            credential_revision: authority.revision,
            observed_at: UtcMicros(50),
        };
        assert_eq!(
            authenticate_remote_request(
                &RejectAuthority,
                &current_authority,
                &authority,
                &authority,
                &secret,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::AuthorityAuthenticationFailed)
        );
    }
}
