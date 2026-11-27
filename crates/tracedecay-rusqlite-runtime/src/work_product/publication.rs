//! Verified Work product graph persistence inside the event transaction.
//!
//! The event port recovers the graph the new event produces, digests it, and
//! records the version before committing either row. There is no
//! independently callable publication or restart-reconciliation authority.

use tracedecay_contracts::{
    VerifiedWorkGraphVersionV1, WorkProductEventPortErrorV1, WorkProductPortContextV1,
};
use tracedecay_domain::{WorkProductEventPayloadV1, WorkProductEventV1, WorkProductGraphV1};

use super::{
    CHECKPOINT_INTERVAL, ChainHeadV1, FromStoreFault, VersionLineV1, checkpoint,
    fold_chain_checkpointed, insert_fault, line_params, recovered_graph_digest, selection_covers,
};
use crate::exact_sql::ExactSqlValue;
use crate::work::{RegisteredWorkWrite, exact_sql_statement};

type PortError = WorkProductEventPortErrorV1;

/// Publish `event`, just inserted on its selection's own line, and answer the
/// verified version and the graph it recovered.
///
/// `extends` is the chain head the writer's compare-and-swap matched, paired
/// with the verified identity the draft was written against; `None` only for
/// a creation. The head's chain is folded from that line's newest checkpoint,
/// so the writer reads the events since it and never the retained journal.
/// The head it recovers must be exactly the identity the caller read, digest
/// included: the same sequence and version under another digest is history
/// that changed under the caller, and is refused as stale.
pub(super) fn publish_in_transaction(
    transaction: &impl RegisteredWorkWrite,
    context: &WorkProductPortContextV1,
    event: &WorkProductEventV1,
    extends: Option<(&ChainHeadV1, &VerifiedWorkGraphVersionV1)>,
) -> Result<(VerifiedWorkGraphVersionV1, WorkProductGraphV1), PortError> {
    let scope = context.authorized_scope();
    if !selection_covers(scope.selection(), event) {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    // An observation earlier than the change it verifies would make forensic
    // and as-of reads disagree, so the entire event transaction is refused.
    if context.observed_at() < event.occurred_at() {
        return Err(PortError::Unavailable);
    }
    let line = VersionLineV1::of_event(event).map_err(PortError::from_fault)?;
    let invalidated_from = checkpoint::invalidated_from(transaction, scope, event.sequence())
        .map_err(PortError::from_fault)?;
    let (graph, own_since_base) = match (extends, event.payload()) {
        (None, WorkProductEventPayloadV1::Created { graph }) => (graph.clone(), 1),
        (Some((head, expected)), WorkProductEventPayloadV1::Changed { change }) => {
            let base = fold_chain_checkpointed(transaction, scope, head, invalidated_from)
                .map_err(PortError::from_fault)?
                .ok_or(PortError::Unavailable)?;
            if base.head_digest != expected.recovered_graph_digest().as_str() {
                return Err(PortError::VersionConflict);
            }
            // The change was validated against this exact graph, by digest,
            // so a refusal here is a store that cannot be trusted, never stale
            // input.
            let graph = base
                .graph
                .apply(change.as_ref().clone())
                .map_err(|_| PortError::Unavailable)?;
            // A first event roots its line, so only an event extending its
            // own line's head carries that line's suffix forward.
            let own_since_base = if head.line.key() == line.key() {
                base.own_since_base + 1
            } else {
                1
            };
            (graph, own_since_base)
        }
        _ => return Err(PortError::Unavailable),
    };
    if graph.version() != event.result_graph_version() {
        return Err(PortError::VersionConflict);
    }
    let digest = recovered_graph_digest(&graph).ok_or(PortError::Unavailable)?;

    transaction
        .work_execute(
            exact_sql_statement(
                "INSERT INTO work_product_graph_versions_v1 (
                    owner_brain_id, owner_profile_id, selection_key, graph_version,
                    event_sequence, valid_at, observed_at, source_watermark,
                    recovered_graph_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                line_params(scope, &line)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(
                            i64::try_from(event.result_graph_version().get())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(
                            i64::try_from(event.sequence().get())
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
            .map_err(PortError::from_fault)?,
        )
        .map_err(|error| insert_fault(error, PortError::VersionConflict))?;

    if own_since_base >= CHECKPOINT_INTERVAL {
        checkpoint::write(transaction, scope, &line, event.sequence(), &graph, &digest)
            .map_err(PortError::from_fault)?;
    }
    if let Some(from) = invalidated_from {
        checkpoint::clear_invalidation(transaction, scope, from).map_err(PortError::from_fault)?;
    }

    let verified = VerifiedWorkGraphVersionV1::new(
        event.result_graph_version(),
        event.sequence(),
        event.source_watermark().clone(),
        digest,
    )
    .map_err(|_| PortError::Unavailable)?;
    Ok((verified, graph))
}
