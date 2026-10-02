//! Persisted per-file evidence of one generation.
//!
//! Sealing derives three kinds of evidence across files rather than reading
//! them from one file's artifacts: the cross-file edges name resolution
//! binds, the call sites it cannot bind, and each symbol's lineage from the
//! prior generation. Each row is stored with the file that owns its source
//! site (an edge's caller, a call site, a lineage row's current symbol), so
//! restoring one file's evidence reads that file and the snapshot, never
//! another file's symbols.
//!
//! A row names its file's symbols by position in their occurrence order and
//! its file's unresolved references by position in their sorted order; both
//! are fixed by the file's content. An edge target is named by its file's
//! logical path and its symbol identity, which an edit to the target file
//! keeps. Implicit lineage rows say that a symbol continued unchanged from
//! the prior occurrence of its identity tuple, a pure function of the two
//! generation ids, that prior occurrence, and the symbol, so the evidence of
//! a carried file keeps its bytes across generations.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, EdgeAuthorityV1, FileOccurrenceId,
    RelationEdgeKindV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
};

use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1, collect_bounded_ordered};
use crate::chunks::CodeIndexUnresolvedReferenceV1;
use crate::lineage::{LineageKindV1, LineageSymbolRecordV1, SymbolLineageCandidateV1};

/// `(source symbol, target, kind, evidence start byte, evidence end byte)`;
/// the authority is the name resolution every cross-file edge carries.
type EdgeRowV1 = (u32, u32, RelationEdgeKindV1, u64, u64);

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedFileEvidenceV1 {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    target_paths: Vec<String>,
    /// `(position in target_paths, symbol identity in that file)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    targets: Vec<(u32, SymbolIdentityDigest)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    edges: Vec<EdgeRowV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved_calls: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lineage: Vec<PersistedLineageRowV1>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum PersistedLineageRowV1 {
    /// Symbols `start..start + count`, each unchanged from itself.
    Unchanged {
        start: u32,
        count: u32,
    },
    /// Symbol `current`, unchanged from the prior occurrence `prior`.
    UnchangedFrom {
        current: u32,
        prior: SymbolOccurrenceId,
    },
    Candidate(Box<SymbolLineageCandidateV1>),
}

/// One file's evidence restored onto its generation.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FileEvidenceV1 {
    pub(super) cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    pub(super) lineage: Vec<SymbolLineageCandidateV1>,
}

/// The positions a file's evidence rows index, derived from the file alone.
struct FileRowsV1<'a> {
    symbols: Vec<&'a LineageSymbolRecordV1>,
    references: Vec<&'a CodeIndexUnresolvedReferenceV1>,
}

impl<'a> FileRowsV1<'a> {
    fn of(file: &'a FileGenerationArtifactsV1) -> Self {
        let mut symbols = file
            .artifacts
            .symbols
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        let mut references = file
            .artifacts
            .unresolved_references
            .iter()
            .collect::<Vec<_>>();
        references.sort();
        Self {
            symbols,
            references,
        }
    }

    fn symbol(
        &self,
        position: u32,
    ) -> Result<&'a LineageSymbolRecordV1, CodeIndexProductionErrorV1> {
        usize::try_from(position)
            .ok()
            .and_then(|position| self.symbols.get(position).copied())
            .ok_or_else(|| contract("sealed file evidence names a symbol outside its file"))
    }
}

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

fn position(index: usize) -> Result<u32, CodeIndexProductionErrorV1> {
    u32::try_from(index).map_err(|_| contract("sealed file evidence position exceeds u32"))
}

