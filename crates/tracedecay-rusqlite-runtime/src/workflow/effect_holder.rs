//! Exact-root pending workflow-effect holder read.

use std::time::Duration;

use tracedecay_application::{WorkflowEffectAuthorityErrorV1, WorkflowEffectIdentityV1};
use tracedecay_domain::WorktreeId;

use crate::exact_sql::{ExactSqlHandle, ExactSqlStatement};

use super::{
    decode_json, sql_text, workflow_effect_codec_unavailable, workflow_effect_unavailable,
};

pub(super) fn has_pending_effects(
    storage: &ExactSqlHandle,
    worktree_id: &WorktreeId,
) -> Result<bool, WorkflowEffectAuthorityErrorV1> {
    let statement = ExactSqlStatement::new(
        "SELECT identity_payload FROM workflow_effect_journal
         WHERE state IN ('before_effect', 'in_flight')
         ORDER BY idempotency_key LIMIT 1025"
            .to_owned(),
        Vec::new(),
    )
    .map_err(|_| workflow_effect_codec_unavailable())?;
    let rows = storage
        .query(statement, Duration::from_secs(5))
        .map_err(workflow_effect_unavailable)?;
    if rows.rows.len() > 1024 {
        return Err(workflow_effect_codec_unavailable());
    }
    rows.rows.iter().try_fold(false, |matched, row| {
        let payload = sql_text(&row.values, 0).ok_or_else(workflow_effect_codec_unavailable)?;
        let identity: WorkflowEffectIdentityV1 =
            decode_json(payload).map_err(|_| workflow_effect_codec_unavailable())?;
        Ok(matched || &identity.scope().worktree_id == worktree_id)
    })
}
