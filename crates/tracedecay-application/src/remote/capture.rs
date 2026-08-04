//! Transport-neutral admission for remote offline capture.
//!
//! The application owns authorization and command/result identity. Concrete
//! frame encoding, encrypted spooling, state transitions, and runtime bindings
//! remain behind [`RemoteCapturePortV1`].

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    BrainNodeId, CurrentRemoteAuthorityStateV1, CurrentRemoteAuthorityV1, DurableObservationV1,
    EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1, EntityId, ProjectId,
    RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteRepositoryScopeV1, UtcMicros,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureSequenceV1 {
    pub sequence: u64,
    pub previous_event_id: Option<String>,
}

impl RemoteCaptureSequenceV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureApplicationErrorV1> {
        if self.sequence == 0
            || (self.sequence == 1) != self.previous_event_id.is_none()
            || self.previous_event_id.as_ref().is_some_and(|event_id| {
                event_id.len() < 16
                    || event_id.len() > 160
                    || event_id.trim() != event_id
                    || event_id.chars().any(char::is_control)
            })
        {
            return Err(RemoteCaptureApplicationErrorV1::InvalidSequence);
        }
        Ok(())
    }
}

/// Exact authority identity required by capture and replay. The concrete store
/// runtime binding is intentionally absent and remains adapter-owned.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWriterAuthorityV1 {
    pub project_id: ProjectId,
    pub scope: RemoteRepositoryScopeV1,
    pub authority: CurrentRemoteAuthorityV1,
}

impl RemoteWriterAuthorityV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureApplicationErrorV1> {
        self.project_id
            .validate()
            .map_err(|_| RemoteCaptureApplicationErrorV1::WriterFenceMismatch)?;
        self.scope
            .validate()
            .map_err(|_| RemoteCaptureApplicationErrorV1::WriterFenceMismatch)?;
        self.authority
            .validate()
            .map_err(|_| RemoteCaptureApplicationErrorV1::WriterFenceMismatch)
    }
}

#[derive(Clone, Debug)]
pub struct RemoteOfflineCaptureCommandV1 {
    pub enrollment: EnrollmentCredentialRecordV1,
    pub writer: RemoteWriterAuthorityV1,
    pub policy_revision: u64,
    pub sequence: RemoteCaptureSequenceV1,
    pub observation: DurableObservationV1,
    pub captured_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedRemoteCaptureV1 {
    pub enrollment_id: EntityId,
    pub enrollment_revision: u64,
    pub node_id: BrainNodeId,
    pub writer: RemoteWriterAuthorityV1,
    pub policy_revision: u64,
    pub sequence: RemoteCaptureSequenceV1,
    pub observation: DurableObservationV1,
    pub captured_at: UtcMicros,
}

/// Durable capture lifecycle, kept distinct from replay transaction outcomes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCaptureStateV1 {
    Captured,
    Pending,
    Acknowledged,
    GarbageCollectionEligible,
}

