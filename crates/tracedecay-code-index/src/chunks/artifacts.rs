//! Canonical parser-backed evidence retained for one indexed code file.

use std::{cmp::Ordering, collections::BTreeMap};

use serde::{Deserialize, Serialize};
use tracedecay_code_extraction::{
    ExtractedImportEvidenceV1, ExtractionArtifactV1, ImportModuleKindV1, ImportNamespaceV1,
};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, ExtractionBatchV1, FileOccurrenceId, SourceSpan,
    SymbolOccurrenceId,
};

use super::{ChunkingFailureV1, CodeFileChunksV1, canonical_edge_key};
use crate::extract::parser_import_rows_digest;
use crate::lineage::LineageSymbolRecordV1;

const IMPORT_AUTHORITY_MISMATCH: &str =
    "import evidence does not match parser-backed extraction rows";

/// One file-bound import binding observed directly by the language parser.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexImportEvidenceV1 {
    pub logical_path: String,
    pub file_occurrence_id: FileOccurrenceId,
    pub module_specifier: String,
    pub imported_name: Option<String>,
    pub local_name: Option<String>,
    pub namespace: ImportNamespaceV1,
    pub module_kind: ImportModuleKindV1,
    pub span: SourceSpan,
    pub start_line: u32,
    pub start_column: u32,
}

impl CodeIndexImportEvidenceV1 {
    fn from_extracted(
        row: &ExtractedImportEvidenceV1,
        file_occurrence_id: &FileOccurrenceId,
    ) -> Self {
        Self {
            logical_path: row.logical_path.clone(),
            file_occurrence_id: file_occurrence_id.clone(),
            module_specifier: row.module_specifier.clone(),
            imported_name: row.imported_name.clone(),
            local_name: row.local_name.clone(),
            namespace: row.namespace,
            module_kind: row.module_kind,
            span: row.span,
            start_line: row.start_line,
            start_column: row.start_column,
        }
    }

    fn to_extracted(&self) -> ExtractedImportEvidenceV1 {
        ExtractedImportEvidenceV1 {
            logical_path: self.logical_path.clone(),
            module_specifier: self.module_specifier.clone(),
            imported_name: self.imported_name.clone(),
            local_name: self.local_name.clone(),
            namespace: self.namespace,
            module_kind: self.module_kind,
            span: self.span,
            start_line: self.start_line,
            start_column: self.start_column,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ChunkingFailureV1> {
        self.file_occurrence_id
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        if self.logical_path.is_empty() || self.module_specifier.is_empty() {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import evidence has an empty logical path or module specifier".to_owned(),
            ));
        }
        if self.span.is_empty() {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import evidence has an empty source span".to_owned(),
            ));
        }
        if self.imported_name.as_deref().is_some_and(str::is_empty)
            || self.local_name.as_deref().is_some_and(str::is_empty)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import evidence has an empty binding name".to_owned(),
            ));
        }

        let binding_shape_is_valid = match self.namespace {
            ImportNamespaceV1::SideEffect => {
                self.imported_name.is_none() && self.local_name.is_none()
            }
            ImportNamespaceV1::Type | ImportNamespaceV1::Value => {
                self.imported_name.is_some() && self.local_name.is_some()
            }
        };
        if !binding_shape_is_valid {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import namespace does not match its binding names".to_owned(),
            ));
        }

        let is_project_relative =
            self.module_specifier.starts_with("./") || self.module_specifier.starts_with("../");
        let module_kind_is_valid = matches!(
            (self.module_kind, is_project_relative),
            (ImportModuleKindV1::ProjectRelative, true) | (ImportModuleKindV1::BareModule, false)
        );
        if !module_kind_is_valid {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import module kind does not match its module specifier".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Parser-backed evidence for one indexed file. The canonical relation rows
/// contain only relation kinds the graph contract can represent; everything
/// else remains a typed abstention rather than a synthetic edge.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFileIndexArtifactsV1 {
    pub chunks: CodeFileChunksV1,
    pub symbols: Vec<LineageSymbolRecordV1>,
    pub edges: Vec<CanonicalRelationEdgeV1>,
    pub edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
    pub imports: Vec<CodeIndexImportEvidenceV1>,
}

/// Why one parser relation was not promoted into the canonical graph lane.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexEdgeAbstentionReasonV1 {
    MissingSymbolEndpoint,
    UnsupportedRelationKind,
}

