use tracedecay_domain::FactEventId;
use tracedecay_store::{
    FactCommitConflict, FactStoreError, FactStoreResult, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactIdV1,
};

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;

use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, row_string, storage_error, storage_message,
};

async fn verify_reviewed_last_event_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactIdV1,
    expected: &FactEventId,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(target.owner())?;
    let mut rows = transaction
        .query(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
             LIMIT 1",
            params![key.kind, key.project_id.as_str(), target.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let actual = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .map(|row| row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION))
        .transpose()?
        .map(FactEventId::new)
        .transpose()?;
    if actual.as_ref() != Some(expected) {
        return Err(FactStoreError::CommitConflict {
            conflict: FactCommitConflict::LastEventMismatch {
                expected: Some(expected.clone()),
                actual,
            },
        });
    }
    Ok(())
}

pub(super) async fn verify_curation_review_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactCurationBatchV1,
) -> FactStoreResult<()> {
    for operation in request.operations() {
        match operation {
            ProjectMemoryFactCurationOperationV1::Add(operation) => {
                verify_refs(transaction, operation.evidence().facts()).await?;
            }
            ProjectMemoryFactCurationOperationV1::Update(operation) => {
                let expected = operation
                    .command()
                    .expected_last_event_id()
                    .ok_or_else(|| {
                        storage_message(
                            PROJECT_MEMORY_WRITE_OPERATION,
                            "curation update is missing its reviewed event identity",
                        )
                    })?;
                verify_reviewed_last_event_tx(transaction, operation.command().target(), expected)
                    .await?;
                verify_refs(transaction, operation.evidence().facts()).await?;
            }
            ProjectMemoryFactCurationOperationV1::Merge(operation) => {
                verify_reviewed_last_event_tx(
                    transaction,
                    operation.command().winner(),
                    operation.command().winner_target().expected_last_event_id(),
                )
                .await?;
                for loser in operation.command().loser_targets() {
                    verify_reviewed_last_event_tx(
                        transaction,
                        loser.fact(),
                        loser.expected_last_event_id(),
                    )
                    .await?;
                }
                verify_refs(transaction, operation.evidence().facts()).await?;
            }
            ProjectMemoryFactCurationOperationV1::Remove(operation) => {
                let expected = operation
                    .command()
                    .expected_last_event_id()
                    .ok_or_else(|| {
                        storage_message(
                            PROJECT_MEMORY_WRITE_OPERATION,
                            "curation remove is missing its reviewed event identity",
                        )
                    })?;
                verify_reviewed_last_event_tx(transaction, operation.command().target(), expected)
                    .await?;
                verify_refs(transaction, operation.evidence().facts()).await?;
            }
            ProjectMemoryFactCurationOperationV1::NormalizeTags(operation) => {
                verify_reviewed_last_event_tx(
                    transaction,
                    operation.fact().fact(),
                    operation.fact().expected_last_event_id(),
                )
                .await?;
                verify_refs(transaction, operation.evidence_facts()).await?;
            }
            ProjectMemoryFactCurationOperationV1::LinkFacts(operation) => {
                verify_reviewed_last_event_tx(
                    transaction,
                    operation.source().fact(),
                    operation.source().expected_last_event_id(),
                )
                .await?;
                verify_reviewed_last_event_tx(
                    transaction,
                    operation.target().fact(),
                    operation.target().expected_last_event_id(),
                )
                .await?;
                verify_refs(transaction, operation.evidence_facts()).await?;
            }
        }
    }
    Ok(())
}

async fn verify_refs(
    transaction: &Transaction<'_>,
    reviewed: &[tracedecay_store::ProjectMemoryFactCurationReviewRefV1],
) -> FactStoreResult<()> {
    for reviewed in reviewed {
        verify_reviewed_last_event_tx(
            transaction,
            reviewed.fact(),
            reviewed.expected_last_event_id(),
        )
        .await?;
    }
    Ok(())
}
