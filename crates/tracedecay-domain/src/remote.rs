//! Remote Brain identity, enrollment, authority, and availability contracts.
//!
//! These values contain no transport locations, storage paths, or plaintext
//! credentials. Network adapters authenticate peers and then present these
//! exact, validated identities to the application layer.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AuthorityEpoch, BrainId, BrainNodeId, DomainError, EntityId, EntityVersionId, ManifestDigest,
    ProjectionGenerationId, RefId, RepositoryId, RepositoryStateSnapshotId, ShardId, UtcMicros,
    WorktreeId, canonical_sha256,
};

const CREDENTIAL_FINGERPRINT_DOMAIN: &str = "tracedecay.remote-credential-fingerprint.v1";
pub const MIN_REMOTE_CREDENTIAL_BYTES: usize = 32;
pub const MAX_REMOTE_CREDENTIAL_BYTES: usize = 4_096;

/// Exact Git scope attached to an enrollment.
///
/// Paths, hostnames, directory names, URLs, and mutable CWD state are
/// intentionally absent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRepositoryScopeV1 {
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub reference: Option<RefId>,
    pub snapshot_id: RepositoryStateSnapshotId,
}

impl RemoteRepositoryScopeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        if let Some(reference) = &self.reference {
            reference.validate()?;
        }
        self.snapshot_id.validate()
    }
}

/// The complete single-writer identity for one mutable shard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWriterFenceV1 {
    pub brain_id: BrainId,
    pub shard_id: ShardId,
    pub generation_id: ProjectionGenerationId,
    /// Canonical placement revision; this reuses the domain's entity-version
    /// identity rather than introducing a second revision namespace.
    pub placement_revision: EntityVersionId,
    pub authority_epoch: AuthorityEpoch,
    pub authority_node_id: BrainNodeId,
}

impl RemoteWriterFenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.brain_id.validate()?;
        self.shard_id.validate()?;
        self.generation_id.validate()?;
        self.placement_revision.validate()?;
        self.authority_node_id.validate()?;
        if self.authority_epoch.0 == 0 {
            return Err(DomainError::NonCanonical {
                field: "remote authority epoch",
            });
        }
        Ok(())
    }

    pub fn same_mutable_shard(&self, other: &Self) -> bool {
        self.brain_id == other.brain_id
            && self.shard_id == other.shard_id
            && self.generation_id == other.generation_id
            && self.placement_revision == other.placement_revision
    }

    pub fn fences(&self, older: &Self) -> bool {
        self.same_mutable_shard(older) && self.authority_epoch > older.authority_epoch
    }
}

/// Remote operation capability retained with an enrollment credential.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCapabilityV1 {
    DiscoverAuthority,
    RotateCredential,
    CaptureOffline,
    Replay,
    Query,
    RefreshReplica,
    ReadBackup,
    CreateBackup,
    StageRestore,
    PublishRestore,
    Promote,
    RevokeEnrollment,
    ServeAuthority,
}

/// One-way, domain-separated fingerprint of a high-entropy opaque credential.
///
/// This value is safe to retain and serialize. The credential bytes are not.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RemoteCredentialFingerprintV1(ManifestDigest);

impl RemoteCredentialFingerprintV1 {
    pub fn from_secret(secret: &[u8]) -> Result<Self, DomainError> {
        validate_remote_secret_length(secret)?;
        Ok(Self(canonical_sha256(&(
            CREDENTIAL_FINGERPRINT_DOMAIN,
            secret,
        ))?))
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

pub fn validate_remote_secret_length(secret: &[u8]) -> Result<(), DomainError> {
    if !(MIN_REMOTE_CREDENTIAL_BYTES..=MAX_REMOTE_CREDENTIAL_BYTES).contains(&secret.len()) {
        return Err(DomainError::NonCanonical {
            field: "remote credential length",
        });
    }
    Ok(())
}

/// Retained enrollment record. It contains only a one-way credential
/// fingerprint and authorization metadata, never the plaintext credential.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentCredentialRecordV1 {
    pub enrollment_id: EntityId,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub fingerprint: RemoteCredentialFingerprintV1,
    pub revision: u64,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub revoked_at: Option<UtcMicros>,
    pub capabilities: BTreeSet<RemoteCapabilityV1>,
    pub scope: RemoteRepositoryScopeV1,
}

/// Revocable, expiring authority-issued permission to enroll one exact node.
/// The one-time secret is retained only as a one-way fingerprint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentGrantV1 {
    pub grant_id: EntityId,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub fingerprint: RemoteCredentialFingerprintV1,
    pub revision: u64,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub revoked_at: Option<UtcMicros>,
    pub capabilities: BTreeSet<RemoteCapabilityV1>,
    pub scope: RemoteRepositoryScopeV1,
}

