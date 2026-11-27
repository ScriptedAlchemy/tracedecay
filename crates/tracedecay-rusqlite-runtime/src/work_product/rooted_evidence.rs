//! Exact TaskId-rooted projection from an immutable verified Work graph.

use tracedecay_contracts::{
    VerifiedWorkEvidenceRootV1, VerifiedWorkGraphVersionV1, WorkEvidenceRootReadErrorV1,
    WorkEvidenceRootReadPortV1, WorkProductPortContextV1,
};
use tracedecay_domain::{TaskId, WorkProductGraphV1};

use super::{FromStoreFault, fold_graph, selection_chain, verified_version};
use crate::work::WorkSqliteStorage;

impl WorkEvidenceRootReadPortV1 for WorkSqliteStorage {
    fn read_evidence_root(
        &self,
        context: &WorkProductPortContextV1,
        task_id: &TaskId,
        requested: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1> {
        let (verified_version, graph) = verified_graph(self, context, requested)?;
        let item = graph
            .item(task_id)
            .cloned()
            .ok_or(WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized)?;
        let mut links = graph
            .evidence()
            .iter()
            .filter(|link| link.task_id() == task_id)
            .cloned()
            .collect::<Vec<_>>();
        links.sort_by(|left, right| left.link_id().cmp(right.link_id()));
        let relations = graph
            .relations()
            .into_iter()
            .filter(|relation| relation.touches_task(task_id))
            .collect();
        let proposal_decisions = graph
            .proposal_decisions()
            .iter()
            .filter(|decision| decision.proposal().task_id() == task_id)
            .cloned()
            .collect();
        let relation_replan_decisions = graph
            .relation_replan_decisions()
            .iter()
            .filter(|decision| &decision.proposal.task_id == task_id)
            .cloned()
            .collect();
        Ok(VerifiedWorkEvidenceRootV1 {
            verified_version,
            item,
            relations,
            proposal_decisions,
            relation_replan_decisions,
            links,
        })
    }
}

/// Resolve the exact published graph identity named by a rooted retrieval.
/// The helper lives with the single mounted reader so no legacy evidence port
/// remains an authority over the same journal.
fn verified_graph(
    storage: &WorkSqliteStorage,
    context: &WorkProductPortContextV1,
    requested: &VerifiedWorkGraphVersionV1,
) -> Result<(VerifiedWorkGraphVersionV1, WorkProductGraphV1), WorkEvidenceRootReadErrorV1> {
    let scope = context.authorized_scope();
    // Resolve the selection's chain through the requested event. Later writes
    // cannot change that exact version's ancestry, even at the same timestamp.
    // Scope containment and the complete verified identity still fence the read.
    let chain = selection_chain(
        storage.handle(),
        scope,
        context.observed_at(),
        Some(requested.event_sequence()),
    )
    .map_err(WorkEvidenceRootReadErrorV1::from_fault)?
    .ok_or(WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized)?;
    let (folded, published) = (chain.entries, chain.published);
    let Some(version) = published
        .iter()
        .find(|version| version.graph_version == requested.graph_version())
    else {
        return Err(
            if published
                .iter()
                .any(|version| version.graph_version.get() > requested.graph_version().get())
            {
                WorkEvidenceRootReadErrorV1::Stale
            } else {
                WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized
            },
        );
    };
    let entry = folded
        .iter()
        .find(|entry| entry.sequence == version.event_sequence)
        .ok_or(WorkEvidenceRootReadErrorV1::Unavailable)?;
    let graph = fold_graph(&folded, version.event_sequence)
        .ok_or(WorkEvidenceRootReadErrorV1::Unavailable)?;
    if graph.version() != version.graph_version {
        return Err(WorkEvidenceRootReadErrorV1::Unavailable);
    }
    let verified =
        verified_version(version, &entry.event).ok_or(WorkEvidenceRootReadErrorV1::Unavailable)?;
    if verified != *requested {
        return Err(WorkEvidenceRootReadErrorV1::Stale);
    }
    Ok((verified, graph))
}
