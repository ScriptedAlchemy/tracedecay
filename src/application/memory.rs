//! Typed memory mutations shared by transport and storage adapters.

use std::collections::BTreeSet;
use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DomainError, FactAssertionId, FactEventId, FactOwnerV1, PayloadAccessState,
};
use tracedecay_store::FactCommitConflict;

use crate::memory::types::{
    AddFactDiff, AddFactDiffKind, AddFactOutcome, AddFactRequest, FactRecord, FeedbackRequest,
    FeedbackResult, MemoryRepairStats, UpdateFactRequest,
};

pub const MAX_DERIVED_REPAIR_BATCH: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryMutationKind {
    Add,
    Correction,
    Deletion,
    Feedback,
    RetrievalCounters,
    DerivedRepair,
}

/// One application-level memory mutation. Every command carries its canonical
/// owner; adapters must not infer ownership from a database path or transport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMutationCommand {
    Add {
        owner: FactOwnerV1,
        request: AddFactRequest,
    },
    Correct {
        owner: FactOwnerV1,
        request: UpdateFactRequest,
        supersedes: FactAssertionId,
        expected_last_event_id: Option<FactEventId>,
    },
    Delete {
        owner: FactOwnerV1,
        fact_id: i64,
        expected_last_event_id: Option<FactEventId>,
    },
    Feedback {
        owner: FactOwnerV1,
        request: FeedbackRequest,
        expected_last_event_id: Option<FactEventId>,
    },
    RecordRetrieval {
        owner: FactOwnerV1,
        fact_ids: Vec<i64>,
        recall: bool,
    },
    RepairDerived {
        owner: FactOwnerV1,
        max_missing_vectors: usize,
        max_dirty_banks: usize,
    },
}

impl MemoryMutationCommand {
    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Add { owner, .. }
            | Self::Correct { owner, .. }
            | Self::Delete { owner, .. }
            | Self::Feedback { owner, .. }
            | Self::RecordRetrieval { owner, .. }
            | Self::RepairDerived { owner, .. } => owner,
        }
    }

    const fn kind(&self) -> MemoryMutationKind {
        match self {
            Self::Add { .. } => MemoryMutationKind::Add,
            Self::Correct { .. } => MemoryMutationKind::Correction,
            Self::Delete { .. } => MemoryMutationKind::Deletion,
            Self::Feedback { .. } => MemoryMutationKind::Feedback,
            Self::RecordRetrieval { .. } => MemoryMutationKind::RetrievalCounters,
            Self::RepairDerived { .. } => MemoryMutationKind::DerivedRepair,
        }
    }
}

/// Compatibility projection after an immutable correction assertion is
/// appended. `assertion_id` must differ from `superseded_assertion_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryCorrectionResult {
    pub fact: FactRecord,
    pub superseded_assertion_id: FactAssertionId,
    pub assertion_id: FactAssertionId,
}

/// Deletion is a lineage tombstone, not destruction of the fact identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDeletionResult {
    pub fact_id: i64,
    pub payload_access: PayloadAccessState,
    pub event_id: FactEventId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryFeedbackEventResult {
    pub feedback: FeedbackResult,
    pub event_id: FactEventId,
}

/// Counts committed by one transaction. Retrieval counts preserve duplicate
/// hits; recall counts at most once per distinct fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRetrievalCounterResult {
    pub fact_count: usize,
    pub retrieval_increments: usize,
    pub recall_increments: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMutationResult {
    Added(AddFactOutcome),
    Corrected(MemoryCorrectionResult),
    Deleted(MemoryDeletionResult),
    FeedbackRecorded(MemoryFeedbackEventResult),
    RetrievalRecorded(MemoryRetrievalCounterResult),
    DerivedRepaired(MemoryRepairStats),
}