#[derive(Clone, Copy)]
enum CredentialRecordKind {
    Grant,
    Enrollment,
}

impl CredentialRecordKind {
    const fn revision_field(self) -> &'static str {
        match self {
            Self::Grant => "enrollment grant revision",
            Self::Enrollment => "enrollment credential revision",
        }
    }

    const fn validity_field(self) -> &'static str {
        match self {
            Self::Grant => "enrollment grant validity",
            Self::Enrollment => "enrollment credential validity",
        }
    }

    const fn revocation_field(self) -> &'static str {
        match self {
            Self::Grant => "enrollment grant revocation time",
            Self::Enrollment => "enrollment credential revocation time",
        }
    }

    const fn capabilities_field(self) -> &'static str {
        match self {
            Self::Grant => "enrollment grant capabilities",
            Self::Enrollment => "enrollment capabilities",
        }
    }
}

struct CredentialValidity<'a> {
    fingerprint: &'a RemoteCredentialFingerprintV1,
    revision: u64,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    revoked_at: Option<UtcMicros>,
    capabilities: &'a BTreeSet<RemoteCapabilityV1>,
    scope: &'a RemoteRepositoryScopeV1,
}

impl CredentialValidity<'_> {
    fn validate(&self, kind: CredentialRecordKind) -> Result<(), DomainError> {
        self.fingerprint.validate()?;
        self.scope.validate()?;
        if self.revision == 0 {
            return Err(DomainError::NonCanonical {
                field: kind.revision_field(),
            });
        }
        if self.expires_at <= self.issued_at {
            return Err(DomainError::NonCanonical {
                field: kind.validity_field(),
            });
        }
        if self
            .revoked_at
            .is_some_and(|revoked_at| revoked_at < self.issued_at)
        {
            return Err(DomainError::NonCanonical {
                field: kind.revocation_field(),
            });
        }
        if self.capabilities.is_empty() {
            return Err(DomainError::Empty {
                field: kind.capabilities_field(),
            });
        }
        Ok(())
    }

    fn state_at(&self, observed_at: UtcMicros) -> EnrollmentCredentialStateV1 {
        if self
            .revoked_at
            .is_some_and(|revoked_at| observed_at >= revoked_at)
        {
            EnrollmentCredentialStateV1::Revoked
        } else if observed_at >= self.expires_at {
            EnrollmentCredentialStateV1::Expired
        } else {
            EnrollmentCredentialStateV1::Active
        }
    }
}

impl EnrollmentGrantV1 {
    fn validity(&self) -> CredentialValidity<'_> {
        CredentialValidity {
            fingerprint: &self.fingerprint,
            revision: self.revision,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            revoked_at: self.revoked_at,
            capabilities: &self.capabilities,
            scope: &self.scope,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.grant_id.validate()?;
        self.brain_id.validate()?;
        self.node_id.validate()?;
        self.validity().validate(CredentialRecordKind::Grant)
    }

    pub fn state_at(&self, observed_at: UtcMicros) -> EnrollmentCredentialStateV1 {
        self.validity().state_at(observed_at)
    }
}

impl EnrollmentCredentialRecordV1 {
    fn validity(&self) -> CredentialValidity<'_> {
        CredentialValidity {
            fingerprint: &self.fingerprint,
            revision: self.revision,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            revoked_at: self.revoked_at,
            capabilities: &self.capabilities,
            scope: &self.scope,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.enrollment_id.validate()?;
        self.brain_id.validate()?;
        self.node_id.validate()?;
        self.validity().validate(CredentialRecordKind::Enrollment)
    }