/// The rows each file of `files` owns, in `files` order, and the prior
/// generation implicit lineage rows are relative to.
///
/// Every cross-file edge's source and target, every call limitation's site,
/// and every lineage row's current symbol must belong to a file of `files`;
/// the seal is refused otherwise.
pub(super) fn compact_file_evidence(
    files: &[Arc<FileGenerationArtifactsV1>],
    edges: &[CanonicalRelationEdgeV1],
    unresolved_calls: &[CodeIndexUnresolvedReferenceV1],
    lineage: &[SymbolLineageCandidateV1],
    current_generation: &CodeGenerationId,
) -> Result<(Option<CodeGenerationId>, Vec<PersistedFileEvidenceV1>), CodeIndexProductionErrorV1> {
    let rows = files
        .iter()
        .map(|file| FileRowsV1::of(file))
        .collect::<Vec<_>>();
    let mut owners = HashMap::<&SymbolOccurrenceId, (usize, u32)>::new();
    for (file, file_rows) in rows.iter().enumerate() {
        for (local, symbol) in file_rows.symbols.iter().enumerate() {
            if owners
                .insert(&symbol.occurrence, (file, position(local)?))
                .is_some()
            {
                return Err(contract(
                    "one sealed symbol occurrence belongs to multiple files",
                ));
            }
        }
    }
    let owner = |occurrence: &SymbolOccurrenceId, message: &str| {
        owners
            .get(occurrence)
            .copied()
            .ok_or_else(|| contract(message))
    };
    let mut per_file = (0..files.len())
        .map(|_| PerFileInputsV1::default())
        .collect::<Vec<_>>();
    for edge in edges
        .iter()
        .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
    {
        let (file, from) = owner(
            &edge.from_occurrence,
            "sealed cross-file edge source is not a generation symbol",
        )?;
        let target = owner(
            &edge.to_occurrence,
            "sealed cross-file edge target is not a generation symbol",
        )?;
        per_file[file].edges.push((from, target, edge));
    }
    for call in unresolved_calls {
        let (file, _) = owner(
            &call.from_occurrence,
            "sealed call limitation site is not a generation symbol",
        )?;
        per_file[file].calls.push(call);
    }
    for candidate in lineage {
        let (file, current) = owner(
            &candidate.current_occurrence,
            "sealed lineage row names a symbol outside the generation",
        )?;
        per_file[file].lineage.push((current, candidate));
    }
    let prior_generation = lineage
        .first()
        .map(|row| row.evidence.prior_generation.clone());
    let indexed = per_file.into_iter().enumerate().collect::<Vec<_>>();
    let persisted = collect_bounded_ordered(&indexed, |(file, inputs), _worker| {
        inputs.compact(
            &rows[*file],
            &rows,
            files,
            prior_generation.as_ref(),
            current_generation,
        )
    })?;
    Ok((prior_generation, persisted))
}

#[derive(Default)]
struct PerFileInputsV1<'a> {
    edges: Vec<(u32, (usize, u32), &'a CanonicalRelationEdgeV1)>,
    calls: Vec<&'a CodeIndexUnresolvedReferenceV1>,
    lineage: Vec<(u32, &'a SymbolLineageCandidateV1)>,
}

impl PerFileInputsV1<'_> {
    fn compact(
        &self,
        file_rows: &FileRowsV1<'_>,
        rows: &[FileRowsV1<'_>],
        files: &[Arc<FileGenerationArtifactsV1>],
        prior_generation: Option<&CodeGenerationId>,
        current_generation: &CodeGenerationId,
    ) -> Result<PersistedFileEvidenceV1, CodeIndexProductionErrorV1> {
        let mut persisted = PersistedFileEvidenceV1::default();
        let mut paths = HashMap::<usize, u32>::new();
        let mut targets = HashMap::<(usize, u32), u32>::new();
        for &(from, (target_file, target_local), edge) in &self.edges {
            let target = match targets.entry((target_file, target_local)) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    let path = match paths.entry(target_file) {
                        Entry::Occupied(path) => *path.get(),
                        Entry::Vacant(path) => {
                            let at = position(persisted.target_paths.len())?;
                            persisted
                                .target_paths
                                .push(files[target_file].authority.logical_path.clone());
                            *path.insert(at)
                        }
                    };
                    let at = position(persisted.targets.len())?;
                    persisted.targets.push((
                        path,
                        rows[target_file].symbol(target_local)?.identity.clone(),
                    ));
                    *entry.insert(at)
                }
            };
            persisted.edges.push((
                from,
                target,
                edge.kind,
                edge.evidence_span.start_byte,
                edge.evidence_span.end_byte,
            ));
        }
        for call in &self.calls {
            let local = file_rows.references.binary_search(call).map_err(|_| {
                contract("sealed call limitation is not one of its file's references")
            })?;
            persisted.unresolved_calls.push(position(local)?);
        }
        for &(current, candidate) in &self.lineage {
            let symbol = file_rows.symbol(current)?;
            let implicit = match prior_generation {
                Some(prior_generation) if candidate.kind == LineageKindV1::Unchanged => {
                    SymbolLineageCandidateV1::exact_unchanged(
                        prior_generation,
                        current_generation,
                        &candidate.prior_occurrence,
                        symbol,
                    )
                    .map_err(CodeIndexProductionErrorV1::Lineage)?
                        == *candidate
                }
                _ => false,
            };
            let row = if !implicit {
                PersistedLineageRowV1::Candidate(Box::new(candidate.clone()))
            } else if candidate.prior_occurrence == candidate.current_occurrence {
                if let Some(PersistedLineageRowV1::Unchanged { start, count }) =
                    persisted.lineage.last_mut()
                    && start.checked_add(*count) == Some(current)
                {
                    *count += 1;
                    continue;
                }
                PersistedLineageRowV1::Unchanged {
                    start: current,
                    count: 1,
                }
            } else {
                PersistedLineageRowV1::UnchangedFrom {
                    current,
                    prior: candidate.prior_occurrence.clone(),
                }
            };
            persisted.lineage.push(row);
        }
        Ok(persisted)
    }
}

