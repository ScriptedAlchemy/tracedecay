//! Verified Work product graph publication.
//!
//! Publication is what turns an appended event into something a read may serve.
//! It folds the journal through that event, digests the recovered graph, and
//! records the version. A read that finds no published row for a version
//! reports the absence — it never falls back to folding unpublished events,
//! because an unverified fold is exactly the falsified reading the Work views
//! must never draw.

use tracedecay_application::{
    VerifiedWorkGraphVersionV1, WorkGraphPublishPortErrorV1, WorkGraphPublishPortV1,
    WorkProductPortContextV1,
};
use tracedecay_domain::WorkProductEventV1;

use super::{fold_graph, load_journal, owner_params, recovered_graph_digest, selection_covers};
use crate::exact_sql::ExactSqlValue;
use crate::work::{WorkSqliteStorage, exact_sql_statement, exact_sql_text, registered_work_query};

type PortError = WorkGraphPublishPortErrorV1;

impl WorkGraphPublishPortV1 for WorkSqliteStorage {
    fn publish_event(
        &self,
        context: &WorkProductPortContextV1,
        event: &WorkProductEventV1,
    ) -> Result<VerifiedWorkGraphVersionV1, PortError> {
        let scope = context.authorized_scope();
        if !selection_covers(scope.selection(), event) {
            return Err(PortError::Unavailable);
        }
        // An observation earlier than the change it publishes is not a state
        // this authority can record: `valid_at <= observed_at` is a schema
        // invariant, and silently widening it would make the forensic and
        // as-of readings disagree.
        if context.observed_at() < event.occurred_at() {
            return Err(PortError::Unavailable);
        }

        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| PortError::Unavailable)?;

        let outcome = publish_in_transaction(&transaction, context, event);
        match outcome {
            Ok(verified) => {
                transaction.commit().map_err(|_| PortError::Unavailable)?;
                Ok(verified)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }
}

pub(super) fn publish_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    context: &WorkProductPortContextV1,
    event: &WorkProductEventV1,
) -> Result<VerifiedWorkGraphVersionV1, PortError> {
    let scope = context.authorized_scope();
    let journal = load_journal(transaction, scope).ok_or(PortError::Unavailable)?;
    let entry = journal
        .iter()
        .find(|entry| entry.event.event_id() == event.event_id())
        .ok_or(PortError::Unavailable)?;
    // Publishing an event whose stored bytes differ from the caller's copy
    // would digest a graph nobody appended.
    if entry.event != *event {
        return Err(PortError::VersionConflict);
    }
    let graph = fold_graph(&journal, entry.sequence).ok_or(PortError::Unavailable)?;
    if graph.version() != event.result_graph_version() {
        return Err(PortError::VersionConflict);
    }
    let digest = recovered_graph_digest(&graph).ok_or(PortError::Unavailable)?;

    let existing = registered_work_query(
        transaction,
        "SELECT recovered_graph_digest FROM work_product_graph_versions_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND graph_version = ?3",
        owner_params(scope)
            .into_iter()
            .chain([ExactSqlValue::Integer(
                i64::try_from(event.result_graph_version().get())
                    .map_err(|_| PortError::Unavailable)?,
            )])
            .collect(),
    )
    .map_err(|_| PortError::Unavailable)?;
    if let Some(row) = existing.rows.first() {
        // Republishing is idempotent when it recovers the identical graph and
        // a conflict otherwise: two different graphs at one version is the
        // state this authority exists to make impossible.
        let stored = exact_sql_text(&row.values, 0).ok_or(PortError::Unavailable)?;
        if stored != digest.as_str() {
            return Err(PortError::VersionConflict);
        }
        let published = super::load_published_versions(transaction, scope)
            .ok_or(PortError::Unavailable)?
            .into_iter()
            .find(|published| published.graph_version == event.result_graph_version())
            .ok_or(PortError::Unavailable)?;
        return super::verified_version(&published, event).ok_or(PortError::Unavailable);
    }

    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_product_graph_versions_v1 (
                    owner_brain_id, owner_profile_id, graph_version, event_sequence,
                    valid_at, observed_at, source_watermark, recovered_graph_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                owner_params(scope)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(
                            i64::try_from(event.result_graph_version().get())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(
                            i64::try_from(entry.sequence.get())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(event.occurred_at().0),
                        ExactSqlValue::Integer(context.observed_at().0),
                        ExactSqlValue::Text(
                            serde_json::to_string(event.source_watermark())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(digest.as_str().to_owned()),
                    ])
                    .collect(),
            )
            .map_err(|_| PortError::Unavailable)?,
        )
        .map_err(|_| PortError::VersionConflict)?;

    transaction
        .execute(
            exact_sql_statement(
                "UPDATE work_product_event_outbox_v1
                 SET published_at = ?4
                 WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND sequence = ?3
                   AND published_at IS NULL",
                owner_params(scope)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(
                            i64::try_from(entry.sequence.get())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(context.observed_at().0),
                    ])
                    .collect(),
            )
            .map_err(|_| PortError::Unavailable)?,
        )
        .map_err(|_| PortError::DurabilityUncertain)?;

    VerifiedWorkGraphVersionV1::new(
        event.result_graph_version(),
        entry.sequence,
        event.source_watermark().clone(),
        digest,
    )
    .map_err(|_| PortError::Unavailable)
}