    pub fn state_at(&self, observed_at: UtcMicros) -> EnrollmentCredentialStateV1 {
        self.validity().state_at(observed_at)
    }

    pub fn permits(
        &self,
        capability: RemoteCapabilityV1,
        scope: &RemoteRepositoryScopeV1,
        observed_at: UtcMicros,
    ) -> bool {
        self.state_at(observed_at) == EnrollmentCredentialStateV1::Active
            && self.capabilities.contains(&capability)
            && &self.scope == scope
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentCredentialStateV1 {
    Active,
    Expired,
    Revoked,
}

/// Non-secret durable result of credential rotation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRotationReceiptV1 {
    pub enrollment_id: EntityId,
    pub node_id: BrainNodeId,
    pub prior_revision: u64,
    pub current_revision: u64,
    pub rotated_at: UtcMicros,
    pub expires_at: UtcMicros,
}

/// Non-secret durable result of credential revocation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRevocationReceiptV1 {
    pub enrollment_id: EntityId,
    pub node_id: BrainNodeId,
    pub prior_revision: u64,
    pub current_revision: u64,
    pub revoked_at: UtcMicros,
}

/// Authenticated current authority for a mutable shard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentRemoteAuthorityV1 {
    pub fence: RemoteWriterFenceV1,
    /// Revision of the authority node's current enrollment credential.
    pub credential_revision: u64,
    pub observed_at: UtcMicros,
}

impl CurrentRemoteAuthorityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.fence.validate()?;
        if self.credential_revision == 0 {
            return Err(DomainError::NonCanonical {
                field: "authority credential revision",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuthorityUnavailableReasonV1 {
    RegistryUnavailable,
    PlacementUnknown,
    AuthorityUnreachable,
    AuthorityAuthenticationFailed,
    CallerAuthenticationFailed,
    EnrollmentExpired,
    EnrollmentRevoked,
    InsufficientCapability,
    ScopeMismatch,
    FenceUnverified,
    ProtocolIncompatible,
}

/// Truthful authority lookup state. Missing evidence is never represented as
/// an available authority or a successful empty response.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum CurrentRemoteAuthorityStateV1 {
    Available(CurrentRemoteAuthorityV1),
    Partial {
        known_fence: Option<RemoteWriterFenceV1>,
        missing: BTreeSet<RemoteAuthorityUnavailableReasonV1>,
        observed_at: UtcMicros,
    },
    Unavailable {
        reason: RemoteAuthorityUnavailableReasonV1,
        observed_at: UtcMicros,
    },
}

impl CurrentRemoteAuthorityStateV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Available(authority) => authority.validate(),
            Self::Partial {
                known_fence,
                missing,
                ..
            } => {
                if missing.is_empty() {
                    return Err(DomainError::Empty {
                        field: "partial authority evidence",
                    });
                }
                if let Some(fence) = known_fence {
                    fence.validate()?;
                }
                Ok(())
            }
            Self::Unavailable { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(make: impl FnOnce(String) -> Result<T, DomainError>, value: &str) -> T {
        make(value.to_owned()).unwrap()
    }

    fn scope() -> RemoteRepositoryScopeV1 {
        RemoteRepositoryScopeV1 {
            repository_id: id(RepositoryId::new, "repository.remote"),
            worktree_id: id(WorktreeId::new, "worktree.remote"),
            reference: Some(id(RefId::new, "refs/heads/main")),
            snapshot_id: id(RepositoryStateSnapshotId::new, "repository.state.remote"),
        }
    }

    fn record(secret: &[u8]) -> EnrollmentCredentialRecordV1 {
        EnrollmentCredentialRecordV1 {
            enrollment_id: id(EntityId::new, "enrollment.remote"),
            brain_id: id(BrainId::new, "brain.remote"),
            node_id: id(BrainNodeId::new, "node.remote"),
            fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
            revision: 1,
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: scope(),
        }
    }

    fn grant(secret: &[u8]) -> EnrollmentGrantV1 {
        EnrollmentGrantV1 {
            grant_id: id(EntityId::new, "grant.remote"),
            brain_id: id(BrainId::new, "brain.remote"),
            node_id: id(BrainNodeId::new, "node.remote"),
            fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
            revision: 1,
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: scope(),
        }
    }

    #[test]
    fn retained_enrollment_serialization_contains_no_plaintext_secret() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let value = serde_json::to_string(&record(secret)).unwrap();
        assert!(!value.contains("0123456789abcdef"));
        assert!(value.contains("sha256:"));
    }

    #[test]
    fn credential_record_and_grant_wire_shapes_are_byte_exact() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let record = record(secret);
        let grant = grant(secret);
        let fingerprint = record.fingerprint.digest().as_str();
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            format!(
                r#"{{"enrollment_id":"enrollment.remote","brain_id":"brain.remote","node_id":"node.remote","fingerprint":"{fingerprint}","revision":1,"issued_at":10,"expires_at":100,"revoked_at":null,"capabilities":["query"],"scope":{{"repository_id":"repository.remote","worktree_id":"worktree.remote","reference":"refs/heads/main","snapshot_id":"repository.state.remote"}}}}"#
            )
        );
        assert_eq!(
            serde_json::to_string(&grant).unwrap(),
            format!(
                r#"{{"grant_id":"grant.remote","brain_id":"brain.remote","node_id":"node.remote","fingerprint":"{fingerprint}","revision":1,"issued_at":10,"expires_at":100,"revoked_at":null,"capabilities":["query"],"scope":{{"repository_id":"repository.remote","worktree_id":"worktree.remote","reference":"refs/heads/main","snapshot_id":"repository.state.remote"}}}}"#
            )
        );
    }

    #[test]
    fn credential_record_and_grant_share_state_transitions() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let mut record = record(secret);
        let mut grant = grant(secret);

        for observed_at in [UtcMicros(9), UtcMicros(10), UtcMicros(99), UtcMicros(100)] {
            assert_eq!(record.state_at(observed_at), grant.state_at(observed_at));
        }

        record.revoked_at = Some(UtcMicros(50));
        grant.revoked_at = Some(UtcMicros(50));
        for observed_at in [UtcMicros(49), UtcMicros(50), UtcMicros(100)] {
            assert_eq!(record.state_at(observed_at), grant.state_at(observed_at));
        }
    }

    #[test]
    fn credential_state_and_scope_fail_closed() {
        let mut credential = record(b"0123456789abcdef0123456789abcdef");
        assert!(credential.permits(RemoteCapabilityV1::Query, &scope(), UtcMicros(99)));
        assert!(!credential.permits(RemoteCapabilityV1::Replay, &scope(), UtcMicros(99)));
        assert!(!credential.permits(RemoteCapabilityV1::Query, &scope(), UtcMicros(100)));
        credential.revoked_at = Some(UtcMicros(50));
        assert_eq!(
            credential.state_at(UtcMicros(50)),
            EnrollmentCredentialStateV1::Revoked
        );
    }

    #[test]
    fn writer_fence_requires_exact_identity_and_higher_epoch() {
        let older = RemoteWriterFenceV1 {
            brain_id: id(BrainId::new, "brain.remote"),
            shard_id: id(ShardId::new, "shard.remote"),
            generation_id: id(ProjectionGenerationId::new, "generation.remote"),
            placement_revision: id(EntityVersionId::new, "placement.remote.1"),
            authority_epoch: AuthorityEpoch(7),
            authority_node_id: id(BrainNodeId::new, "node.old"),
        };
        let mut newer = older.clone();
        newer.authority_epoch = AuthorityEpoch(8);
        newer.authority_node_id = id(BrainNodeId::new, "node.new");
        assert!(newer.fences(&older));

        newer.placement_revision = id(EntityVersionId::new, "placement.remote.2");
        assert!(!newer.fences(&older));
    }

    #[test]
    fn partial_authority_requires_missing_evidence() {
        let state = CurrentRemoteAuthorityStateV1::Partial {
            known_fence: None,
            missing: BTreeSet::new(),
            observed_at: UtcMicros(20),
        };
        assert!(state.validate().is_err());
    }
}