/// A parser-observed edge that remains explicitly unavailable to graph
/// traversal. This preserves the raw limitation without inventing a
/// semantically stronger relation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexEdgeAbstentionV1 {
    pub source_node_id: String,
    pub target_node_id: String,
    pub legacy_kind: String,
    pub reason: CodeIndexEdgeAbstentionReasonV1,
}

impl CodeFileIndexArtifactsV1 {
    pub(crate) fn from_parser_artifact(
        chunks: CodeFileChunksV1,
        symbols: Vec<LineageSymbolRecordV1>,
        edges: Vec<CanonicalRelationEdgeV1>,
        edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
        artifact: &ExtractionArtifactV1,
        extraction: &ExtractionBatchV1,
    ) -> Result<Self, ChunkingFailureV1> {
        let imports = artifact
            .imports
            .iter()
            .map(|row| {
                CodeIndexImportEvidenceV1::from_extracted(row, &extraction.file_occurrence_id)
            })
            .collect::<Vec<_>>();
        let artifacts = Self::from_parts(chunks, symbols, edges, edge_abstentions, imports)?;
        artifacts.validate_generation_import_authority(extraction)?;
        Ok(artifacts)
    }

    pub(crate) fn without_parser_rows(
        chunks: CodeFileChunksV1,
        extraction: &ExtractionBatchV1,
    ) -> Result<Self, ChunkingFailureV1> {
        let artifacts = Self::from_parts(chunks, Vec::new(), Vec::new(), Vec::new(), Vec::new())?;
        artifacts.validate_generation_import_authority(extraction)?;
        Ok(artifacts)
    }

    fn from_parts(
        chunks: CodeFileChunksV1,
        symbols: Vec<LineageSymbolRecordV1>,
        edges: Vec<CanonicalRelationEdgeV1>,
        edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
        mut imports: Vec<CodeIndexImportEvidenceV1>,
    ) -> Result<Self, ChunkingFailureV1> {
        imports.sort_by(canonical_import_order);
        let artifacts = Self {
            chunks,
            symbols,
            edges,
            edge_abstentions,
            imports,
        };
        artifacts.validate()?;
        Ok(artifacts)
    }

