//! Transport-neutral control contract for daemon-owned host-session import.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

use crate::{
    ApplicationContractError, CancellationSignal, Deadline, IdempotencyKey, OperationTermination,
    RequestId,
};

/// Exact project/profile authority bound to one daemon session-sync service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSyncScopeV1 {
    project_id: ProjectId,
    profile_id: UserProfileId,
}

impl SessionSyncScopeV1 {
    pub fn new(project_id: ProjectId, profile_id: UserProfileId) -> Self {
        Self {
            project_id,
            profile_id,
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }
}

/// Imports current and historical transcripts through every native host parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionTranscriptImportV1;

impl SessionTranscriptImportV1 {
    pub const fn all_hosts() -> Self {
        Self
    }
}

/// Bounded session/Git convergence request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionGitSyncV1 {
    since_unix: i64,
    max_sessions: usize,
    dry_run: bool,
}

impl SessionGitSyncV1 {
    pub fn new(
        since_unix: i64,
        max_sessions: usize,
        dry_run: bool,
    ) -> Result<Self, ApplicationContractError> {
        if since_unix < 0 {
            return Err(ApplicationContractError::InvalidRange {
                field: "session git sync lower bound",
            });
        }
        if max_sessions == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "session git sync maximum sessions",
            });
        }
        Ok(Self {
            since_unix,
            max_sessions,
            dry_run,
        })
    }

    pub const fn since_unix(self) -> i64 {
        self.since_unix
    }

    pub const fn max_sessions(self) -> usize {
        self.max_sessions
    }

    pub const fn dry_run(self) -> bool {
        self.dry_run
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSyncCommandV1 {
    ImportTranscripts(SessionTranscriptImportV1),
    SynchronizeGit(SessionGitSyncV1),
}

#[derive(Clone, Debug)]
pub struct SessionSyncRequestV1 {
    operation_id: RequestId,
    idempotency_key: IdempotencyKey,
    scope: SessionSyncScopeV1,
    deadline: Deadline,
    cancellation: CancellationSignal,
    command: SessionSyncCommandV1,
}

impl SessionSyncRequestV1 {
    pub fn new(
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
        scope: SessionSyncScopeV1,
        deadline: Deadline,
        cancellation: CancellationSignal,
        command: SessionSyncCommandV1,
    ) -> Self {
        Self {
            operation_id,
            idempotency_key,
            scope,
            deadline,
            cancellation,
            command,
        }
    }

    pub fn operation_id(&self) -> &RequestId {
        &self.operation_id
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub fn scope(&self) -> &SessionSyncScopeV1 {
        &self.scope
    }

    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub fn cancellation(&self) -> &CancellationSignal {
        &self.cancellation
    }

    pub const fn command(&self) -> SessionSyncCommandV1 {
        self.command
    }

    pub fn admit_at(&self, observed_at: UtcMicros) -> Result<(), SessionSyncAdmissionErrorV1> {
        if self.cancellation.is_cancelled() {
            return Err(SessionSyncAdmissionErrorV1::Cancelled);
        }
        if self.deadline.is_elapsed_at(observed_at) {
            return Err(SessionSyncAdmissionErrorV1::DeadlineExceeded);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SessionSyncAdmissionErrorV1 {
    #[error("session sync was cancelled before admission")]
    Cancelled,
    #[error("session sync deadline elapsed before admission")]
    DeadlineExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionSyncAdmissionReceiptV1 {
    pub operation_id: RequestId,
    pub idempotency_key: IdempotencyKey,
    pub accepted_at: UtcMicros,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSyncStatsV1 {
    pub sessions_imported: u64,
    pub messages_imported: u64,
    pub sessions_scanned: u64,
    pub spans_written: u64,
    pub commits_attributed: u64,
    pub skipped: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionSyncCompletionReceiptV1 {
    pub admission: SessionSyncAdmissionReceiptV1,
    pub completed_at: UtcMicros,
    pub termination: OperationTermination,
    pub stats: SessionSyncStatsV1,
    pub failure_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSyncOutcomeV1 {
    Accepted(SessionSyncAdmissionReceiptV1),
    Joined(SessionSyncAdmissionReceiptV1),
    Complete(SessionSyncCompletionReceiptV1),
    Cancelled,
    DeadlineExceeded,
    WrongScope,
    Unavailable { reason_code: &'static str },
}

pub type SessionSyncFuture<'a> = Pin<Box<dyn Future<Output = SessionSyncOutcomeV1> + Send + 'a>>;
pub type SessionSyncShutdownFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Daemon-owned boundary used by CLI/MCP adapters. Implementations schedule
/// bounded convergence and return without awaiting transcript discovery.
pub trait SessionSyncServicePort: Send + Sync {
    fn execute(&self, request: SessionSyncRequestV1) -> SessionSyncFuture<'_>;
    fn shutdown(&self) -> SessionSyncShutdownFuture<'_>;
}

#[cfg(test)]
mod tests {
    use super::{
        SessionSyncCommandV1, SessionSyncRequestV1, SessionSyncScopeV1, SessionTranscriptImportV1,
    };
    use crate::{CancellationSignal, Deadline, IdempotencyKey, RequestId};
    use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

    #[test]
    fn transcript_import_request_rejects_an_elapsed_deadline() {
        let request = SessionSyncRequestV1::new(
            RequestId::new("session-sync.fixture").unwrap(),
            IdempotencyKey::new("session-sync.fixture").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.fixture").unwrap(),
                UserProfileId::new("profile.fixture").unwrap(),
            ),
            Deadline::new(UtcMicros(20)).unwrap(),
            CancellationSignal::active("session-sync.fixture").unwrap(),
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
        );

        assert!(request.admit_at(UtcMicros(20)).is_err());
    }

    #[test]
    fn transcript_import_request_rejects_pre_cancelled_work() {
        let cancellation = CancellationSignal::active("session-sync.cancelled").unwrap();
        assert!(cancellation.cancel(UtcMicros(10)));
        let request = SessionSyncRequestV1::new(
            RequestId::new("session-sync.cancelled").unwrap(),
            IdempotencyKey::new("session-sync.cancelled").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.fixture").unwrap(),
                UserProfileId::new("profile.fixture").unwrap(),
            ),
            Deadline::new(UtcMicros(20)).unwrap(),
            cancellation,
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
        );

        assert!(request.admit_at(UtcMicros(11)).is_err());
    }
}
