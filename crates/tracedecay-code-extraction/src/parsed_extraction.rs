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

use crate::ExtractionArtifactV1;
use crate::incremental::{ParseChangedRange, ParsePoint};

#[derive(Clone, Copy, Debug)]
pub enum ParsedExtractionScope<'a> {
    FullDocument,
    ChangedRegions(&'a [ParseChangedRange]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedExtractionResetReason {
    ChangedRootIdentity,
    CompositeGrammar,
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

/// Structured artifact extracted from one parser-owned tree traversal.
#[derive(Debug)]
pub struct ParsedExtractionArtifactV1 {
    pub artifact: ExtractionArtifactV1,
    pub disposition: ParsedExtractionDisposition,
    pub metrics: ParsedTraversalMetrics,
}

impl ParsedExtractionArtifactV1 {
    pub(crate) fn complete(
        mut artifact: ExtractionArtifactV1,
        scope: ParsedExtractionScope<'_>,
        metrics: ParsedTraversalMetrics,
    ) -> Self {
        let disposition = match scope {
            ParsedExtractionScope::FullDocument => ParsedExtractionDisposition::FullDocument,
            ParsedExtractionScope::ChangedRegions(_) => ParsedExtractionDisposition::ChangedRegions,
        };
        artifact.canonicalize_order();
        Self {
            artifact,
            disposition,
            metrics,
        }
    }

    pub(crate) fn reset(
        mut artifact: ExtractionArtifactV1,
        reason: ParsedExtractionResetReason,
        source_bytes: usize,
    ) -> Self {
        artifact.canonicalize_order();
        Self {
            artifact,
            disposition: ParsedExtractionDisposition::Reset { reason },
            metrics: ParsedTraversalMetrics {
                visited_top_level_nodes: 0,
                visited_bytes: source_bytes,
            },
        }
    }

    pub(crate) fn from_parsed(parsed: ParsedExtraction) -> Self {
        Self {
            artifact: ExtractionArtifactV1::from_result(parsed.result),
            disposition: parsed.disposition,
            metrics: parsed.metrics,
        }
    }

    pub(crate) fn into_parsed(self) -> ParsedExtraction {
        ParsedExtraction {
            result: self.artifact.result,
            disposition: self.disposition,
            metrics: self.metrics,
        }
    }
}

impl ParsedExtraction {
    pub fn complete(
        mut result: ExtractionResult,
        scope: ParsedExtractionScope<'_>,
        metrics: ParsedTraversalMetrics,
    ) -> Self {
        let disposition = match scope {
            ParsedExtractionScope::FullDocument => ParsedExtractionDisposition::FullDocument,
            ParsedExtractionScope::ChangedRegions(_) => ParsedExtractionDisposition::ChangedRegions,
        };
        result.canonicalize_order();
        Self {
            result,
            disposition,
            metrics,
        }
    }

    pub fn reset(
        mut result: ExtractionResult,
        reason: ParsedExtractionResetReason,
        source_bytes: usize,
    ) -> Self {
        result.canonicalize_order();
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
    edit_start: ParsePoint,
    old_edit_end: ParsePoint,
) -> Option<ExtractionResult> {
    if !previous.errors.is_empty() || !delta.errors.is_empty() {
        return None;
    }
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
            node_intersects_edit(node, edit_start, old_edit_end) || delta_ids.contains(&node.id)
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
    // `ParsedExtraction::complete` canonicalizes row order for every
    // extraction path, so the merge only restores set membership here.
    merged.sanitize();
    Some(merged)
}

pub(crate) fn node_intersects_edit(
    node: &tracedecay_domain::Node,
    edit_start: ParsePoint,
    old_edit_end: ParsePoint,
) -> bool {
    let node_start = (node.start_line as usize, node.start_column as usize);
    let node_end = (node.end_line as usize, node.end_column as usize);
    let edit_start = (edit_start.row, edit_start.column);
    let edit_end = (old_edit_end.row, old_edit_end.column);
    if edit_start == edit_end {
        node_start <= edit_start && edit_start < node_end
    } else {
        node_start < edit_end && edit_start < node_end
    }
}