impl MemoryMutationResult {
    const fn kind(&self) -> MemoryMutationKind {
        match self {
            Self::Added(_) => MemoryMutationKind::Add,
            Self::Corrected(_) => MemoryMutationKind::Correction,
            Self::Deleted(_) => MemoryMutationKind::Deletion,
            Self::FeedbackRecorded(_) => MemoryMutationKind::Feedback,
            Self::RetrievalRecorded(_) => MemoryMutationKind::RetrievalCounters,
            Self::DerivedRepaired(_) => MemoryMutationKind::DerivedRepair,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MemoryWriterError {
    #[error("memory lineage is stale")]
    Stale {
        expected: Option<FactEventId>,
        actual: Option<FactEventId>,
    },
    #[error("memory commit conflicted: {0:?}")]
    Conflict(FactCommitConflict),
    #[error("memory writer operation {operation} failed: {message}")]
    Storage {
        operation: &'static str,
        message: String,
    },
}

/// Single authoritative write call implemented by a daemon or local adapter.
/// The port owns transaction boundaries and never exposes its connection or store.
pub trait MemoryWriterPort: Send + Sync {
    fn write_memory(
        &self,
        command: MemoryMutationCommand,
    ) -> impl Future<Output = Result<MemoryMutationResult, MemoryWriterError>> + Send;
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum MemoryMutationError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("memory command owner does not match the service scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        command_owner: FactOwnerV1,
    },
    #[error("memory command field {field} is invalid")]
    InvalidCommand { field: &'static str },
    #[error("memory lineage is stale")]
    Stale {
        expected: Option<FactEventId>,
        actual: Option<FactEventId>,
    },
    #[error("memory commit conflicted: {0:?}")]
    Conflict(FactCommitConflict),
    #[error("memory content was rejected as secret-like")]
    RejectedSecretLike { diff: AddFactDiff },
    #[error("memory writer operation {operation} failed: {message}")]
    Storage {
        operation: &'static str,
        message: String,
    },
    #[error("memory writer returned a result that violates {invariant}")]
    InvalidWriterResult { invariant: &'static str },
}

impl From<MemoryWriterError> for MemoryMutationError {
    fn from(error: MemoryWriterError) -> Self {
        match error {
            MemoryWriterError::Stale { expected, actual } => Self::Stale { expected, actual },
            MemoryWriterError::Conflict(conflict) => Self::Conflict(conflict),
            MemoryWriterError::Storage { operation, message } => {
                Self::Storage { operation, message }
            }
        }
    }
}

/// Transport-neutral mutation policy bound to one authoritative owner.
pub struct MemoryMutationService<W> {
    scope: FactOwnerV1,
    writer: W,
}

impl<W: MemoryWriterPort> MemoryMutationService<W> {
    pub fn new(scope: FactOwnerV1, writer: W) -> Result<Self, MemoryMutationError> {
        scope.validate()?;
        Ok(Self { scope, writer })
    }

    pub fn scope(&self) -> &FactOwnerV1 {
        &self.scope
    }

    /// Validates command ownership and bounds, then performs exactly one
    /// authoritative port call. Result validation cannot cause another write.
    pub async fn execute(
        &self,
        command: MemoryMutationCommand,
    ) -> Result<MemoryMutationResult, MemoryMutationError> {
        command.owner().validate()?;
        if command.owner() != &self.scope {
            return Err(MemoryMutationError::OwnerMismatch {
                scope: self.scope.clone(),
                command_owner: command.owner().clone(),
            });
        }
        validate_command(&command)?;
        let expectation = MutationExpectation::from_command(&command);
        let result = self.writer.write_memory(command).await?;
        validate_result(&expectation, &result)?;
        if let MemoryMutationResult::Added(outcome) = &result
            && outcome.diff.diff == AddFactDiffKind::RejectedSecretLike
        {
            return Err(MemoryMutationError::RejectedSecretLike {
                diff: outcome.diff.clone(),
            });
        }
        Ok(result)
    }
}

#[derive(Clone, Debug)]
enum MutationExpectation {
    Add,
    Correction {
        fact_id: i64,
        supersedes: FactAssertionId,
    },
    Deletion {
        fact_id: i64,
    },
    Feedback {
        fact_id: i64,
    },
    RetrievalCounters {
        distinct_facts: usize,
        retrieval_increments: usize,
        recall_increments: usize,
    },
    DerivedRepair {
        max_missing_vectors: usize,
        max_dirty_banks: usize,
    },
}

impl MutationExpectation {
    fn from_command(command: &MemoryMutationCommand) -> Self {
        match command {
            MemoryMutationCommand::Add { .. } => Self::Add,
            MemoryMutationCommand::Correct {
                request,
                supersedes,
                ..
            } => Self::Correction {
                fact_id: request.fact_id,
                supersedes: supersedes.clone(),
            },
            MemoryMutationCommand::Delete { fact_id, .. } => Self::Deletion { fact_id: *fact_id },
            MemoryMutationCommand::Feedback { request, .. } => Self::Feedback {
                fact_id: request.fact_id,
            },
            MemoryMutationCommand::RecordRetrieval {
                fact_ids, recall, ..
            } => Self::RetrievalCounters {
                distinct_facts: fact_ids.iter().copied().collect::<BTreeSet<_>>().len(),
                retrieval_increments: fact_ids.len(),
                recall_increments: if *recall {
                    fact_ids.iter().copied().collect::<BTreeSet<_>>().len()
                } else {
                    0
                },
            },
            MemoryMutationCommand::RepairDerived {
                max_missing_vectors,
                max_dirty_banks,
                ..
            } => Self::DerivedRepair {
                max_missing_vectors: *max_missing_vectors,
                max_dirty_banks: *max_dirty_banks,
            },
        }
    }

    const fn kind(&self) -> MemoryMutationKind {
        match self {
            Self::Add => MemoryMutationKind::Add,
            Self::Correction { .. } => MemoryMutationKind::Correction,
            Self::Deletion { .. } => MemoryMutationKind::Deletion,
            Self::Feedback { .. } => MemoryMutationKind::Feedback,
            Self::RetrievalCounters { .. } => MemoryMutationKind::RetrievalCounters,
            Self::DerivedRepair { .. } => MemoryMutationKind::DerivedRepair,
        }
    }
}

fn validate_command(command: &MemoryMutationCommand) -> Result<(), MemoryMutationError> {
    match command {
        MemoryMutationCommand::Add { request, .. } => {
            if request.content.trim().is_empty() {
                return Err(MemoryMutationError::InvalidCommand {
                    field: "add_fact.content",
                });
            }
        }
        MemoryMutationCommand::Correct {
            request,
            supersedes,
            expected_last_event_id,
            ..
        } => {
            validate_fact_id(request.fact_id, "correction.fact_id")?;
            supersedes.validate()?;
            if let Some(event_id) = expected_last_event_id {
                event_id.validate()?;
            }
            if request.content.is_none()
                && request.category.is_none()
                && request.tags.is_none()
                && request.entities.is_none()
                && request.trust.is_none()
                && request.source.is_none()
                && request.metadata.is_none()
            {
                return Err(MemoryMutationError::InvalidCommand {
                    field: "correction.change",
                });
            }
        }
        MemoryMutationCommand::Delete {
            fact_id,
            expected_last_event_id,
            ..
        } => {
            validate_fact_id(*fact_id, "deletion.fact_id")?;
            if let Some(event_id) = expected_last_event_id {
                event_id.validate()?;
            }
        }
        MemoryMutationCommand::Feedback {
            request,
            expected_last_event_id,
            ..
        } => {
            validate_fact_id(request.fact_id, "feedback.fact_id")?;
            if let Some(event_id) = expected_last_event_id {
                event_id.validate()?;
            }
        }
        MemoryMutationCommand::RecordRetrieval { fact_ids, .. } => {
            if fact_ids.iter().any(|fact_id| *fact_id <= 0) {
                return Err(MemoryMutationError::InvalidCommand {
                    field: "retrieval.fact_ids",
                });
            }
        }
        MemoryMutationCommand::RepairDerived {
            max_missing_vectors,
            max_dirty_banks,
            ..
        } => {
            validate_repair_bound(*max_missing_vectors, "repair.max_missing_vectors")?;
            validate_repair_bound(*max_dirty_banks, "repair.max_dirty_banks")?;
        }
    }
    Ok(())
}

fn validate_fact_id(fact_id: i64, field: &'static str) -> Result<(), MemoryMutationError> {
    if fact_id <= 0 {
        return Err(MemoryMutationError::InvalidCommand { field });
    }
    Ok(())
}

fn validate_repair_bound(value: usize, field: &'static str) -> Result<(), MemoryMutationError> {
    if !(1..=MAX_DERIVED_REPAIR_BATCH).contains(&value) {
        return Err(MemoryMutationError::InvalidCommand { field });
    }
    Ok(())
}

fn validate_result(
    expectation: &MutationExpectation,
    result: &MemoryMutationResult,
) -> Result<(), MemoryMutationError> {
    if expectation.kind() != result.kind() {
        return Err(MemoryMutationError::InvalidWriterResult {
            invariant: "command/result kind",
        });
    }
    match (expectation, result) {
        (MutationExpectation::Add, MemoryMutationResult::Added(outcome)) => {
            let rejected = outcome.diff.diff == AddFactDiffKind::RejectedSecretLike;
            if rejected != outcome.fact.is_none() {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "secret rejection payload",
                });
            }
        }
        (
            MutationExpectation::Correction {
                fact_id,
                supersedes,
            },
            MemoryMutationResult::Corrected(corrected),
        ) => {
            if corrected.fact.fact_id != *fact_id
                || &corrected.superseded_assertion_id != supersedes
                || corrected.assertion_id == corrected.superseded_assertion_id
            {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "append-only correction lineage",
                });
            }
        }
        (MutationExpectation::Deletion { fact_id }, MemoryMutationResult::Deleted(deleted)) => {
            if deleted.fact_id != *fact_id || deleted.payload_access != PayloadAccessState::Deleted
            {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "deletion tombstone lineage",
                });
            }
        }
        (
            MutationExpectation::Feedback { fact_id },
            MemoryMutationResult::FeedbackRecorded(recorded),
        ) => {
            if recorded.feedback.fact_id != *fact_id {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "feedback fact identity",
                });
            }
        }
        (
            MutationExpectation::RetrievalCounters {
                distinct_facts,
                retrieval_increments,
                recall_increments,
            },
            MemoryMutationResult::RetrievalRecorded(recorded),
        ) => {
            if recorded.fact_count != *distinct_facts
                || recorded.retrieval_increments != *retrieval_increments
                || recorded.recall_increments != *recall_increments
            {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "combined retrieval counters",
                });
            }
        }
        (
            MutationExpectation::DerivedRepair {
                max_missing_vectors,
                max_dirty_banks,
            },
            MemoryMutationResult::DerivedRepaired(repaired),
        ) => {
            if repaired.missing_vectors_repaired > *max_missing_vectors
                || repaired.banks_rebuilt > *max_dirty_banks
            {
                return Err(MemoryMutationError::InvalidWriterResult {
                    invariant: "derived repair bounds",
                });
            }
        }
        _ => unreachable!("command and result kinds were checked above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use serde_json::json;
    use tracedecay_domain::{ProjectId, research::DomainError};

    use super::*;
    use crate::memory::types::{AddFactDiff, FactRecord, FeedbackAction, MemoryCategory};

    #[derive(Default)]
    struct FakeWriterPort {
        calls: Mutex<Vec<MemoryMutationCommand>>,
        responses: Mutex<VecDeque<Result<MemoryMutationResult, MemoryWriterError>>>,
    }

    impl FakeWriterPort {
        fn with_response(response: Result<MemoryMutationResult, MemoryWriterError>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::from([response])),
            }
        }

        fn push_response(&self, response: Result<MemoryMutationResult, MemoryWriterError>) {
            self.responses.lock().unwrap().push_back(response);
        }
    }

    impl MemoryWriterPort for FakeWriterPort {
        async fn write_memory(
            &self,
            command: MemoryMutationCommand,
        ) -> Result<MemoryMutationResult, MemoryWriterError> {
            self.calls.lock().unwrap().push(command);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("fake response")
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn project_owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.memory.application").unwrap(),
        }
    }

    fn add_request(content: &str) -> AddFactRequest {
        AddFactRequest {
            content: content.to_owned(),
            category: MemoryCategory::Project,
            source: Some("test".to_owned()),
            tags: vec!["memory".to_owned()],
            entities: vec!["TraceDecay".to_owned()],
            trust: Some(0.9),
            metadata: json!({}),
        }
    }

    fn fact_record(fact_id: i64, content: &str) -> FactRecord {
        FactRecord {
            fact_id,
            content: content.to_owned(),
            category: MemoryCategory::Project,
            tags: vec![],
            entities: vec![],
            trust_score: 0.9,
            source: Some("test".to_owned()),
            retrieval_count: 0,
            access_count: 0,
            helpful_count: 0,
            unhelpful_count: 0,
            created_at: 1,
            updated_at: 1,
            last_retrieved_at: None,
            last_recalled_at: None,
            last_feedback_at: None,
            metadata: json!({}),
        }
    }

    fn stored_add(fact_id: i64) -> MemoryMutationResult {
        MemoryMutationResult::Added(AddFactOutcome {
            fact: Some(fact_record(fact_id, "daemon is the only writer")),
            diff: AddFactDiff::plain_add(),
        })
    }

    #[tokio::test]
    async fn owner_is_propagated_through_one_atomic_port_call() {
        let owner = project_owner();
        let service = MemoryMutationService::new(
            owner.clone(),
            FakeWriterPort::with_response(Ok(stored_add(7))),
        )
        .unwrap();
        service
            .execute(MemoryMutationCommand::Add {
                owner: owner.clone(),
                request: add_request("daemon is the only writer"),
            })
            .await
            .unwrap();

        let calls = service.writer.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].owner(), &owner);
    }

    #[tokio::test]
    async fn owner_mismatch_fails_before_the_port() {
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(stored_add(7))),
        )
        .unwrap();
        let error = service
            .execute(MemoryMutationCommand::Add {
                owner: FactOwnerV1::Profile,
                request: add_request("profile fact"),
            })
            .await
            .unwrap_err();

        assert!(matches!(error, MemoryMutationError::OwnerMismatch { .. }));
        assert!(service.writer.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_and_conflict_errors_propagate_without_retry() {
        let expected = id::<FactEventId>("event.expected");
        let actual = id::<FactEventId>("event.actual");
        let port = FakeWriterPort::with_response(Err(MemoryWriterError::Stale {
            expected: Some(expected.clone()),
            actual: Some(actual.clone()),
        }));
        port.push_response(Err(MemoryWriterError::Conflict(
            FactCommitConflict::IdentityCollision {
                kind: "fact",
                id: "fact.collision".to_owned(),
            },
        )));
        let service = MemoryMutationService::new(project_owner(), port).unwrap();
        let command = || MemoryMutationCommand::Delete {
            owner: project_owner(),
            fact_id: 7,
            expected_last_event_id: Some(expected.clone()),
        };

        assert_eq!(
            service.execute(command()).await.unwrap_err(),
            MemoryMutationError::Stale {
                expected: Some(expected),
                actual: Some(actual),
            }
        );
        assert!(matches!(
            service.execute(command()).await.unwrap_err(),
            MemoryMutationError::Conflict(FactCommitConflict::IdentityCollision { .. })
        ));
        assert_eq!(service.writer.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn secret_rejection_is_mapped_to_a_typed_error() {
        let diff = AddFactDiff {
            diff: AddFactDiffKind::RejectedSecretLike,
            closest_fact_id: None,
            similarity: None,
            reason: Some("secret-like".to_owned()),
        };
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::Added(AddFactOutcome {
                fact: None,
                diff: diff.clone(),
            }))),
        )
        .unwrap();

        assert_eq!(
            service
                .execute(MemoryMutationCommand::Add {
                    owner: project_owner(),
                    request: add_request("looks sensitive"),
                })
                .await
                .unwrap_err(),
            MemoryMutationError::RejectedSecretLike { diff }
        );
        assert_eq!(service.writer.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn correction_appends_a_new_assertion_instead_of_updating_in_place() {
        let superseded = id::<FactAssertionId>("assertion.old");
        let correction = id::<FactAssertionId>("assertion.correction");
        let result = MemoryCorrectionResult {
            fact: fact_record(7, "corrected payload"),
            superseded_assertion_id: superseded.clone(),
            assertion_id: correction.clone(),
        };
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::Corrected(result.clone()))),
        )
        .unwrap();
        let outcome = service
            .execute(MemoryMutationCommand::Correct {
                owner: project_owner(),
                request: UpdateFactRequest {
                    fact_id: 7,
                    content: Some("corrected payload".to_owned()),
                    category: None,
                    tags: None,
                    entities: None,
                    trust: None,
                    source: None,
                    metadata: None,
                },
                supersedes: superseded,
                expected_last_event_id: None,
            })
            .await
            .unwrap();

        assert_eq!(outcome, MemoryMutationResult::Corrected(result));
        assert_ne!(correction, id::<FactAssertionId>("assertion.old"));
        assert_eq!(service.writer.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deletion_requires_a_deleted_payload_tombstone() {
        let event_id = id::<FactEventId>("event.deleted");
        let deleted = MemoryDeletionResult {
            fact_id: 7,
            payload_access: PayloadAccessState::Deleted,
            event_id,
        };
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::Deleted(deleted.clone()))),
        )
        .unwrap();

        assert_eq!(
            service
                .execute(MemoryMutationCommand::Delete {
                    owner: project_owner(),
                    fact_id: 7,
                    expected_last_event_id: None,
                })
                .await
                .unwrap(),
            MemoryMutationResult::Deleted(deleted)
        );
    }

    #[tokio::test]
    async fn feedback_is_a_lineage_event() {
        let feedback = FeedbackResult {
            event_id: 3,
            fact_id: 7,
            action: FeedbackAction::Helpful,
            old_trust: 0.5,
            new_trust: 0.6,
            trust_delta: 0.1,
            helpful_count: 1,
            unhelpful_count: 0,
        };
        let recorded = MemoryFeedbackEventResult {
            feedback,
            event_id: id("event.feedback"),
        };
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::FeedbackRecorded(
                recorded.clone(),
            ))),
        )
        .unwrap();

        assert_eq!(
            service
                .execute(MemoryMutationCommand::Feedback {
                    owner: project_owner(),
                    request: FeedbackRequest {
                        fact_id: 7,
                        action: FeedbackAction::Helpful,
                        source: None,
                        note: None,
                    },
                    expected_last_event_id: None,
                })
                .await
                .unwrap(),
            MemoryMutationResult::FeedbackRecorded(recorded)
        );
    }

    #[tokio::test]
    async fn retrieval_and_recall_counters_share_one_atomic_call() {
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::RetrievalRecorded(
                MemoryRetrievalCounterResult {
                    fact_count: 2,
                    retrieval_increments: 3,
                    recall_increments: 2,
                },
            ))),
        )
        .unwrap();
        service
            .execute(MemoryMutationCommand::RecordRetrieval {
                owner: project_owner(),
                fact_ids: vec![7, 7, 8],
                recall: true,
            })
            .await
            .unwrap();

        let calls = service.writer.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            &calls[0],
            MemoryMutationCommand::RecordRetrieval {
                fact_ids,
                recall: true,
                ..
            } if fact_ids == &[7, 7, 8]
        ));
    }

    #[tokio::test]
    async fn derived_repair_is_bounded_before_and_after_the_port() {
        let service = MemoryMutationService::new(
            project_owner(),
            FakeWriterPort::with_response(Ok(MemoryMutationResult::DerivedRepaired(
                MemoryRepairStats {
                    missing_vectors_repaired: MAX_DERIVED_REPAIR_BATCH,
                    banks_rebuilt: 1,
                },
            ))),
        )
        .unwrap();
        let invalid = service
            .execute(MemoryMutationCommand::RepairDerived {
                owner: project_owner(),
                max_missing_vectors: MAX_DERIVED_REPAIR_BATCH + 1,
                max_dirty_banks: 1,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            invalid,
            MemoryMutationError::InvalidCommand {
                field: "repair.max_missing_vectors"
            }
        ));
        assert!(service.writer.calls.lock().unwrap().is_empty());

        service
            .execute(MemoryMutationCommand::RepairDerived {
                owner: project_owner(),
                max_missing_vectors: MAX_DERIVED_REPAIR_BATCH,
                max_dirty_banks: 1,
            })
            .await
            .unwrap();
        assert_eq!(service.writer.calls.lock().unwrap().len(), 1);
    }
}
