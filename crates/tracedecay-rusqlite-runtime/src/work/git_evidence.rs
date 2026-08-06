//! Durable Work-to-Git graph evidence publication journal.

use super::attempts::{AttemptStoreError, AttemptStoreResult};
use super::*;

const MAX_PENDING_INTENTS: u32 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkGitGraphEvidenceJournalEntry {
    journal_sequence: u64,
    intent: GitGraphEvidenceIntent,
}

impl WorkGitGraphEvidenceJournalEntry {
    #[must_use]
    pub const fn journal_sequence(&self) -> u64 {
        self.journal_sequence
    }

    #[must_use]
    pub const fn intent(&self) -> &GitGraphEvidenceIntent {
        &self.intent
    }
}

pub(super) fn stage(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
    let Ok(commit) = GitOidV1::new(attempt.execution().commit().as_str().to_owned()) else {
        return Ok(());
    };
    let intent = GitGraphEvidenceIntent::new(
        authority.project_id().clone(),
        commit,
        GitGraphEvidenceTarget::Work(attempt.identity().task_id().clone()),
    )
    .map_err(|_| AttemptStoreError::InvalidRequest)?;
    let intent_payload =
        serde_json::to_string(&intent).map_err(|_| AttemptStoreError::Unavailable)?;
    let params = authority_params_owned(authority)
        .into_iter()
        .chain([
            ExactSqlValue::Text(attempt.identity().task_id().as_str().to_owned()),
            ExactSqlValue::Text(attempt.identity().run_id().as_str().to_owned()),
            ExactSqlValue::Text(attempt.identity().attempt_id().as_str().to_owned()),
            ExactSqlValue::Text(intent.intent_digest().as_str().to_owned()),
            ExactSqlValue::Text(intent_payload.clone()),
        ])
        .collect();
    let outcome = transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_git_graph_evidence_journal (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, intent_digest, intent_payload,
                    state, receipt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', NULL)
                 ON CONFLICT (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, intent_digest
                 ) DO NOTHING",
                params,
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    if outcome.changed_rows == 1 {
        return Ok(());
    }
    let rows = registered_work_query(
        transaction,
        "SELECT intent_payload
         FROM work_git_graph_evidence_journal
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND intent_digest = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(
                intent.intent_digest().as_str().to_owned(),
            )])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let stored = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
        .ok_or(AttemptStoreError::Unavailable)?;
    if stored != intent_payload {
        return Err(AttemptStoreError::Conflict);
    }
    Ok(())
}

pub(super) fn pending(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    limit: u32,
) -> AttemptStoreResult<Vec<WorkGitGraphEvidenceJournalEntry>> {
    if limit == 0 || limit > MAX_PENDING_INTENTS {
        return Err(AttemptStoreError::InvalidRequest);
    }
    let rows = registered_work_query(
        handle,
        "SELECT journal_sequence, intent_digest, intent_payload
         FROM work_git_graph_evidence_journal
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND state = 'pending'
         ORDER BY journal_sequence
         LIMIT ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Integer(i64::from(limit))])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    rows.rows
        .into_iter()
        .map(|row| {
            let journal_sequence = u64::try_from(
                exact_sql_integer(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?,
            )
            .map_err(|_| AttemptStoreError::Unavailable)?;
            let intent_digest =
                exact_sql_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
            let payload = exact_sql_text(&row.values, 2).ok_or(AttemptStoreError::Unavailable)?;
            let intent: GitGraphEvidenceIntent =
                serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)?;
            intent
                .validate()
                .map_err(|_| AttemptStoreError::Unavailable)?;
            if intent.project_id() != authority.project_id()
                || intent.intent_digest().as_str() != intent_digest
                || !matches!(intent.target(), GitGraphEvidenceTarget::Work(_))
            {
                return Err(AttemptStoreError::Unavailable);
            }
            Ok(WorkGitGraphEvidenceJournalEntry {
                journal_sequence,
                intent,
            })
        })
        .collect()
}

pub(super) fn acknowledge(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    journal_sequence: u64,
    receipt: &GitGraphEvidencePublicationReceipt,
) -> AttemptStoreResult<()> {
    if journal_sequence == 0 {
        return Err(AttemptStoreError::InvalidRequest);
    }
    receipt
        .validate()
        .map_err(|_| AttemptStoreError::InvalidRequest)?;
    let transaction = handle
        .begin_immediate()
        .map_err(|_| AttemptStoreError::Unavailable)?;
    let rows = registered_work_query(
        &transaction,
        "SELECT state, intent_digest, receipt_payload
         FROM work_git_graph_evidence_journal
         WHERE journal_sequence = ?1
           AND project_id = ?2
           AND repository_id = ?3
           AND worktree_id = ?4
           AND actor_id = ?5
           AND policy_digest = ?6",
        [ExactSqlValue::Integer(
            i64::try_from(journal_sequence).map_err(|_| AttemptStoreError::InvalidRequest)?,
        )]
        .into_iter()
        .chain(authority_params_owned(authority))
        .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        let _ = transaction.rollback();
        return Err(AttemptStoreError::Conflict);
    };
    let state = exact_sql_text(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
    let intent_digest = exact_sql_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
    if receipt.intent_digest().as_str() != intent_digest {
        let _ = transaction.rollback();
        return Err(AttemptStoreError::InvalidRequest);
    }
    if state == "acknowledged" {
        let payload = exact_sql_text(&row.values, 2).ok_or(AttemptStoreError::Unavailable)?;
        let stored: GitGraphEvidencePublicationReceipt =
            serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)?;
        stored
            .validate()
            .map_err(|_| AttemptStoreError::Unavailable)?;
        let result = if &stored == receipt {
            Ok(())
        } else {
            Err(AttemptStoreError::Conflict)
        };
        let _ = transaction.rollback();
        return result;
    }
    if state != "pending" {
        let _ = transaction.rollback();
        return Err(AttemptStoreError::Unavailable);
    }
    let receipt_payload =
        serde_json::to_string(receipt).map_err(|_| AttemptStoreError::Unavailable)?;
    let changed = transaction
        .execute(
            exact_sql_statement(
                "UPDATE work_git_graph_evidence_journal
                 SET state = 'acknowledged', receipt_payload = ?1
                 WHERE journal_sequence = ?2
                   AND project_id = ?3
                   AND repository_id = ?4
                   AND worktree_id = ?5
                   AND actor_id = ?6
                   AND policy_digest = ?7
                   AND state = 'pending'
                   AND intent_digest = ?8",
                [
                    ExactSqlValue::Text(receipt_payload),
                    ExactSqlValue::Integer(
                        i64::try_from(journal_sequence)
                            .map_err(|_| AttemptStoreError::InvalidRequest)?,
                    ),
                ]
                .into_iter()
                .chain(authority_params_owned(authority))
                .chain([ExactSqlValue::Text(intent_digest.to_owned())])
                .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    if changed.changed_rows != 1 {
        let _ = transaction.rollback();
        return Err(AttemptStoreError::Conflict);
    }
    transaction
        .commit()
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}
