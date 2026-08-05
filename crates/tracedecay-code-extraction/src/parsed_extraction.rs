//! Canonical extraction over a parser-owned Tree-sitter tree.
//!
//! Language adapters receive an already parsed tree plus an explicit traversal
//! scope. They never acquire a second parser on this path. Changed traversal is
//! rooted at complete top-level syntax nodes so language state and qualified
//! names remain stable; the shared retained-document owner merges the returned
//! delta with its prior canonical rows.

use std::collections::BTreeSet;

use tracedecay_domain::{ExtractionResult, NodeKind};
use tree_sitter::{Node as TreeSitterNode, Tree};

use crate::incremental::ParseChangedRange;

#[derive(Clone, Copy, Debug)]
pub enum ParsedExtractionScope<'a> {
    FullDocument,
    ChangedRegions(&'a [ParseChangedRange]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedExtractionResetReason {
    AdapterColdParserFallback,
    ChangedRootIdentity,
    FullReplacement,
    LanguageChanged,
    MissingPriorExtraction,
    MultilineEdit,
    PartialParse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedExtractionDisposition {
    FullDocument,
    ChangedRegions,
    Reset { reason: ParsedExtractionResetReason },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParsedTraversalMetrics {
    pub visited_top_level_nodes: usize,
    pub visited_bytes: usize,
}

#[derive(Debug)]
pub struct ParsedExtraction {
    pub result: ExtractionResult,
    pub disposition: ParsedExtractionDisposition,
    pub metrics: ParsedTraversalMetrics,
}

impl ParsedExtraction {
    pub fn complete(
        result: ExtractionResult,
        scope: ParsedExtractionScope<'_>,
        metrics: ParsedTraversalMetrics,
    ) -> Self {
        let disposition = match scope {
            ParsedExtractionScope::FullDocument => ParsedExtractionDisposition::FullDocument,
            ParsedExtractionScope::ChangedRegions(_) => ParsedExtractionDisposition::ChangedRegions,
        };
        Self {
            result,
            disposition,
            metrics,
        }
    }

    pub fn reset(
        result: ExtractionResult,
        reason: ParsedExtractionResetReason,
        source_bytes: usize,
    ) -> Self {
        Self {
            result,
            disposition: ParsedExtractionDisposition::Reset { reason },
            metrics: ParsedTraversalMetrics {
                visited_top_level_nodes: 0,
                visited_bytes: source_bytes,
            },
        }
    }
}

/// Visit direct root children selected by one full or changed extraction
/// request. A top-level child is visited at most once even when ranges overlap.
pub(crate) fn visit_root_children(
    tree: &Tree,
    scope: ParsedExtractionScope<'_>,
    mut visit: impl FnMut(TreeSitterNode<'_>),
) -> ParsedTraversalMetrics {
    let root = tree.root_node();
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return ParsedTraversalMetrics::default();
    }

    let mut selected = BTreeSet::new();
    loop {
        let child = cursor.node();
        let include = match scope {
            ParsedExtractionScope::FullDocument => true,
            ParsedExtractionScope::ChangedRegions(regions) => {
                regions.iter().any(|region| node_intersects(child, region))
            }
        };
        if include {
            selected.insert((child.start_byte(), child.end_byte()));
            visit(child);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    ParsedTraversalMetrics {
        visited_top_level_nodes: selected.len(),
        visited_bytes: selected.into_iter().fold(0usize, |total, (start, end)| {
            total.saturating_add(end.saturating_sub(start))
        }),
    }
}

fn node_intersects(node: TreeSitterNode<'_>, region: &ParseChangedRange) -> bool {
    if region.start_byte == region.end_byte {
        node.start_byte() <= region.start_byte && node.end_byte() >= region.end_byte
    } else {
        node.start_byte() < region.end_byte && node.end_byte() > region.start_byte
    }
}

pub(crate) fn merge_changed_extraction(
    previous: &ExtractionResult,
    mut delta: ExtractionResult,
    old_start_row: u32,
    old_end_row: u32,
) -> Option<ExtractionResult> {
    let _delta_file = delta
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File)?;
    let delta_ids = delta
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let prior_roots = previous
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
        .filter(|node| {
            ranges_overlap(node.start_line, node.end_line, old_start_row, old_end_row)
                || delta_ids.contains(&node.id)
        })
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let mut removed = prior_roots.into_iter().collect::<BTreeSet<_>>();
    loop {
        let descendants = previous
            .nodes
            .iter()
            .filter_map(|node| {
                node.parent_id
                    .as_ref()
                    .filter(|parent| removed.contains(*parent) && !removed.contains(&node.id))
                    .map(|_| node.id.clone())
            })
            .collect::<Vec<_>>();
        if descendants.is_empty() {
            break;
        }
        removed.extend(descendants);
    }

    let mut merged = previous.clone();
    merged
        .nodes
        .retain(|node| node.kind != NodeKind::File && !removed.contains(&node.id));
    merged
        .edges
        .retain(|edge| !removed.contains(&edge.source) && !removed.contains(&edge.target));
    merged
        .unresolved_refs
        .retain(|reference| !removed.contains(&reference.from_node_id));
    merged.errors.clear();

    merged.nodes.append(&mut delta.nodes);
    merged.edges.append(&mut delta.edges);
    merged.unresolved_refs.append(&mut delta.unresolved_refs);
    merged.errors.append(&mut delta.errors);
    merged.duration_ms = delta.duration_ms;
    merged.sanitize();
    Some(merged)
}

fn ranges_overlap(left_start: u32, left_end: u32, right_start: u32, right_end: u32) -> bool {
    left_start <= right_end && right_start <= left_end
}
