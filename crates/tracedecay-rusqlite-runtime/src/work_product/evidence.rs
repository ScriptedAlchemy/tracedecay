//! Task evidence selection and expansion, served from the same verified graph
//! version the caller named.
//!
//! Evidence is not a second store. `WorkGraphChangeV1::EvidenceLinked` and
//! `WorkGraphChangeV1::AcceptedAttemptLinked` write a `TaskEvidenceLinkV1` into
//! the journal, and folding the journal to a published version reproduces
//! exactly the links that version declared. So this authority reads evidence
//! the same way it reads a graph: fold, verify, answer. It never joins an
//! attempt row, a retrieval anchor row, or any other authority's table to
//! enrich a link.
//!
//! Two consequences are worth stating, because both are absences that must not
//! be dressed up as data.
//!
//! ## The version must be the one that was asked for
//!
//! Every request carries a `VerifiedWorkGraphVersionV1`, and the answer is that
//! version — never "whatever is current", which would silently swap the
//! caller's question. An older published version is therefore answered as
//! itself: verified versions are retained, and reading one is a temporal read,
//! not a stale one. What is refused is an identity this authority did not
//! verify. The identity is rebuilt from the published row and the journaled
//! event, and a byte-for-byte disagreement at a version this authority does
//! know is reported as staleness; a version it never published is an absence,
//! unless a higher version exists, in which case the caller is behind and is
//! told that instead.
//!
//! ## Expansion returns a handle, and says so
//!
//! A journaled link names a `RetrievalAnchorId` and an evidence digest. It does
//! not carry content, and this authority owns no retrieval or disclosure store
//! it could read content from — the anchor rows live under the repository
//! observation authority, which is scoped by project and repository, not by the
//! registered profile owner this journal is keyed on. So an expansion returns
//! the anchor id as the content handle and reports `redacted`: the content
//! behind the handle was NOT disclosed here. Reporting an undisclosed
//! expansion as unredacted would claim a disclosure that never happened.
//! When a retrieval authority that can prove the correspondence lands, it
//! supplies content here and the flag becomes an observation instead of a
//! standing non-disclosure, without any other shape changing.

use std::collections::BTreeSet;

use tracedecay_application::{
    SelectedWorkEvidenceV1, VerifiedWorkEvidenceExpansionV1, VerifiedWorkGraphVersionV1,
    WorkEvidenceExpandRequestV1, WorkEvidenceExpansionV1, WorkEvidenceReadPortErrorV1,
    WorkEvidenceReadPortV1, WorkEvidenceSelectRequestV1, WorkProductPortContextV1,
};
use tracedecay_domain::{
    TaskEvidenceLinkV1, TaskId, WorkProductGraphV1, WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1,
};

use super::{
    fold_graph, load_journal, load_published_versions, selection_covers, verified_version,
};
use crate::work::WorkSqliteStorage;

type PortError = WorkEvidenceReadPortErrorV1;

/// The named absence a bounded selection reports: links this task has that the
/// caller's own limit kept out of the answer. It is a truncation the caller
/// caused, never a link this authority failed to find.
const TRUNCATED_BY_LIMIT_UNKNOWN: &str = "work-product-evidence-links-beyond-requested-limit";

