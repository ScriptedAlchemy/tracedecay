use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_application::source_edit::{
    RenameHazardKindV1, RenameSiteKindV1, RenameSymbolBindingV1,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionError, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{
    ContentDigest, EdgeAuthorityV1, ManifestDigest, RelationEdgeKindV1, SnapshotFileDispositionV1,
    SourceSpan, SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_usecases::graph::{map_code_graph_read_runtime_error, map_projection_error};
use tracedecay_usecases::tracedecay::SourceEditGraphReadV1;

use crate::errors::{Result, TraceDecayError};

const MAX_RENAME_SYMBOLS: usize = 10_000;
const MAX_RENAME_RELATIONS: usize = 100_000;
const RENAME_REFERENCE_RELATIONS: &[RelationEdgeKindV1] = &[
    RelationEdgeKindV1::Calls,
    RelationEdgeKindV1::Uses,
    RelationEdgeKindV1::TypeOf,
    RelationEdgeKindV1::Implements,
    RelationEdgeKindV1::Extends,
    RelationEdgeKindV1::Annotates,
];

#[derive(Clone)]
pub(super) struct RenameGraphFileV1 {
    pub(super) path: String,
    pub(super) content_digest: ContentDigest,
    pub(super) disposition: SnapshotFileDispositionV1,
}

#[derive(Clone)]
pub(super) struct RenameGraphSiteV1 {
    pub(super) file: String,
    pub(super) evidence_span: Option<SourceSpan>,
    pub(super) line: u32,
    pub(super) column: u32,
    pub(super) source_occurrence: String,
    pub(super) source_qualified_name: String,
    pub(super) declaration_kind: Option<RenameSiteKindV1>,
    pub(super) relation_kind: Option<RelationEdgeKindV1>,
    pub(super) apply_grade: bool,
}

pub(super) struct RenameGraphEvidenceV1 {
    pub(super) files: Vec<RenameGraphFileV1>,
    pub(super) target_sites: Vec<RenameGraphSiteV1>,
    pub(super) other_sites: Vec<RenameGraphSiteV1>,
    pub(super) callers: BTreeSet<String>,
    pub(super) affected_tests: BTreeSet<String>,
    pub(super) reference_count: usize,
    pub(super) graph_revision: ManifestDigest,
}

pub(super) enum RenameGraphEvidenceLoadV1 {
    Ready(RenameGraphEvidenceV1),
    Refused {
        message: String,
        kind: RenameHazardKindV1,
    },
}

fn projection_error(error: CodeGraphProjectionError) -> TraceDecayError {
    map_code_graph_read_runtime_error(map_projection_error(error))
}

fn metadata(
    summary: &CodeGraphSymbolSummaryV1,
) -> Result<&tracedecay_code_index::lineage::LineageSymbolRecordV1> {
    summary.metadata.as_ref().ok_or_else(|| {
        TraceDecayError::project_route(
            "code-graph-projection-incomplete",
            false,
            "rename evidence requires canonical symbol metadata",
        )
    })
}

fn path_for<'a>(
    summary: &CodeGraphSymbolSummaryV1,
    paths: &'a BTreeMap<tracedecay_domain::FileOccurrenceId, String>,
) -> Result<&'a str> {
    let binding = summary.binding.as_ref().ok_or_else(|| {
        TraceDecayError::project_route(
            "code-graph-projection-incomplete",
            false,
            "rename evidence requires a canonical file binding",
        )
    })?;
    paths.get(&binding.file).map(String::as_str).ok_or_else(|| {
        TraceDecayError::project_route(
            "code-graph-projection-corrupt",
            false,
            "rename symbol refers to a file absent from its pinned generation",
        )
    })
}