impl RemoteCaptureStateV1 {
    pub const fn permits_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Captured, Self::Pending)
                | (Self::Pending, Self::Acknowledged)
                | (Self::Acknowledged, Self::GarbageCollectionEligible)
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCaptureDispositionV1 {
    CapturedPending,
    AlreadyPending,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureReceiptV1 {
    pub event_id: String,
    pub sequence: u64,
    pub disposition: RemoteCaptureDispositionV1,
}

impl RemoteCaptureReceiptV1 {
    pub fn validate_for(
        &self,
        sequence: &RemoteCaptureSequenceV1,
    ) -> Result<(), RemoteCaptureApplicationErrorV1> {
        if self.event_id.len() < 16
            || self.event_id.len() > 160
            || self.event_id.trim() != self.event_id
            || self.event_id.chars().any(char::is_control)
            || self.sequence != sequence.sequence
        {
            return Err(RemoteCaptureApplicationErrorV1::InvalidPortResult);
        }
        Ok(())
    }
}

/// Application-owned boundary implemented by encrypted spool adapters.
///
/// `capture_pending` must atomically preserve an existing identical frame or
/// persist the new canonical frame and advance it to pending. It must not
/// expose a store runtime request or concrete frame DTO to the application.
pub trait RemoteCapturePortV1: Send + Sync {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1>;

    fn capture_pending(
        &self,
        command: &AdmittedRemoteCaptureV1,
    ) -> Result<RemoteCaptureReceiptV1, RemoteCapturePersistenceErrorV1>;
}

pub struct RemoteCaptureServiceV1<P> {
    port: P,
}

impl<P> RemoteCaptureServiceV1<P>
where
    P: RemoteCapturePortV1,
{
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    pub fn capture(
        &self,
        command: RemoteOfflineCaptureCommandV1,
    ) -> Result<RemoteCaptureReceiptV1, RemoteCaptureApplicationErrorV1> {
        let admitted = admit_capture(&self.port, command)?;
        let receipt = self
            .port
            .capture_pending(&admitted)
            .map_err(RemoteCaptureApplicationErrorV1::Persistence)?;
        receipt.validate_for(&admitted.sequence)?;
        Ok(receipt)
    }
}

fn admit_capture(
    port: &dyn RemoteCapturePortV1,
    command: RemoteOfflineCaptureCommandV1,
) -> Result<AdmittedRemoteCaptureV1, RemoteCaptureApplicationErrorV1> {
    command
        .enrollment
        .validate()
        .map_err(|_| RemoteCaptureApplicationErrorV1::InvalidEnrollment)?;
    match command.enrollment.state_at(command.captured_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::NotYetValid => {
            return Err(RemoteCaptureApplicationErrorV1::InvalidEnrollment);
        }
        EnrollmentCredentialStateV1::Expired => {
            return Err(RemoteCaptureApplicationErrorV1::EnrollmentExpired);
        }
        EnrollmentCredentialStateV1::Revoked => {
            return Err(RemoteCaptureApplicationErrorV1::EnrollmentRevoked);
        }
    }
    if !command
        .enrollment
        .capabilities
        .contains(&RemoteCapabilityV1::CaptureOffline)
    {
        return Err(RemoteCaptureApplicationErrorV1::CaptureNotAuthorized);
    }
    command.writer.validate()?;
    command.sequence.validate()?;
    if command.policy_revision == 0
        || command.enrollment.brain_id != command.writer.authority.fence.brain_id
        || command.enrollment.scope != command.writer.scope
    {
        return Err(RemoteCaptureApplicationErrorV1::WriterFenceMismatch);
    }
    let current_authority = port
        .current_writer_authority(&command.writer)
        .map_err(RemoteCaptureApplicationErrorV1::Persistence)?;
    current_authority
        .validate()
        .map_err(|_| RemoteCaptureApplicationErrorV1::AuthorityReachabilityUnknown)?;
    match current_authority {
        CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
            ..
        } => {}
        CurrentRemoteAuthorityStateV1::Available(_) => {
            return Err(RemoteCaptureApplicationErrorV1::AuthorityReachable);
        }
        CurrentRemoteAuthorityStateV1::Partial { .. }
        | CurrentRemoteAuthorityStateV1::Unavailable { .. } => {
            return Err(RemoteCaptureApplicationErrorV1::AuthorityReachabilityUnknown);
        }
    }
    Ok(AdmittedRemoteCaptureV1 {
        enrollment_id: command.enrollment.enrollment_id,
        enrollment_revision: command.enrollment.revision,
        node_id: command.enrollment.node_id,
        writer: command.writer,
        policy_revision: command.policy_revision,
        sequence: command.sequence,
        observation: command.observation,
        captured_at: command.captured_at,
    })
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteCapturePersistenceErrorV1 {
    #[error("remote spool at-rest encryption is unavailable")]
    AtRestEncryptionUnavailable,
    #[error("remote spool is full")]
    Overflow,
    #[error("remote spool is corrupt")]
    Corruption,
    #[error("remote spool has a sequence gap")]
    SequenceGap,
    #[error("remote spool persistence failed")]
    Unavailable,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteCaptureApplicationErrorV1 {
    #[error("remote enrollment is invalid")]
    InvalidEnrollment,
    #[error("remote enrollment is expired")]
    EnrollmentExpired,
    #[error("remote enrollment is revoked")]
    EnrollmentRevoked,
    #[error("remote enrollment does not authorize offline capture")]
    CaptureNotAuthorized,
    #[error("remote capture sequence is invalid")]
    InvalidSequence,
    #[error("current remote writer fence does not match enrollment")]
    WriterFenceMismatch,
    #[error("current remote authority is reachable")]
    AuthorityReachable,
    #[error("current remote authority reachability is unknown")]
    AuthorityReachabilityUnknown,
    #[error("remote capture port returned a detached result")]
    InvalidPortResult,
    #[error(transparent)]
    Persistence(RemoteCapturePersistenceErrorV1),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct FakeCapturePort {
        authority: CurrentRemoteAuthorityStateV1,
        captures: Mutex<Vec<u64>>,
    }

    impl RemoteCapturePortV1 for FakeCapturePort {
        fn current_writer_authority(
            &self,
            _writer: &RemoteWriterAuthorityV1,
        ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
            Ok(self.authority.clone())
        }

        fn capture_pending(
            &self,
            command: &AdmittedRemoteCaptureV1,
        ) -> Result<RemoteCaptureReceiptV1, RemoteCapturePersistenceErrorV1> {
            self.captures
                .lock()
                .expect("capture calls")
                .push(command.sequence.sequence);
            Ok(RemoteCaptureReceiptV1 {
                event_id: "remote.event.0123456789abcdef".to_owned(),
                sequence: command.sequence.sequence,
                disposition: RemoteCaptureDispositionV1::CapturedPending,
            })
        }
    }

    #[test]
    fn unknown_reachability_is_not_treated_as_offline() {
        let authority = CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
            observed_at: UtcMicros(1),
        };
        let port = FakeCapturePort {
            authority: authority.clone(),
            captures: Mutex::new(Vec::new()),
        };
        assert!(!matches!(
            port.authority,
            CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                ..
            }
        ));
        assert!(port.captures.lock().unwrap().is_empty());
    }

    #[test]
    fn detached_capture_receipt_is_rejected() {
        let sequence = RemoteCaptureSequenceV1 {
            sequence: 1,
            previous_event_id: None,
        };
        let receipt = RemoteCaptureReceiptV1 {
            event_id: "remote.event.0123456789abcdef".to_owned(),
            sequence: 2,
            disposition: RemoteCaptureDispositionV1::CapturedPending,
        };
        assert_eq!(
            receipt.validate_for(&sequence),
            Err(RemoteCaptureApplicationErrorV1::InvalidPortResult)
        );
    }

    #[test]
    fn capture_lifecycle_preserves_spool_and_acknowledgement_boundaries() {
        assert!(
            RemoteCaptureStateV1::Captured.permits_transition_to(RemoteCaptureStateV1::Pending)
        );
        assert!(
            RemoteCaptureStateV1::Pending.permits_transition_to(RemoteCaptureStateV1::Acknowledged)
        );
        assert!(
            RemoteCaptureStateV1::Acknowledged
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
        assert!(
            !RemoteCaptureStateV1::Pending
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
    }
}