impl WorkEvidenceReadPortV1 for WorkSqliteStorage {
    fn select_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceSelectRequestV1,
    ) -> Result<SelectedWorkEvidenceV1, PortError> {
        let (verified, graph) = verified_graph(self, context, &request.verified_version)?;
        // A task the version never declared has no evidence to be empty about.
        if graph.item(&request.task_id).is_none() {
            return Err(PortError::NotFoundOrNotAuthorized);
        }
        let mut links = task_links(&graph, &request.task_id);
        let available = u32::try_from(links.len()).map_err(|_| PortError::Unavailable)?;
        let limit = usize::try_from(request.limit).map_err(|_| PortError::Unavailable)?;
        let coverage = if links.len() <= limit {
            WorkTaskEvidenceCoverageV1::Complete {
                returned: available,
                available,
            }
        } else {
            links.truncate(limit);
            WorkTaskEvidenceCoverageV1::Partial {
                returned: request.limit,
                available,
                unknowns: BTreeSet::from([TRUNCATED_BY_LIMIT_UNKNOWN.to_owned()]),
            }
        };
        let evidence = WorkTaskEvidenceV1::new(
            request.task_id.clone(),
            verified.graph_version(),
            links,
            coverage,
        )
        .map_err(|_| PortError::Unavailable)?;
        Ok(SelectedWorkEvidenceV1 {
            verified_version: verified,
            evidence,
        })
    }

    fn expand_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceExpandRequestV1,
    ) -> Result<VerifiedWorkEvidenceExpansionV1, PortError> {
        let (verified, graph) = verified_graph(self, context, &request.verified_version)?;
        let link = graph
            .evidence()
            .iter()
            .find(|link| link.link_id() == &request.link_id && link.task_id() == &request.task_id)
            .cloned()
            .ok_or(PortError::NotFoundOrNotAuthorized)?;
        // See the module documentation: the handle is the journaled anchor id,
        // and the content behind it is not disclosed by this authority.
        let expansion = WorkEvidenceExpansionV1::new(
            link.clone(),
            link.anchor_id().as_str().to_owned(),
            true,
            request.observed_at,
        )
        .map_err(|_| PortError::Unavailable)?;
        Ok(VerifiedWorkEvidenceExpansionV1 {
            verified_version: verified,
            expansion,
        })
    }
}

/// Every link the folded version declares for one task, in canonical link-id
/// order so a bounded page is a stable prefix rather than an arbitrary subset.
fn task_links(graph: &WorkProductGraphV1, task_id: &TaskId) -> Vec<TaskEvidenceLinkV1> {
    let mut links = graph
        .evidence()
        .iter()
        .filter(|link| link.task_id() == task_id)
        .cloned()
        .collect::<Vec<_>>();
    links.sort_by(|left, right| left.link_id().cmp(right.link_id()));
    links
}

/// Resolve the exact verified version the caller named, and the graph folded to
/// it.
///
/// The selection must cover the whole journal for the same reason a graph read
/// requires it: a fold over part of the history is a graph that never existed,
/// and answering "no evidence" would conceal a task the caller simply is not
/// authorized for.
fn verified_graph(
    storage: &WorkSqliteStorage,
    context: &WorkProductPortContextV1,
    requested: &VerifiedWorkGraphVersionV1,
) -> Result<(VerifiedWorkGraphVersionV1, WorkProductGraphV1), PortError> {
    let scope = context.authorized_scope();
    let journal = load_journal(&storage.handle, scope).ok_or(PortError::Unavailable)?;
    if journal
        .iter()
        .any(|entry| !selection_covers(scope.selection(), &entry.event))
    {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    let published =
        load_published_versions(&storage.handle, scope).ok_or(PortError::Unavailable)?;
    let Some(version) = published
        .iter()
        .find(|version| version.graph_version == requested.graph_version())
    else {
        // A version this authority never published is an absence. A version
        // the caller is behind is staleness. They are not the same answer.
        return Err(
            if published
                .iter()
                .any(|version| version.graph_version.get() > requested.graph_version().get())
            {
                PortError::Stale
            } else {
                PortError::NotFoundOrNotAuthorized
            },
        );
    };
    let entry = journal
        .iter()
        .find(|entry| entry.sequence == version.event_sequence)
        .ok_or(PortError::Unavailable)?;
    let graph = fold_graph(&journal, version.event_sequence).ok_or(PortError::Unavailable)?;
    if graph.version() != version.graph_version {
        return Err(PortError::Unavailable);
    }
    let verified = verified_version(version, &entry.event).ok_or(PortError::Unavailable)?;
    // The same version number under a different verified identity is a
    // different reading of history, so the caller's identity is honoured
    // exactly rather than reconciled to this one.
    if verified != *requested {
        return Err(PortError::Stale);
    }
    Ok((verified, graph))
}
