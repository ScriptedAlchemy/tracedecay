//! Exact sealed-attempt receipt lookup for TaskId-rooted evidence composition.

use tracedecay_application::{
    WorkAttemptReceiptReadErrorV1, WorkAttemptReceiptReadPortV1, WorkAttemptReceiptV1,
};
use tracedecay_domain::{WorkAttemptIdentityV1, WorkAuthority};

use super::{StoredWorkAttemptV1, identity_params};
use crate::exact_sql::ExactSqlValue;
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_text, registered_work_query,
};

impl WorkAttemptReceiptReadPortV1 for WorkSqliteStorage {
    fn attempt_receipt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptReceiptV1, WorkAttemptReceiptReadErrorV1> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT attempt_payload, evidence_payload
             FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
            authority_params_owned(authority)
                .into_iter()
                .chain(identity_params(identity))
                .collect::<Vec<ExactSqlValue>>(),
        )
        .map_err(|_| WorkAttemptReceiptReadErrorV1::Unavailable)?;
        let row = rows
            .rows
            .first()
            .ok_or(WorkAttemptReceiptReadErrorV1::NotFoundOrNotAuthorized)?;
        let attempt_payload =
            exact_sql_text(&row.values, 0).ok_or(WorkAttemptReceiptReadErrorV1::Unavailable)?;
        let attempt = serde_json::from_str::<StoredWorkAttemptV1>(attempt_payload)
            .map_err(|_| WorkAttemptReceiptReadErrorV1::Unavailable)?
            .attempt;
        if attempt.identity() != identity {
            return Err(WorkAttemptReceiptReadErrorV1::Unavailable);
        }
        let evidence = exact_sql_text(&row.values, 1)
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| WorkAttemptReceiptReadErrorV1::Unavailable)?;
        if evidence.as_ref().is_some_and(
            |record: &tracedecay_application::WorkAttemptEvidenceRecordV1| {
                &record.identity != identity
            },
        ) {
            return Err(WorkAttemptReceiptReadErrorV1::Unavailable);
        }
        match (attempt.terminal(), evidence.as_ref()) {
            (None, None) => {}
            (Some(terminal), Some(evidence)) => {
                let sealed = terminal
                    .runtime_evidence_ref(identity.run_id().clone())
                    .map_err(|_| WorkAttemptReceiptReadErrorV1::Unavailable)?;
                let digest = evidence
                    .digest()
                    .map_err(|_| WorkAttemptReceiptReadErrorV1::Unavailable)?;
                if sealed.evidence_digest() != &digest {
                    return Err(WorkAttemptReceiptReadErrorV1::Unavailable);
                }
            }
            (None, Some(_)) | (Some(_), None) => {
                return Err(WorkAttemptReceiptReadErrorV1::Unavailable);
            }
        }
        Ok(WorkAttemptReceiptV1 {
            identity: identity.clone(),
            artifacts: attempt.artifacts().to_vec(),
            evidence,
        })
    }
}
