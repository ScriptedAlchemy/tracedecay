//! Durable Work-minted managed-Test retry evidence journal.

use serde::Deserialize;
use tracedecay_application::{
    WorkAttemptStorageError, WorkRetryEvidenceBindingSourceV1,
    WorkRetryEvidenceBindingStoragePortV1, WorkRetryEvidenceBindingV1, WorkRetryFailureSelectorV1,
    WorkRetrySourceV1, WorkRetryTestBindingTokenV1, WorkRetryTestFailureEvidenceV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkAttemptIdentityV1, WorkAttemptV1, WorkAuthority,
    WorkTerminalEvidenceV1, canonical_sha256,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, WorkSqliteStorage, authority_params_owned, exact_sql_integer,
    exact_sql_statement, exact_sql_text, registered_work_query,
};

const TEST_EVIDENCE_DIGEST_DOMAIN_V1: &str = "tracedecay.application.work-retry-test-evidence.v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredWorkAttemptV1 {
    attempt: WorkAttemptV1,
    #[serde(rename = "synthesis")]
    _synthesis: Option<serde_json::Value>,
}

impl WorkRetryEvidenceBindingStoragePortV1 for WorkSqliteStorage {
    fn resolve_test_retry_binding_authority(
        &self,
        token: &WorkRetryTestBindingTokenV1,
    ) -> Result<WorkAuthority, WorkAttemptStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT project_id, repository_id, worktree_id, actor_id, policy_digest
             FROM work_retry_test_binding_tokens_v1 WHERE token_id = ?1",
            vec![ExactSqlValue::Text(token.as_str().to_owned())],
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        match rows.rows.as_slice() {
            [] => Err(WorkAttemptStorageError::NotFoundOrNotAuthorized),
            [row] => WorkAuthority::new(
                tracedecay_domain::ProjectId::new(
                    exact_sql_text(&row.values, 0)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                tracedecay_domain::RepositoryId::new(
                    exact_sql_text(&row.values, 1)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                tracedecay_domain::WorktreeId::new(
                    exact_sql_text(&row.values, 2)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                tracedecay_domain::ActorId::new(
                    exact_sql_text(&row.values, 3)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                ManifestDigest::new(
                    exact_sql_text(&row.values, 4)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable),
            _ => Err(WorkAttemptStorageError::Unavailable),
        }
    }

    fn mint_test_retry_binding_token(
        &self,
        authority: &WorkAuthority,
        original_attempt: &WorkAttemptIdentityV1,
        token: &WorkRetryTestBindingTokenV1,
        minted_at: UtcMicros,
    ) -> Result<WorkRetryTestBindingTokenV1, WorkAttemptStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        require_terminal_non_success_attempt(&transaction, authority, original_attempt)?;
        if let Some(existing) = load_token_by_attempt(&transaction, authority, original_attempt)? {
            match existing.state {
                TokenState::Minted => {
                    let _ = transaction.rollback();
                    return Ok(existing.token);
                }
                TokenState::Launched => {
                    let _ = transaction.rollback();
                    return Err(WorkAttemptStorageError::AttemptConflict);
                }
                TokenState::Sealed
                    if existing
                        .sealed_evidence
                        .as_ref()
                        .is_some_and(WorkRetryTestFailureEvidenceV1::is_failure) =>
                {
                    let _ = transaction.rollback();
                    return Err(WorkAttemptStorageError::AttemptConflict);
                }
                // Operational terminals without affirmative failed tests do
                // not authorize retry and must not permanently strand the
                // Work attempt. A new token may start the next exact run.
                TokenState::Sealed => {}
            }
        }
        transaction
            .execute(
                exact_sql_statement(
                    "INSERT INTO work_retry_test_binding_tokens_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        token_id, task_id, run_id, attempt_id, minted_at, state,
                        source_ref, sealed_evidence_digest, sealed_evidence_payload
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'minted', NULL, NULL, NULL
                     )",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain([
                            ExactSqlValue::Text(token.as_str().to_owned()),
                            ExactSqlValue::Text(original_attempt.task_id().as_str().to_owned()),
                            ExactSqlValue::Text(original_attempt.run_id().as_str().to_owned()),
                            ExactSqlValue::Text(original_attempt.attempt_id().as_str().to_owned()),
                            ExactSqlValue::Integer(minted_at.0),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(token.clone())
    }

    fn launch_test_retry_binding(
        &self,
        authority: &WorkAuthority,
        source: &WorkRetryEvidenceBindingSourceV1,
        token: &WorkRetryTestBindingTokenV1,
    ) -> Result<(), WorkAttemptStorageError> {
        source
            .validate()
            .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let stored = load_token_by_id(&transaction, authority, token)?
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        let source_ref = source.selector().evidence_ref;
        match stored.state {
            TokenState::Minted => {
                let update = transaction
                    .execute(
                        exact_sql_statement(
                            "UPDATE work_retry_test_binding_tokens_v1
                             SET state = 'launched', source_ref = ?6
                             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                               AND actor_id = ?4 AND policy_digest = ?5 AND token_id = ?7
                               AND state = 'minted'",
                            authority_params_owned(authority)
                                .into_iter()
                                .chain([
                                    ExactSqlValue::Text(source_ref),
                                    ExactSqlValue::Text(token.as_str().to_owned()),
                                ])
                                .collect(),
                        )
                        .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                    )
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
                if update.changed_rows != 1 {
                    let _ = transaction.rollback();
                    return Err(WorkAttemptStorageError::AttemptConflict);
                }
            }
            TokenState::Launched if stored.source_ref.as_deref() == Some(source_ref.as_str()) => {}
            TokenState::Launched | TokenState::Sealed => {
                let _ = transaction.rollback();
                return Err(WorkAttemptStorageError::AttemptConflict);
            }
        }
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(())
    }

    fn seal_test_retry_terminal(
        &self,
        authority: &WorkAuthority,
        evidence: &WorkRetryTestFailureEvidenceV1,
    ) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError> {
        evidence
            .validate()
            .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
        let source = WorkRetryEvidenceBindingSourceV1::test(evidence.operation_id.clone())
            .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
        let source_ref = source.selector().evidence_ref;
        let digest = canonical_sha256(&(TEST_EVIDENCE_DIGEST_DOMAIN_V1, evidence))
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let stored = load_token_by_id(&transaction, authority, &evidence.token)?
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        if evidence.observed_at().0 < stored.minted_at.0
            || stored.source_ref.as_deref() != Some(source_ref.as_str())
        {
            let _ = transaction.rollback();
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        if stored.state == TokenState::Sealed {
            let _ = transaction.rollback();
            if stored.sealed_evidence_digest.as_ref() != Some(&digest)
                || stored.sealed_evidence.as_ref() != Some(evidence)
            {
                return Err(WorkAttemptStorageError::AttemptConflict);
            }
            return replay_sealed_terminal(
                &self.handle,
                authority,
                &source_ref,
                &digest,
                evidence.is_failure(),
            );
        }
        if stored.state != TokenState::Launched {
            let _ = transaction.rollback();
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        require_terminal_non_success_attempt(&transaction, authority, &stored.original_attempt)?;
        let binding = if evidence.is_failure() {
            let binding = WorkRetryEvidenceBindingV1::from_sealed_test_terminal(
                source,
                evidence.token.clone(),
                stored.original_attempt,
                digest.clone(),
                evidence.observed_at(),
            )
            .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
            insert_binding(&transaction, authority, &binding)?;
            Some(binding)
        } else {
            None
        };
        let evidence_payload =
            serde_json::to_string(evidence).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let update = transaction
            .execute(
                exact_sql_statement(
                    "UPDATE work_retry_test_binding_tokens_v1
                     SET state = 'sealed', sealed_evidence_digest = ?6, sealed_evidence_payload = ?7
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5 AND token_id = ?8
                       AND state = 'launched'",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain([
                            ExactSqlValue::Text(digest.as_str().to_owned()),
                            ExactSqlValue::Text(evidence_payload),
                            ExactSqlValue::Text(evidence.token.as_str().to_owned()),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if update.changed_rows != 1 {
            let _ = transaction.rollback();
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(binding)
    }

    fn load_retry_evidence(
        &self,
        authority: &WorkAuthority,
        original_attempt: &WorkAttemptIdentityV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError> {
        if selector.source != WorkRetrySourceV1::Test {
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        let binding = load_by_source(&self.handle, authority, &selector.evidence_ref)?;
        let Some(binding) = binding else {
            return Ok(None);
        };
        binding
            .validate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if binding.original_attempt() != original_attempt || binding.selector() != selector {
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        Ok(Some(binding))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenState {
    Minted,
    Launched,
    Sealed,
}

struct StoredTokenV1 {
    token: WorkRetryTestBindingTokenV1,
    original_attempt: WorkAttemptIdentityV1,
    minted_at: UtcMicros,
    state: TokenState,
    source_ref: Option<String>,
    sealed_evidence_digest: Option<ManifestDigest>,
    sealed_evidence: Option<WorkRetryTestFailureEvidenceV1>,
}

fn load_token_by_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    original_attempt: &WorkAttemptIdentityV1,
) -> Result<Option<StoredTokenV1>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT token_id, minted_at, state, source_ref, sealed_evidence_digest, sealed_evidence_payload
         FROM work_retry_test_binding_tokens_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8
         ORDER BY CASE state WHEN 'minted' THEN 0 WHEN 'launched' THEN 1 ELSE 2 END,
                  minted_at DESC, token_id DESC
         LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(original_attempt))
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            stored_token(
                exact_sql_text(&row.values, 0).ok_or(WorkAttemptStorageError::Unavailable)?,
                original_attempt.clone(),
                exact_sql_integer(&row.values, 1).ok_or(WorkAttemptStorageError::Unavailable)?,
                exact_sql_text(&row.values, 2).ok_or(WorkAttemptStorageError::Unavailable)?,
                exact_sql_text(&row.values, 3).map(str::to_owned),
                exact_sql_text(&row.values, 4).map(str::to_owned),
                exact_sql_text(&row.values, 5).map(str::to_owned),
            )
        })
        .transpose()
}

fn load_token_by_id(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    token: &WorkRetryTestBindingTokenV1,
) -> Result<Option<StoredTokenV1>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT task_id, run_id, attempt_id, minted_at, state, source_ref, sealed_evidence_digest,
                sealed_evidence_payload
         FROM work_retry_test_binding_tokens_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND token_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(token.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let identity = WorkAttemptIdentityV1::new(
                tracedecay_domain::TaskId::new(
                    exact_sql_text(&row.values, 0)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                tracedecay_domain::RunId::new(
                    exact_sql_text(&row.values, 1)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                tracedecay_domain::AttemptId::new(
                    exact_sql_text(&row.values, 2)
                        .ok_or(WorkAttemptStorageError::Unavailable)?
                        .to_owned(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            stored_token(
                token.as_str(),
                identity,
                exact_sql_integer(&row.values, 3).ok_or(WorkAttemptStorageError::Unavailable)?,
                exact_sql_text(&row.values, 4).ok_or(WorkAttemptStorageError::Unavailable)?,
                exact_sql_text(&row.values, 5).map(str::to_owned),
                exact_sql_text(&row.values, 6).map(str::to_owned),
                exact_sql_text(&row.values, 7).map(str::to_owned),
            )
        })
        .transpose()
}

fn stored_token(
    token: &str,
    original_attempt: WorkAttemptIdentityV1,
    minted_at: i64,
    state: &str,
    source_ref: Option<String>,
    sealed_evidence_digest: Option<String>,
    sealed_evidence_payload: Option<String>,
) -> Result<StoredTokenV1, WorkAttemptStorageError> {
    let token = WorkRetryTestBindingTokenV1::new(token.to_owned())
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let state = match state {
        "minted" => TokenState::Minted,
        "launched" => TokenState::Launched,
        "sealed" => TokenState::Sealed,
        _ => return Err(WorkAttemptStorageError::Unavailable),
    };
    let sealed_evidence_digest = sealed_evidence_digest
        .map(ManifestDigest::new)
        .transpose()
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let sealed_evidence = sealed_evidence_payload
        .map(|payload| serde_json::from_str::<WorkRetryTestFailureEvidenceV1>(&payload))
        .transpose()
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    if matches!(state, TokenState::Minted)
        != (source_ref.is_none() && sealed_evidence_digest.is_none() && sealed_evidence.is_none())
        || matches!(state, TokenState::Launched)
            != (source_ref.is_some()
                && sealed_evidence_digest.is_none()
                && sealed_evidence.is_none())
        || matches!(state, TokenState::Sealed)
            != (source_ref.is_some()
                && sealed_evidence_digest.is_some()
                && sealed_evidence.is_some())
    {
        return Err(WorkAttemptStorageError::Unavailable);
    }
    if let (TokenState::Sealed, Some(expected_digest), Some(evidence), Some(source_ref)) = (
        state,
        sealed_evidence_digest.as_ref(),
        sealed_evidence.as_ref(),
        source_ref.as_ref(),
    ) {
        evidence
            .validate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let expected_source = WorkRetryEvidenceBindingSourceV1::test(evidence.operation_id.clone())
            .map_err(|_| WorkAttemptStorageError::Unavailable)?
            .selector()
            .evidence_ref;
        let observed_digest = canonical_sha256(&(TEST_EVIDENCE_DIGEST_DOMAIN_V1, evidence))
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if &evidence.token != &token
            || source_ref != &expected_source
            || expected_digest != &observed_digest
        {
            return Err(WorkAttemptStorageError::Unavailable);
        }
    }
    Ok(StoredTokenV1 {
        token,
        original_attempt,
        minted_at: UtcMicros(minted_at),
        state,
        source_ref,
        sealed_evidence_digest,
        sealed_evidence,
    })
}

fn replay_sealed_terminal(
    query: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    source_ref: &str,
    digest: &ManifestDigest,
    failure: bool,
) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError> {
    let binding = load_by_source(query, authority, source_ref)?;
    match (failure, binding) {
        (true, Some(binding)) if binding.evidence_digest() == digest => Ok(Some(binding)),
        (false, None) => Ok(None),
        _ => Err(WorkAttemptStorageError::AttemptConflict),
    }
}

fn insert_binding(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    binding: &WorkRetryEvidenceBindingV1,
) -> Result<(), WorkAttemptStorageError> {
    let payload =
        serde_json::to_string(binding).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_retry_evidence_bindings_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    source_kind, source_ref, token_id, task_id, run_id, attempt_id,
                    evidence_digest, observed_at, binding_payload
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, 'test', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
                 )",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(binding.selector().evidence_ref.clone()),
                        ExactSqlValue::Text(binding.token().as_str().to_owned()),
                        ExactSqlValue::Text(
                            binding.original_attempt().task_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Text(
                            binding.original_attempt().run_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Text(
                            binding.original_attempt().attempt_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Text(binding.evidence_digest().as_str().to_owned()),
                        ExactSqlValue::Integer(binding.observed_at().0),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?,
        )
        .map_err(|_| WorkAttemptStorageError::AttemptConflict)?;
    Ok(())
}

fn require_terminal_non_success_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<(), WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT terminal, attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Err(WorkAttemptStorageError::NotFoundOrNotAuthorized);
    };
    if exact_sql_integer(&row.values, 0) != Some(1) {
        return Err(WorkAttemptStorageError::AttemptConflict);
    }
    let stored = serde_json::from_str::<StoredWorkAttemptV1>(
        exact_sql_text(&row.values, 1).ok_or(WorkAttemptStorageError::Unavailable)?,
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    matches!(
        stored.attempt.terminal(),
        Some(
            WorkTerminalEvidenceV1::Failed { .. }
                | WorkTerminalEvidenceV1::TimedOut { .. }
                | WorkTerminalEvidenceV1::Cancelled { .. }
        )
    )
    .then_some(())
    .ok_or(WorkAttemptStorageError::AttemptConflict)
}

fn load_by_source<T>(
    query: &T,
    authority: &WorkAuthority,
    source_ref: &str,
) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError>
where
    T: RegisteredWorkQuery,
{
    let rows = registered_work_query(
        query,
        "SELECT source_ref, token_id, task_id, run_id, attempt_id, evidence_digest,
                observed_at, binding_payload
         FROM work_retry_evidence_bindings_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND source_kind = 'test' AND source_ref = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(source_ref.to_owned())])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let binding = serde_json::from_str::<WorkRetryEvidenceBindingV1>(
                exact_sql_text(&row.values, 7).ok_or(WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            binding
                .validate()
                .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            if exact_sql_text(&row.values, 0) != Some(binding.selector().evidence_ref.as_str())
                || exact_sql_text(&row.values, 1) != Some(binding.token().as_str())
                || exact_sql_text(&row.values, 2)
                    != Some(binding.original_attempt().task_id().as_str())
                || exact_sql_text(&row.values, 3)
                    != Some(binding.original_attempt().run_id().as_str())
                || exact_sql_text(&row.values, 4)
                    != Some(binding.original_attempt().attempt_id().as_str())
                || exact_sql_text(&row.values, 5) != Some(binding.evidence_digest().as_str())
                || exact_sql_integer(&row.values, 6) != Some(binding.observed_at().0)
            {
                return Err(WorkAttemptStorageError::Unavailable);
            }
            Ok(binding)
        })
        .transpose()
}

fn identity_params(identity: &WorkAttemptIdentityV1) -> [ExactSqlValue; 3] {
    [
        ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
    ]
}
