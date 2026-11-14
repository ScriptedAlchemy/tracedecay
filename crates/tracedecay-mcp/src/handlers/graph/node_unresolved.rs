//! Recorded unresolved calls for an exact node, without inferred targets.

use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{
    NodeDetailsV1, NodeUnresolvedCallOutcomeV1, NodeUnresolvedCallReasonV1,
    NodeUnresolvedCallReportV1, NodeUnresolvedCallSiteV1,
};
use tracedecay_domain::UnmodeledImportShapeV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use crate::handlers::dependency_hints::unavailable_evidence;
use crate::tools::render::{self, Md};

use super::{graph_symbol_corrupt, required_graph_file_path};

const NODE_UNRESOLVED_REFERENCE_LIMIT: u16 = 16;

pub(super) fn node_unresolved_calls(
    graph: &VerifiedGraphQuery,
    caller: &CodeGraphSymbolSummaryV1,
) -> Result<NodeUnresolvedCallReportV1> {
    let binding = caller.binding.as_ref().ok_or_else(|| {
        graph_symbol_corrupt("unresolved-call report has no bound caller file".to_owned())
    })?;
    let outcome = match recorded_sites(graph, caller) {
        Ok(outcome) => outcome,
        Err(error) => NodeUnresolvedCallOutcomeV1::Unavailable {
            evidence: unavailable_evidence(&error),
        },
    };
    Ok(NodeUnresolvedCallReportV1 {
        code_generation: graph.generation().clone(),
        caller_node_id: caller.occurrence.clone(),
        file_occurrence_id: binding.file.clone(),
        file: required_graph_file_path(caller)?.to_owned(),
        outcome,
    })
}

fn recorded_sites(
    graph: &VerifiedGraphQuery,
    caller: &CodeGraphSymbolSummaryV1,
) -> Result<NodeUnresolvedCallOutcomeV1> {
    let page = graph
        .unresolved_call_sites(
            &caller.occurrence,
            usize::from(NODE_UNRESOLVED_REFERENCE_LIMIT),
        )?
        .ok_or_else(|| {
            graph_symbol_corrupt(
                "bound caller is absent from its generation's evidence catalog".to_owned(),
            )
        })?;
    let mut sites = Vec::with_capacity(page.references.len());
    for reference in page.references {
        let caller_span = caller
            .binding
            .as_ref()
            .and_then(|binding| binding.source_span)
            .ok_or_else(|| {
                graph_symbol_corrupt("unresolved caller has no indexed source span".to_owned())
            })?;
        if reference.from_occurrence != caller.occurrence
            || reference.evidence_span.start_byte < caller_span.start_byte
            || reference.evidence_span.end_byte > caller_span.end_byte
        {
            return Err(graph_symbol_corrupt(
                "unresolved call evidence lies outside its bound caller".to_owned(),
            ));
        }
        reference.evidence_span.validate().map_err(|error| {
            TraceDecayError::project_route(
                "node-unresolved-call-evidence-invalid",
                false,
                format!("unresolved call evidence has an invalid source span: {error}"),
            )
        })?;
        let mut reasons = Vec::new();
        if reference.ambiguous_local {
            reasons.push(NodeUnresolvedCallReasonV1::AmbiguousLocal);
        }
        if let Some(shape) = reference.unmodeled_import {
            reasons.push(match shape {
                UnmodeledImportShapeV1::BlockGlob => NodeUnresolvedCallReasonV1::UnmodeledBlockGlob,
                UnmodeledImportShapeV1::InlineModuleGlob => {
                    NodeUnresolvedCallReasonV1::UnmodeledInlineModuleGlob
                }
            });
        }
        if reasons.is_empty() {
            reasons.push(NodeUnresolvedCallReasonV1::TargetUnresolved);
        }
        sites.push(NodeUnresolvedCallSiteV1 {
            reference_name: reference.reference_name,
            evidence_span: reference.evidence_span,
            argument_count: reference.argument_count,
            reasons,
        });
    }
    Ok(if page.has_more {
        NodeUnresolvedCallOutcomeV1::Truncated {
            sites,
            maximum_references: u32::from(NODE_UNRESOLVED_REFERENCE_LIMIT),
        }
    } else {
        NodeUnresolvedCallOutcomeV1::Complete { sites }
    })
}

/// Keeps the established metadata rendering and gives the report an explicit
/// coverage statement instead of flattening a complete empty list to silence.
pub(crate) fn render_node_md(node: &NodeDetailsV1, value: &Value) -> String {
    let mut metadata = value.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove("unresolved_calls");
    }
    let mut md = Md::new();
    md.raw(&render::generic_md(&metadata));
    md.blank().heading(2, "Recorded unresolved calls").blank();
    let sites = match &node.unresolved_calls.outcome {
        NodeUnresolvedCallOutcomeV1::Complete { sites } => {
            md.line(&format!(
                "Complete for recorded evidence: {} unresolved call sites.",
                sites.len()
            ));
            sites.as_slice()
        }
        NodeUnresolvedCallOutcomeV1::Truncated {
            sites,
            maximum_references,
        } => {
            md.line(&format!(
                "Truncated after {maximum_references} retained evidence records; showing {} unresolved call sites.",
                sites.len()
            ));
            sites.as_slice()
        }
        NodeUnresolvedCallOutcomeV1::Unavailable { evidence } => {
            md.line(&format!(
                "Unavailable ({}): {}",
                evidence.reason_code, evidence.detail
            ));
            &[]
        }
    };
    md.line("This reports recorded limitations, not all possible runtime calls. It does not infer target edges.");
    if !sites.is_empty() {
        md.line("Evidence spans are file byte offsets; extraction may retain the enclosing declaration when no exact token span is available.");
    }
    for site in sites {
        let reasons = site
            .reasons
            .iter()
            .map(|reason| match reason {
                NodeUnresolvedCallReasonV1::TargetUnresolved => "target unresolved",
                NodeUnresolvedCallReasonV1::AmbiguousLocal => "ambiguous local target",
                NodeUnresolvedCallReasonV1::UnmodeledBlockGlob => "unmodeled block glob import",
                NodeUnresolvedCallReasonV1::UnmodeledInlineModuleGlob => {
                    "unmodeled inline-module glob import"
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        md.blank().bullet(&format!(
            "Bytes {}..{}: {reasons}",
            site.evidence_span.start_byte, site.evidence_span.end_byte,
        ));
        if let Some(arguments) = site.argument_count {
            md.line(&format!("  Recorded arguments: {arguments}"));
        }
        md.code("text", &site.reference_name);
    }
    md.render()
}