    /// Verify canonical structure without claiming parser-backed semantic
    /// identity. Generation restoration performs that stronger check against
    /// the persisted extraction batch through
    /// [`Self::validate_generation_import_authority`].
    pub fn validate(&self) -> Result<(), ChunkingFailureV1> {
        self.chunks.validate()?;
        self.validate_imports()?;
        if self
            .symbols
            .windows(2)
            .any(|pair| pair[0].occurrence >= pair[1].occurrence)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "lineage symbols are not in occurrence order".to_owned(),
            ));
        }
        let chunk_occurrences = self
            .chunks
            .chunks
            .iter()
            .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        let occurrences = self
            .symbols
            .iter()
            .map(|symbol| &symbol.occurrence)
            .collect::<std::collections::BTreeSet<_>>();
        if !occurrences.is_subset(&chunk_occurrences) {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "lineage symbol is not represented by a chunk".to_owned(),
            ));
        }
        if self.edges.iter().any(|edge| {
            !occurrences.contains(&edge.from_occurrence)
                || !occurrences.contains(&edge.to_occurrence)
        }) || self
            .edges
            .windows(2)
            .any(|pair| canonical_edge_key(&pair[0]) > canonical_edge_key(&pair[1]))
            || self
                .edge_abstentions
                .windows(2)
                .any(|pair| pair[0] > pair[1])
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "file graph evidence is not canonical".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_generation_import_authority(
        &self,
        extraction: &ExtractionBatchV1,
    ) -> Result<(), ChunkingFailureV1> {
        extraction
            .parser_import_rows_digest
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        let parser_rows = self
            .imports
            .iter()
            .map(CodeIndexImportEvidenceV1::to_extracted)
            .collect::<Vec<_>>();
        let observed = parser_import_rows_digest(&parser_rows).map_err(|error| {
            ChunkingFailureV1::NonCanonicalIdentity(format!(
                "parser import rows digest failed: {error:?}"
            ))
        })?;
        if observed != extraction.parser_import_rows_digest {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                IMPORT_AUTHORITY_MISMATCH.to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_imports(&self) -> Result<(), ChunkingFailureV1> {
        if self
            .imports
            .windows(2)
            .any(|pair| canonical_import_order(&pair[0], &pair[1]) != Ordering::Less)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "import evidence is not in strict canonical source order".to_owned(),
            ));
        }
        let indexed_end = self
            .chunks
            .chunks
            .iter()
            .map(|chunk| chunk.anchor.source_span.end_byte)
            .max();
        let expected_path = self.imports.first().map(|row| row.logical_path.as_str());
        for row in &self.imports {
            row.validate()?;
            if row.file_occurrence_id != self.chunks.document.file_occurrence_id {
                return Err(ChunkingFailureV1::GenerationMismatch);
            }
            if Some(row.logical_path.as_str()) != expected_path {
                return Err(ChunkingFailureV1::NonCanonicalIdentity(
                    "import evidence spans more than one logical file".to_owned(),
                ));
            }
            if indexed_end.is_none_or(|end| row.span.end_byte > end) {
                return Err(ChunkingFailureV1::NonCanonicalIdentity(
                    "import evidence exceeds the indexed file extent".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Carry parser-backed file evidence into a new immutable generation.
    pub fn rematerialize_for_generation(
        &self,
        generation_id: CodeGenerationId,
        file_occurrence_id: FileOccurrenceId,
    ) -> Result<Self, ChunkingFailureV1> {
        self.validate()?;
        let chunks = self
            .chunks
            .rematerialize_for_generation(generation_id, file_occurrence_id.clone())?;
        let mut occurrences = BTreeMap::new();
        for (prior, current) in self.chunks.chunks.iter().zip(&chunks.chunks) {
            if let Some(prior_occurrence) = &prior.anchor.symbol_occurrence_id {
                let current_occurrence = current
                    .anchor
                    .symbol_occurrence_id
                    .as_ref()
                    .ok_or_else(|| {
                        ChunkingFailureV1::NonCanonicalIdentity(
                            "rematerialized symbol occurrence is missing".to_owned(),
                        )
                    })?
                    .clone();
                match occurrences.get(prior_occurrence) {
                    Some(existing) if existing != &current_occurrence => {
                        return Err(ChunkingFailureV1::NonCanonicalIdentity(
                            "symbol occurrence rematerialized inconsistently".to_owned(),
                        ));
                    }
                    _ => {
                        occurrences.insert(prior_occurrence.clone(), current_occurrence);
                    }
                }
            }
        }

        let mut symbols = self.symbols.clone();
        for symbol in &mut symbols {
            symbol.occurrence = rematerialized_occurrence(&occurrences, &symbol.occurrence)?;
        }
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));

        let mut edges = self.edges.clone();
        for edge in &mut edges {
            edge.from_occurrence = rematerialized_occurrence(&occurrences, &edge.from_occurrence)?;
            edge.to_occurrence = rematerialized_occurrence(&occurrences, &edge.to_occurrence)?;
        }
        edges.sort_by(|left, right| canonical_edge_key(left).cmp(&canonical_edge_key(right)));

        let mut imports = self.imports.clone();
        for row in &mut imports {
            row.file_occurrence_id = file_occurrence_id.clone();
        }
        let result = Self {
            chunks,
            symbols,
            edges,
            edge_abstentions: self.edge_abstentions.clone(),
            imports,
        };
        result.validate()?;
        Ok(result)
    }
}

fn rematerialized_occurrence(
    occurrences: &BTreeMap<SymbolOccurrenceId, SymbolOccurrenceId>,
    occurrence: &SymbolOccurrenceId,
) -> Result<SymbolOccurrenceId, ChunkingFailureV1> {
    occurrences.get(occurrence).cloned().ok_or_else(|| {
        ChunkingFailureV1::NonCanonicalIdentity(
            "graph evidence could not be rematerialized".to_owned(),
        )
    })
}

fn canonical_import_order(
    left: &CodeIndexImportEvidenceV1,
    right: &CodeIndexImportEvidenceV1,
) -> Ordering {
    left.span
        .start_byte
        .cmp(&right.span.start_byte)
        .then(left.span.end_byte.cmp(&right.span.end_byte))
        .then(left.start_line.cmp(&right.start_line))
        .then(left.start_column.cmp(&right.start_column))
        .then(left.logical_path.cmp(&right.logical_path))
        .then(left.file_occurrence_id.cmp(&right.file_occurrence_id))
        .then(left.module_specifier.cmp(&right.module_specifier))
        .then(left.imported_name.cmp(&right.imported_name))
        .then(left.local_name.cmp(&right.local_name))
        .then(left.namespace.cmp(&right.namespace))
        .then(left.module_kind.cmp(&right.module_kind))
}