impl PersistedFileEvidenceV1 {
    pub(super) fn is_empty(&self) -> bool {
        self.edges.is_empty() && self.unresolved_calls.is_empty() && self.lineage.is_empty()
    }

    /// The evidence `file` sealed, with edge targets bound to the file each
    /// path names in `present_files`.
    pub(super) fn expand(
        self,
        file: &FileGenerationArtifactsV1,
        present_files: &HashMap<&str, &FileOccurrenceId>,
        prior_generation: Option<&CodeGenerationId>,
        current_generation: &CodeGenerationId,
    ) -> Result<FileEvidenceV1, CodeIndexProductionErrorV1> {
        let rows = FileRowsV1::of(file);
        let targets = self
            .targets
            .iter()
            .map(|(path, identity)| {
                let file_occurrence = usize::try_from(*path)
                    .ok()
                    .and_then(|path| self.target_paths.get(path))
                    .and_then(|path| present_files.get(path.as_str()))
                    .ok_or_else(|| {
                        contract("sealed cross-file edge targets a file outside its snapshot")
                    })?;
                crate::chunks::symbol_occurrence_id(file_occurrence, identity)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut evidence = FileEvidenceV1::default();
        for (from, target, kind, start_byte, end_byte) in self.edges {
            evidence.cross_file_edges.push(CanonicalRelationEdgeV1 {
                from_occurrence: rows.symbol(from)?.occurrence.clone(),
                to_occurrence: usize::try_from(target)
                    .ok()
                    .and_then(|target| targets.get(target))
                    .cloned()
                    .ok_or_else(|| contract("sealed cross-file edge names an unknown target"))?,
                kind,
                authority: EdgeAuthorityV1::NameResolved,
                evidence_span: SourceSpan {
                    start_byte,
                    end_byte,
                },
            });
        }
        for call in self.unresolved_calls {
            let reference = usize::try_from(call)
                .ok()
                .and_then(|call| rows.references.get(call).copied())
                .ok_or_else(|| {
                    contract("sealed call limitation names a reference outside its file")
                })?;
            evidence.unresolved_calls.push(reference.clone());
        }
        let implicit = |symbol: &LineageSymbolRecordV1, prior: Option<&SymbolOccurrenceId>| {
            let prior_generation = prior_generation.ok_or_else(|| {
                contract("sealed lineage has implicit rows without a prior generation")
            })?;
            SymbolLineageCandidateV1::exact_unchanged(
                prior_generation,
                current_generation,
                prior.unwrap_or(&symbol.occurrence),
                symbol,
            )
            .map_err(CodeIndexProductionErrorV1::Lineage)
        };
        for row in self.lineage {
            match row {
                PersistedLineageRowV1::Unchanged { start, count } => {
                    let end = start
                        .checked_add(count)
                        .ok_or_else(|| contract("sealed lineage run exceeds u32"))?;
                    for current in start..end {
                        evidence
                            .lineage
                            .push(implicit(rows.symbol(current)?, None)?);
                    }
                }
                PersistedLineageRowV1::UnchangedFrom { current, prior } => {
                    evidence
                        .lineage
                        .push(implicit(rows.symbol(current)?, Some(&prior))?);
                }
                PersistedLineageRowV1::Candidate(candidate) => evidence.lineage.push(*candidate),
            }
        }
        Ok(evidence)
    }
}
