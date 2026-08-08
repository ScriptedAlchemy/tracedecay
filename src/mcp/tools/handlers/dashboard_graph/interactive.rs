//! Scope verification and interactive-projection neighborhood reads.
//!
//! Two responsibilities share this file because they sit on either side of
//! the same request path in `read_inner`: [`verify_scope`] is the exact-scope
//! denial check every dispatched operation runs first, and
//! [`interactive_neighborhood`] (with its `neighbor_node` hydration helper)
//! is the neighbor read that follows once scope is verified. The neighbor
//! read walks the verified code graph projection's generation-pinned
//! interactive reader rather than the relational `edges` table — once a
//! focus symbol resolves into the projection, the projection is the sole
//! caller/callee/degree adjacency authority; the relational node rows still
//! supply identity (id, path, span) for hydration until the id-space
//! cutover lands.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::{DashboardGraphNodeV1, DashboardGraphReadErrorV1, ResolvedScope};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;

use super::read_models::{decode_node, unavailable};

/// A read for a foreign exact scope is concealed behind the typed denial —
/// the adapter serves exactly one registered project/repository/worktree.
pub(super) fn verify_scope(
    own: &ResolvedScope,
    requested: &ResolvedScope,
) -> Result<(), DashboardGraphReadErrorV1> {
    if own.project_id == requested.project_id
        && own.repository_id == requested.repository_id
        && own.worktree_id == requested.worktree_id
    {
        Ok(())
    } else {
        Err(DashboardGraphReadErrorV1::Denied)
    }
}

/// Candidate cap when resolving a focus row's qualified name against the
/// projection catalog; more same-name overloads than this means the name is
/// not a usable interactive key.
const NEIGHBOR_RESOLVE_CANDIDATES: usize = 16;

/// The HTTP read path carries no per-request cancellation signal; the store
/// lifecycle cancellation the reader was assembled with still applies.
pub(super) struct UnsignalledRead;

impl GraphCancellation for UnsignalledRead {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Adjacency bundle for one focus symbol, read entirely from the verified
/// code graph projection.
pub(super) struct InteractiveNeighborhoodV1 {
    pub(super) callers: Vec<CodeGraphSemanticEdgeV1>,
    pub(super) callees: Vec<CodeGraphSemanticEdgeV1>,
    pub(super) edges_by_kind: Vec<(RelationEdgeKindV1, u64)>,
    pub(super) degrees: BTreeMap<SymbolOccurrenceId, i64>,
}

pub(super) fn interactive_neighborhood(
    reader: &CodeGraphInteractiveReader,
    qualified_name: &str,
    kind: &str,
    max_relations: usize,
) -> Result<InteractiveNeighborhoodV1, DashboardGraphReadErrorV1> {
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(UnsignalledRead);
    let candidates = reader
        .resolve_qualified_name(
            qualified_name,
            None,
            NEIGHBOR_RESOLVE_CANDIDATES,
            Arc::clone(&cancellation),
        )
        .map_err(|error| unavailable(error.to_string()))?;
    let focus = candidates
        .iter()
        .find(|candidate| {
            candidate
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind == kind)
        })
        .or_else(|| candidates.first())
        .map(|candidate| candidate.occurrence.clone())
        .ok_or_else(|| {
            unavailable(format!(
                "symbol {qualified_name:?} is not present in the published code graph generation"
            ))
        })?;
    let seeds = [focus.clone()];
    let callers = single_seed_batch(
        reader
            .callers(&seeds, &[], max_relations, Arc::clone(&cancellation))
            .map_err(|error| unavailable(error.to_string()))?,
    )?;
    let callees = single_seed_batch(
        reader
            .callees(&seeds, &[], max_relations, Arc::clone(&cancellation))
            .map_err(|error| unavailable(error.to_string()))?,
    )?;
    let counts = reader
        .edge_kind_counts(&focus, Arc::clone(&cancellation))
        .map_err(|error| unavailable(error.to_string()))?;
    let mut merged: BTreeMap<RelationEdgeKindV1, u64> = counts.outgoing;
    for (kind, count) in counts.incoming {
        *merged.entry(kind).or_default() += count;
    }
    let mut occurrences: BTreeSet<SymbolOccurrenceId> = BTreeSet::new();
    for edge in callers.iter().chain(callees.iter()) {
        occurrences.insert(edge.neighbor.occurrence.clone());
    }
    let occurrence_list: Vec<SymbolOccurrenceId> = occurrences.into_iter().collect();
    let mut degrees: BTreeMap<SymbolOccurrenceId, i64> = BTreeMap::new();
    if !occurrence_list.is_empty() {
        for entry in reader
            .degrees(&occurrence_list, cancellation)
            .map_err(|error| unavailable(error.to_string()))?
        {
            let total = entry.outgoing.saturating_add(entry.incoming);
            degrees.insert(entry.occurrence, i64::try_from(total).unwrap_or(i64::MAX));
        }
    }
    Ok(InteractiveNeighborhoodV1 {
        callers,
        callees,
        edges_by_kind: merged.into_iter().collect(),
        degrees,
    })
}

/// One-seed adjacency batches must come back with exactly one batch.
fn single_seed_batch(
    mut batches: Vec<Vec<CodeGraphSemanticEdgeV1>>,
) -> Result<Vec<CodeGraphSemanticEdgeV1>, DashboardGraphReadErrorV1> {
    if batches.len() != 1 {
        return Err(DashboardGraphReadErrorV1::Corrupt {
            detail: format!(
                "interactive adjacency returned {} batches for one seed",
                batches.len()
            ),
        });
    }
    Ok(batches.remove(0))
}

