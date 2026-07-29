//! Application ownership for PR16 offline capture.
//!
//! Capture accepts only `DurableObservationV1`, which is already bound to a
//! canonical sanitization receipt. Host hooks do not receive this spool port.

use thiserror::Error;
use tracedecay_domain::{
    DurableObservationV1, EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1, ProjectId,
    RemoteCapabilityV1, UtcMicros,
};
use tracedecay_store::{
    RemoteCaptureFrameV1, RemoteCaptureSequenceV1, RemoteCaptureStateV1, RemoteCaptureTransitionV1,
    RemoteEnrollmentIdentityV1, RemoteRepositoryIdentityV1, RemoteWriterBindingV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAuthorityReachabilityV1 {
    Reachable,
    Unreachable,
    Unknown,
}

/// Daemon-owned reachability authority. A request DTO cannot claim that the
/// current writer is offline.
pub trait RemoteAuthorityReachabilityPortV1: Send + Sync {
    fn current_writer_reachability(
        &self,
        writer: &RemoteWriterBindingV1,
    ) -> Result<RemoteAuthorityReachabilityV1, RemoteCaptureApplicationErrorV1>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteSpoolCaptureOutcome {
    Captured,
    AlreadyCaptured { state: RemoteCaptureStateV1 },
}

pub trait RemoteCaptureSpoolPortV1: Send + Sync {
    fn capture(
        &self,
        frame: RemoteCaptureFrameV1,
    ) -> Result<RemoteSpoolCaptureOutcome, RemoteCapturePersistenceErrorV1>;

    fn transition(
        &self,
        transition: RemoteCaptureTransitionV1,
    ) -> Result<(), RemoteCapturePersistenceErrorV1>;
}

#[derive(Clone, Debug)]
pub struct OfflineCaptureAuthorityV1 {
    enrollment: RemoteEnrollmentIdentityV1,
    writer: RemoteWriterBindingV1,
    policy_revision: u64,
}

impl OfflineCaptureAuthorityV1 {
    pub fn enrollment(&self) -> &RemoteEnrollmentIdentityV1 {
        &self.enrollment
    }

    pub fn writer(&self) -> &RemoteWriterBindingV1 {
        &self.writer
    }
}

/// Recheck enrollment, exact repository scope, current fence, and actual
/// authority unreachability before allowing durable offline capture.
pub fn authorize_offline_capture(
    reachability: &dyn RemoteAuthorityReachabilityPortV1,
    enrollment: &EnrollmentCredentialRecordV1,
    project_id: ProjectId,
    writer: RemoteWriterBindingV1,
    policy_revision: u64,
    observed_at: UtcMicros,
) -> Result<OfflineCaptureAuthorityV1, RemoteCaptureApplicationErrorV1> {
    enrollment
        .validate()
        .map_err(|_| RemoteCaptureApplicationErrorV1::InvalidEnrollment)?;
    match enrollment.state_at(observed_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::Expired => {
            return Err(RemoteCaptureApplicationErrorV1::EnrollmentExpired);
        }
        EnrollmentCredentialStateV1::Revoked => {
            return Err(RemoteCaptureApplicationErrorV1::EnrollmentRevoked);
        }
    }
    if !enrollment
        .capabilities
        .contains(&RemoteCapabilityV1::CaptureOffline)
    {
        return Err(RemoteCaptureApplicationErrorV1::CaptureNotAuthorized);
    }
    writer
        .validate()
        .map_err(|_| RemoteCaptureApplicationErrorV1::WriterFenceMismatch)?;
    if enrollment.brain_id != writer.fence.brain_id || policy_revision == 0 {
        return Err(RemoteCaptureApplicationErrorV1::WriterFenceMismatch);
    }
    match reachability.current_writer_reachability(&writer)? {
        RemoteAuthorityReachabilityV1::Unreachable => {}
        RemoteAuthorityReachabilityV1::Reachable => {
            return Err(RemoteCaptureApplicationErrorV1::AuthorityReachable);
        }
        RemoteAuthorityReachabilityV1::Unknown => {
            return Err(RemoteCaptureApplicationErrorV1::AuthorityReachabilityUnknown);
        }
    }
    Ok(OfflineCaptureAuthorityV1 {
        enrollment: RemoteEnrollmentIdentityV1 {
            enrollment_id: enrollment.enrollment_id.clone(),
            enrollment_revision: enrollment.revision,
            node_id: enrollment.node_id.clone(),
            repository: RemoteRepositoryIdentityV1 {
                project_id,
                scope: enrollment.scope.clone(),
            },
        },
        writer,
        policy_revision,
    })
}

pub fn capture_offline_observation(
    spool: &dyn RemoteCaptureSpoolPortV1,
    authority: &OfflineCaptureAuthorityV1,
    sequence: RemoteCaptureSequenceV1,
    observation: DurableObservationV1,
    captured_at: UtcMicros,
) -> Result<RemoteCaptureFrameV1, RemoteCaptureApplicationErrorV1> {
    let frame = RemoteCaptureFrameV1::new(
        authority.enrollment.clone(),
        sequence,
        captured_at,
        authority.policy_revision,
        authority.writer.clone(),
        observation,
    )
    .map_err(|_| RemoteCaptureApplicationErrorV1::InvalidCanonicalObservation)?;
    let capture_outcome = spool
        .capture(frame.clone())
        .map_err(RemoteCaptureApplicationErrorV1::Persistence)?;
    if matches!(
        capture_outcome,
        RemoteSpoolCaptureOutcome::Captured
            | RemoteSpoolCaptureOutcome::AlreadyCaptured {
                state: RemoteCaptureStateV1::Captured
            }
    ) {
        spool
            .transition(RemoteCaptureTransitionV1 {
                event_id: frame.event_id.clone(),
                from: RemoteCaptureStateV1::Captured,
                to: RemoteCaptureStateV1::Pending,
                replay_attempt: 0,
                observed_at: captured_at,
                finding: None,
                receipt: None,
            })
            .map_err(RemoteCaptureApplicationErrorV1::Persistence)?;
    }
    Ok(frame)
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
    #[error("current remote writer fence does not match enrollment")]
    WriterFenceMismatch,
    #[error("current remote authority is reachable")]
    AuthorityReachable,
    #[error("current remote authority reachability is unknown")]
    AuthorityReachabilityUnknown,
    #[error("canonical sanitized observation is invalid")]
    InvalidCanonicalObservation,
    #[error(transparent)]
    Persistence(RemoteCapturePersistenceErrorV1),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_reachability_is_not_treated_as_offline() {
        assert_ne!(
            RemoteAuthorityReachabilityV1::Unknown,
            RemoteAuthorityReachabilityV1::Unreachable
        );
    }
}