fn declaration_site(
    summary: &CodeGraphSymbolSummaryV1,
    paths: &BTreeMap<tracedecay_domain::FileOccurrenceId, String>,
) -> Result<RenameGraphSiteV1> {
    let metadata = metadata(summary)?;
    Ok(RenameGraphSiteV1 {
        file: path_for(summary, paths)?.to_owned(),
        evidence_span: None,
        line: metadata.presentation.start_line,
        column: metadata.presentation.start_column,
        source_occurrence: summary.occurrence.as_str().to_owned(),
        source_qualified_name: metadata.qualified_name.clone(),
        declaration_kind: Some(declaration_kind(
            &metadata.kind,
            path_for(summary, paths)?,
            &metadata.presentation.name,
        )),
        relation_kind: None,
        apply_grade: true,
    })
}

fn reference_site(
    edge: &CodeGraphSemanticEdgeV1,
    paths: &BTreeMap<tracedecay_domain::FileOccurrenceId, String>,
) -> Result<RenameGraphSiteV1> {
    let metadata = metadata(&edge.neighbor)?;
    Ok(RenameGraphSiteV1 {
        file: path_for(&edge.neighbor, paths)?.to_owned(),
        evidence_span: Some(edge.edge.evidence_span),
        line: metadata.presentation.start_line,
        column: metadata.presentation.start_column,
        source_occurrence: edge.neighbor.occurrence.as_str().to_owned(),
        source_qualified_name: metadata.qualified_name.clone(),
        declaration_kind: None,
        relation_kind: Some(edge.edge.kind),
        apply_grade: matches!(
            edge.edge.authority,
            EdgeAuthorityV1::SyntaxExact
                | EdgeAuthorityV1::NameResolved
                | EdgeAuthorityV1::CompilerOrLspResolved
        ),
    })
}