/// Hydrates one projection neighbor. Prefers the relational node row (same
/// id-space as the not-yet-cut Search/Node operations); a symbol the node
/// index does not know is served as projection truth keyed by its
/// occurrence, never dropped.
pub(super) fn neighbor_node(
    summary: &CodeGraphSymbolSummaryV1,
    rows_by_identity: &BTreeMap<(String, String), Vec<Value>>,
) -> Result<DashboardGraphNodeV1, DashboardGraphReadErrorV1> {
    if let Some(metadata) = summary.metadata.as_ref() {
        let key = (metadata.qualified_name.clone(), metadata.kind.clone());
        match rows_by_identity.get(&key).map(Vec::as_slice) {
            Some([row]) => return decode_node(row.clone()),
            Some(rows) if rows.len() > 1 => {
                // More than one relational row answers this projected symbol's
                // (qualified name, kind). The projection distinguishes them by
                // file identity, but the node table is keyed by path and the
                // two are not joinable, so no correct row can be chosen here.
                // Refuse rather than pick: a silently-picked row serves the
                // wrong symbol's wire id. Resolving this needs the pending
                // `nodes.symbol_occurrence_id` column, which supersedes the
                // qualified-name key with a direct occurrence join.
                return Err(DashboardGraphReadErrorV1::Corrupt {
                    detail: format!(
                        "the node index holds {} rows for qualified name {:?} of kind {:?}; \
                         the qualified-name key cannot identify one and the direct \
                         occurrence join is not yet available",
                        rows.len(),
                        metadata.qualified_name,
                        metadata.kind,
                    ),
                });
            }
            _ => {}
        }
    }
    let metadata = summary.metadata.as_ref();
    Ok(DashboardGraphNodeV1 {
        id: summary.occurrence.as_str().to_owned(),
        kind: metadata.map(|m| m.kind.clone()).unwrap_or_default(),
        name: metadata.map(|m| {
            m.qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(m.qualified_name.as_str())
                .to_owned()
        }),
        qualified_name: metadata.map(|m| m.qualified_name.clone()),
        file_path: None,
        start_line: None,
        end_line: None,
        start_column: None,
        end_column: None,
        attrs_start_line: None,
        doc: None,
        signature: None,
        visibility: None,
        is_async: None,
        branches: None,
        loops: None,
        returns: None,
        max_nesting: None,
        unsafe_blocks: None,
        unchecked_calls: None,
        assertions: None,
        updated_at: None,
        parent_id: None,
        degree: None,
        span: None,
        edge_kind: None,
        edge_line: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
    use tracedecay_graph_db::GraphDbError;

    use super::super::TopologyWatermark;
    use super::super::read_models::map_graph_error;

    fn scope(project: &str, repository: &str, worktree: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(project).expect("project id"),
            RepositoryId::new(repository).expect("repository id"),
            WorktreeId::new(worktree).expect("worktree id"),
            None,
        )
        .expect("resolved scope")
    }

    #[test]
    fn foreign_scope_reads_are_denied_not_aliased() {
        let own = scope(
            "project.dash-graph",
            "repository.dash-graph",
            "worktree.dash-graph",
        );

        let foreign_project = scope(
            "project.other",
            "repository.dash-graph",
            "worktree.dash-graph",
        );
        let foreign_repository = scope(
            "project.dash-graph",
            "repository.other",
            "worktree.dash-graph",
        );
        let foreign_worktree = scope(
            "project.dash-graph",
            "repository.dash-graph",
            "worktree.other",
        );

        assert!(verify_scope(&own, &own.clone()).is_ok());
        for foreign in [foreign_project, foreign_repository, foreign_worktree] {
            assert_eq!(
                verify_scope(&own, &foreign),
                Err(DashboardGraphReadErrorV1::Denied),
                "a foreign exact scope must be concealed behind the typed denial"
            );
        }
    }

    #[test]
    fn graph_store_failures_map_to_their_typed_read_states() {
        assert_eq!(
            map_graph_error(GraphDbError::Cancelled),
            DashboardGraphReadErrorV1::Cancelled
        );
        assert_eq!(
            map_graph_error(GraphDbError::DeadlineExceeded),
            DashboardGraphReadErrorV1::TimedOut
        );
        assert!(matches!(
            map_graph_error(GraphDbError::invalid("bad identifier")),
            DashboardGraphReadErrorV1::InvalidRequest { .. }
        ));
        assert!(matches!(
            map_graph_error(GraphDbError::Corrupt {
                message: "digest mismatch".to_owned()
            }),
            DashboardGraphReadErrorV1::Corrupt { .. }
        ));
        assert!(matches!(
            map_graph_error(GraphDbError::Closed),
            DashboardGraphReadErrorV1::Unavailable { .. }
        ));
    }

    #[test]
    fn topology_watermark_is_content_addressed_and_deterministic() {
        let watermark = TopologyWatermark {
            nodes: 4,
            edges: 3,
            files: 2,
            max_edge_id: 7,
            last_node_update: 1_700_000_000,
        };
        let same = watermark.clone();
        let moved = TopologyWatermark {
            max_edge_id: 8,
            ..watermark.clone()
        };

        assert_eq!(watermark.canonical_text(), same.canonical_text());
        assert_ne!(
            watermark.canonical_text(),
            moved.canonical_text(),
            "any topology movement must publish (and report) a new generation"
        );
    }
}
