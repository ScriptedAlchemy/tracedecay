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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTranscriptImportV1;

impl SessionTranscriptImportV1 {
    pub const fn all_hosts() -> Self {
        Self
    }
}

/// Bounded session/Git convergence request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "options")]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum SessionSyncCoverageV1 {
    Complete,
    Partial {
        deferred_units: u64,
    },
    Backpressured {
        admitted_units: u64,
        rejected_units: u64,
    },
}

impl SessionSyncCoverageV1 {
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    pub const fn remaining_work(&self) -> u64 {
        match self {
            Self::Complete => 0,
            Self::Partial { deferred_units } => *deferred_units,
            Self::Backpressured { rejected_units, .. } => *rejected_units,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSyncSourceCoverageV1 {
    pub store_scope: String,
    pub coverage: SessionSyncCoverageV1,
}

/// Exact canonical observation cursor committed by one source/store authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSyncSourceFrontierV1 {
    pub store_scope: String,
    pub source_json: String,
    pub scope_json: String,
    pub committed_cursor_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSyncCompletionReceiptV1 {
    pub admission: SessionSyncAdmissionReceiptV1,
    pub coalesced_primary: Option<IdempotencyKey>,
    pub completed_at: UtcMicros,
    pub termination: OperationTermination,
    pub stats: SessionSyncStatsV1,
    pub coverage: Vec<SessionSyncSourceCoverageV1>,
    pub source_frontiers: Vec<SessionSyncSourceFrontierV1>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSyncControlV1 {
    scope: SessionSyncScopeV1,
    idempotency_key: IdempotencyKey,
}

impl SessionSyncControlV1 {
    pub fn new(scope: SessionSyncScopeV1, idempotency_key: IdempotencyKey) -> Self {
        Self {
            scope,
            idempotency_key,
        }
    }

    pub fn scope(&self) -> &SessionSyncScopeV1 {
        &self.scope
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSyncJournalStatusV1 {
    Queued,
    Running,
    Complete,
}

/// Durable source/frontier and terminal evidence for one exact idempotency key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSyncJournalV1 {
    pub admission: SessionSyncAdmissionReceiptV1,
    pub scope: SessionSyncScopeV1,
    pub source: SessionSyncCommandV1,
    pub deadline: Deadline,
    pub status: SessionSyncJournalStatusV1,
    pub coalesced_primary: Option<IdempotencyKey>,
    pub stats: SessionSyncStatsV1,
    pub coverage: Vec<SessionSyncSourceCoverageV1>,
    pub source_frontiers: Vec<SessionSyncSourceFrontierV1>,
    pub cancel_requested_at: Option<UtcMicros>,
    pub completion: Option<SessionSyncCompletionReceiptV1>,
    pub updated_at: UtcMicros,
}

impl SessionSyncJournalV1 {
    pub fn queued(request: &SessionSyncRequestV1, accepted_at: UtcMicros) -> Self {
        Self {
            admission: SessionSyncAdmissionReceiptV1 {
                operation_id: request.operation_id().clone(),
                idempotency_key: request.idempotency_key().clone(),
                accepted_at,
            },
            scope: request.scope().clone(),
            source: request.command(),
            deadline: request.deadline().clone(),
            status: SessionSyncJournalStatusV1::Queued,
            coalesced_primary: None,
            stats: SessionSyncStatsV1::default(),
            coverage: Vec::new(),
            source_frontiers: Vec::new(),
            cancel_requested_at: request.cancellation().cancelled_at(),
            completion: None,
            updated_at: accepted_at,
        }
    }

    pub fn coalesced(
        request: &SessionSyncRequestV1,
        accepted_at: UtcMicros,
        primary: IdempotencyKey,
    ) -> Self {
        let mut journal = Self::queued(request, accepted_at);
        journal.coalesced_primary = Some(primary);
        journal
    }

    pub fn outcome(&self) -> SessionSyncOutcomeV1 {
        match (&self.status, &self.completion) {
            (SessionSyncJournalStatusV1::Queued | SessionSyncJournalStatusV1::Running, _) => {
                SessionSyncOutcomeV1::Joined(self.admission.clone())
            }
            (SessionSyncJournalStatusV1::Complete, Some(receipt)) => {
                SessionSyncOutcomeV1::Complete(receipt.clone())
            }
            (SessionSyncJournalStatusV1::Complete, None) => SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_journal_incomplete",
            },
        }
    }
}

pub type SessionSyncFuture<'a> = Pin<Box<dyn Future<Output = SessionSyncOutcomeV1> + Send + 'a>>;
pub type SessionSyncShutdownFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Daemon-owned boundary used by CLI/MCP adapters. Implementations schedule
/// bounded convergence and return without awaiting transcript discovery.
pub trait SessionSyncServicePort: Send + Sync {
    fn execute(&self, request: SessionSyncRequestV1) -> SessionSyncFuture<'_>;
    fn status(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_>;
    fn cancel(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_>;
    fn shutdown(&self) -> SessionSyncShutdownFuture<'_>;
}

#[cfg(test)]
mod tests {
    use super::{
        SessionSyncCommandV1, SessionSyncCompletionReceiptV1, SessionSyncCoverageV1,
        SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1,
        SessionSyncRequestV1, SessionSyncScopeV1, SessionSyncSourceCoverageV1,
        SessionSyncSourceFrontierV1, SessionTranscriptImportV1,
    };
    use crate::{CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId};
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

    #[test]
    fn durable_journal_round_trip_preserves_source_frontier_status_and_cancel() {
        let cancellation = CancellationSignal::active("session-sync.journal").unwrap();
        let request = SessionSyncRequestV1::new(
            RequestId::new("session-sync.journal").unwrap(),
            IdempotencyKey::new("session-sync.journal").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.fixture").unwrap(),
                UserProfileId::new("profile.fixture").unwrap(),
            ),
            Deadline::new(UtcMicros(200)).unwrap(),
            cancellation,
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
        );
        let mut journal = SessionSyncJournalV1::queued(&request, UtcMicros(10));
        journal.status = SessionSyncJournalStatusV1::Running;
        journal.coalesced_primary = Some(IdempotencyKey::new("session-sync.primary").unwrap());
        journal.stats.sessions_imported = 3;
        journal.stats.messages_imported = 8;
        journal.coverage = vec![SessionSyncSourceCoverageV1 {
            store_scope: "project".to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 2 },
        }];
        journal.source_frontiers = vec![SessionSyncSourceFrontierV1 {
            store_scope: "project".to_owned(),
            source_json: r#"{"provider":"codex"}"#.to_owned(),
            scope_json: r#"{"project_id":"project.fixture"}"#.to_owned(),
            committed_cursor_json: r#"{"byte_offset":72}"#.to_owned(),
        }];
        journal.cancel_requested_at = Some(UtcMicros(50));
        let encoded = serde_json::to_string(&journal).unwrap();
        let restored: SessionSyncJournalV1 = serde_json::from_str(&encoded).unwrap();

        assert_eq!(restored.source, request.command());
        assert_eq!(restored.stats.sessions_imported, 3);
        assert_eq!(restored.stats.messages_imported, 8);
        assert_eq!(
            restored.coverage[0].coverage,
            SessionSyncCoverageV1::Partial { deferred_units: 2 }
        );
        assert_eq!(
            restored.source_frontiers[0].committed_cursor_json,
            r#"{"byte_offset":72}"#
        );
        assert_eq!(
            restored
                .coalesced_primary
                .as_ref()
                .map(IdempotencyKey::as_str),
            Some("session-sync.primary")
        );
        assert_eq!(restored.status, SessionSyncJournalStatusV1::Running);
        assert_eq!(restored.cancel_requested_at, Some(UtcMicros(50)));
        assert!(matches!(
            restored.outcome(),
            SessionSyncOutcomeV1::Joined(_)
        ));
    }

    #[test]
    fn completed_coalesced_journal_replays_its_own_admission_and_primary_binding() {
        let request = SessionSyncRequestV1::new(
            RequestId::new("session-sync.alias").unwrap(),
            IdempotencyKey::new("session-sync.alias").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.fixture").unwrap(),
                UserProfileId::new("profile.fixture").unwrap(),
            ),
            Deadline::new(UtcMicros(200)).unwrap(),
            CancellationSignal::active("session-sync.alias").unwrap(),
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
        );
        let primary = IdempotencyKey::new("session-sync.primary").unwrap();
        let mut journal = SessionSyncJournalV1::coalesced(&request, UtcMicros(10), primary.clone());
        journal.status = SessionSyncJournalStatusV1::Complete;
        journal.completion = Some(SessionSyncCompletionReceiptV1 {
            admission: journal.admission.clone(),
            coalesced_primary: Some(primary.clone()),
            completed_at: UtcMicros(20),
            termination: OperationTermination::Completed,
            stats: Default::default(),
            coverage: vec![SessionSyncSourceCoverageV1 {
                store_scope: "profile".to_owned(),
                coverage: SessionSyncCoverageV1::Complete,
            }],
            source_frontiers: Vec::new(),
            failure_codes: Vec::new(),
        });
        let restored: SessionSyncJournalV1 =
            serde_json::from_str(&serde_json::to_string(&journal).unwrap()).unwrap();

        assert_eq!(restored.coalesced_primary, Some(primary.clone()));
        assert!(matches!(
            restored.outcome(),
            SessionSyncOutcomeV1::Complete(receipt)
                if receipt.admission.idempotency_key.as_str() == "session-sync.alias"
                    && receipt.coalesced_primary == Some(primary)
        ));
    }
}