fn looks_like_test(path: &str, name: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("tests/")
        || path.starts_with("test/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.ends_with("_test.rs")
        || path.ends_with(".test.ts")
        || path.ends_with(".spec.ts")
        || name.starts_with("test_")
        || name.ends_with("_test")
}

fn looks_like_example(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("examples/") || path.contains("/examples/")
}

fn neighbor_batches(
    graph: &SourceEditGraphReadV1,
    occurrences: &[SymbolOccurrenceId],
) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>> {
    graph
        .reader()
        .callers(
            occurrences,
            RENAME_REFERENCE_RELATIONS,
            MAX_RENAME_RELATIONS,
            graph.cancellation(),
        )
        .map_err(projection_error)
}

pub(super) fn declaration_kind(kind: &str, path: &str, name: &str) -> RenameSiteKindV1 {
    if looks_like_example(path) {
        return RenameSiteKindV1::Example;
    }
    if looks_like_test(path, name) {
        return RenameSiteKindV1::Test;
    }
    match kind {
        "constructor" => RenameSiteKindV1::Constructor,
        "method" | "struct_method" | "abstract_method" => RenameSiteKindV1::InherentMethod,
        "enum_variant" => RenameSiteKindV1::EnumVariant,
        "pattern" | "match_arm" => RenameSiteKindV1::Pattern,
        "trait" | "interface" | "interface_type" => RenameSiteKindV1::TraitDeclaration,
        "annotation" | "annotation_usage" | "decorator" => RenameSiteKindV1::Annotation,
        _ => RenameSiteKindV1::Declaration,
    }
}

pub(super) fn relation_kind(edge: RelationEdgeKindV1, line: &str, path: &str) -> RenameSiteKindV1 {
    if looks_like_example(path) {
        return RenameSiteKindV1::Example;
    }
    if looks_like_test(path, "") {
        return RenameSiteKindV1::Test;
    }
    if line.trim_start().starts_with("pub use ") || line.trim_start().starts_with("export ") {
        return RenameSiteKindV1::Reexport;
    }
    if line.trim_start().starts_with("use ") || line.contains(" import ") {
        return RenameSiteKindV1::Import;
    }
    match edge {
        RelationEdgeKindV1::Calls => RenameSiteKindV1::ResolvedCall,
        RelationEdgeKindV1::Implements | RelationEdgeKindV1::Extends => {
            RenameSiteKindV1::TraitImplementation
        }
        RelationEdgeKindV1::Annotates => RenameSiteKindV1::Annotation,
        RelationEdgeKindV1::TypeOf => RenameSiteKindV1::GenericArgument,
        _ if line.contains("::") || line.contains('.') => RenameSiteKindV1::QualifiedPath,
        _ => RenameSiteKindV1::UnqualifiedPath,
    }
}

pub(super) fn ensure_active(graph: &SourceEditGraphReadV1) -> Result<()> {
    if graph.cancellation().is_cancelled() {
        return Err(map_code_graph_read_runtime_error(
            tracedecay_usecases::graph::CodeGraphReadError::Cancelled,
        ));
    }
    Ok(())
}

pub(super) fn load(
    graph: &SourceEditGraphReadV1,
    binding: &RenameSymbolBindingV1,
) -> Result<RenameGraphEvidenceLoadV1> {
    let occurrence = match SymbolOccurrenceId::new(binding.node_id.clone()) {
        Ok(occurrence) => occurrence,
        Err(error) => {
            return Ok(RenameGraphEvidenceLoadV1::Refused {
                message: format!("invalid canonical symbol occurrence id: {error}"),
                kind: RenameHazardKindV1::StaleEvidence,
            });
        }
    };
    let cancellation = graph.cancellation();
    let files = graph
        .reader()
        .files(Arc::clone(&cancellation))
        .map_err(projection_error)?;
    let mut paths = BTreeMap::new();
    for file in &files {
        if paths
            .insert(file.file_occurrence_id.clone(), file.logical_path.clone())
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "code-graph-projection-corrupt",
                false,
                "rename generation contains a duplicate file occurrence",
            ));
        }
    }
    let Some(target) = graph
        .reader()
        .symbol_summary(&occurrence, Arc::clone(&cancellation))
        .map_err(projection_error)?
    else {
        return Ok(RenameGraphEvidenceLoadV1::Refused {
            message: format!(
                "stale rename evidence: occurrence {} no longer exists",
                binding.node_id
            ),
            kind: RenameHazardKindV1::StaleEvidence,
        });
    };
    let target_metadata = metadata(&target)?;
    let target_path = path_for(&target, &paths)?;
    if target_metadata.presentation.name != binding.old_name
        || target_metadata.qualified_name != binding.qualified_name
        || target_metadata.kind != binding.kind
        || target_path != binding.file
    {
        return Ok(RenameGraphEvidenceLoadV1::Refused {
            message: "stale rename evidence: exact symbol identity changed".to_owned(),
            kind: RenameHazardKindV1::StaleEvidence,
        });
    }

    let mut same_name = graph
        .reader()
        .resolve_simple_name(
            &binding.old_name,
            None,
            MAX_RENAME_SYMBOLS + 1,
            Arc::clone(&cancellation),
        )
        .map_err(projection_error)?
        .into_iter()
        .filter(|candidate| {
            candidate
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.presentation.name == binding.old_name)
        })
        .collect::<Vec<_>>();
    if same_name.len() > MAX_RENAME_SYMBOLS {
        return Err(TraceDecayError::project_route(
            "code-graph-budget-exhausted",
            false,
            "rename evidence exceeds the 10,000 same-name symbol budget",
        ));
    }
    if !same_name
        .iter()
        .any(|candidate| candidate.occurrence == occurrence)
    {
        return Err(TraceDecayError::project_route(
            "code-graph-projection-corrupt",
            false,
            "rename target is absent from the generation's simple-name index",
        ));
    }
    if same_name.iter().any(|candidate| {
        candidate.occurrence != occurrence
            && candidate
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.qualified_name == binding.qualified_name)
    }) {
        return Ok(RenameGraphEvidenceLoadV1::Refused {
            message: format!("ambiguous canonical symbol `{}`", binding.qualified_name),
            kind: RenameHazardKindV1::AmbiguousSymbol,
        });
    }
    same_name.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
    let occurrences = same_name
        .iter()
        .map(|candidate| candidate.occurrence.clone())
        .collect::<Vec<_>>();
    let batches = neighbor_batches(graph, &occurrences)?;
    if batches.len() != same_name.len() {
        return Err(TraceDecayError::project_route(
            "code-graph-projection-corrupt",
            false,
            "rename caller batch does not match its requested symbol seeds",
        ));
    }

    let mut target_sites = vec![declaration_site(&target, &paths)?];
    let mut other_sites = Vec::new();
    let mut callers = BTreeSet::new();
    let mut affected_tests = BTreeSet::new();
    let mut target_edges = Vec::new();
    let mut all_revision_edges = Vec::new();
    for (candidate, edges) in same_name.iter().zip(batches) {
        let is_target = candidate.occurrence == occurrence;
        if !is_target {
            other_sites.push(declaration_site(candidate, &paths)?);
        }
        for edge in edges {
            let site = reference_site(&edge, &paths)?;
            let source_metadata = metadata(&edge.neighbor)?;
            if is_target {
                if edge.edge.kind == RelationEdgeKindV1::Calls {
                    callers.insert(source_metadata.qualified_name.clone());
                }
                if looks_like_test(&site.file, &source_metadata.presentation.name) {
                    affected_tests.insert(site.file.clone());
                }
                target_sites.push(site);
                target_edges.push(edge.clone());
            } else {
                other_sites.push(site);
            }
            all_revision_edges.push(edge);
        }
    }
    if looks_like_test(target_path, &target_metadata.presentation.name) {
        affected_tests.insert(target_path.to_owned());
    }
    all_revision_edges.sort_by(|left, right| {
        left.edge
            .from_occurrence
            .cmp(&right.edge.from_occurrence)
            .then(left.edge.to_occurrence.cmp(&right.edge.to_occurrence))
            .then(left.edge.kind.cmp(&right.edge.kind))
            .then(left.edge.evidence_span.cmp(&right.edge.evidence_span))
    });
    let revision_rows = all_revision_edges
        .iter()
        .map(|edge| {
            (
                &edge.edge,
                &edge.neighbor.occurrence,
                &edge.neighbor.binding,
                &edge.neighbor.metadata,
            )
        })
        .collect::<Vec<_>>();
    let revision_symbols = same_name
        .iter()
        .map(|summary| (&summary.occurrence, &summary.binding, &summary.metadata))
        .collect::<Vec<_>>();
    let graph_revision = canonical_sha256(&(
        "tracedecay.rename-graph-evidence.v2",
        graph.reader().generation(),
        &files,
        &revision_symbols,
        &revision_rows,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("cannot derive rename graph revision: {error}"),
    })?;

    Ok(RenameGraphEvidenceLoadV1::Ready(RenameGraphEvidenceV1 {
        files: files
            .into_iter()
            .map(|file| RenameGraphFileV1 {
                path: file.logical_path,
                content_digest: file.content_digest,
                disposition: file.disposition,
            })
            .collect(),
        target_sites,
        other_sites,
        callers,
        affected_tests,
        reference_count: target_edges.len(),
        graph_revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::{declaration_kind, relation_kind};
    use tracedecay_application::source_edit::RenameSiteKindV1;
    use tracedecay_domain::RelationEdgeKindV1;

    #[test]
    fn canonical_kinds_map_to_typed_rename_sites() {
        assert_eq!(
            declaration_kind("enum_variant", "src/value.rs", "Ready"),
            RenameSiteKindV1::EnumVariant
        );
        assert_eq!(
            relation_kind(RelationEdgeKindV1::Calls, "value();", "src/main.rs"),
            RenameSiteKindV1::ResolvedCall
        );
        assert_eq!(
            relation_kind(RelationEdgeKindV1::Uses, "pub use value;", "src/lib.rs"),
            RenameSiteKindV1::Reexport
        );
        assert_eq!(
            declaration_kind("match_arm", "src/value.rs", "Ready"),
            RenameSiteKindV1::Pattern
        );
        assert_eq!(
            relation_kind(RelationEdgeKindV1::Uses, "value();", "examples/demo.rs"),
            RenameSiteKindV1::Example
        );
    }
}
