//! Exact TaskId-rooted projection from an immutable verified Work graph.

use tracedecay_contracts::{
    VerifiedWorkEvidenceRootV1, VerifiedWorkGraphVersionV1, WorkEvidenceRootReadErrorV1,
    WorkEvidenceRootReadPortV1, WorkProductPortContextV1,
};
use tracedecay_domain::{TaskId, WorkProductGraphV1};

use super::FromStoreFault;
use super::journal::{
    NamedVersionV1, covered_prefix_len, require_current_journal, resolve_named_version,
};
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
    let source = storage.handle();
    // Resolved inside the selection's own aggregate, exactly as the graph read
    // is.
    require_current_journal(source).map_err(WorkEvidenceRootReadErrorV1::from_fault)?;
    let covered =
        covered_prefix_len(source, scope).map_err(WorkEvidenceRootReadErrorV1::from_fault)?;
    match resolve_named_version(source, scope, covered, requested)
        .map_err(WorkEvidenceRootReadErrorV1::from_fault)?
    {
        NamedVersionV1::Found(found) => Ok(*found),
        NamedVersionV1::Stale => Err(WorkEvidenceRootReadErrorV1::Stale),
        NamedVersionV1::Absent => Err(WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized),
    }
}
