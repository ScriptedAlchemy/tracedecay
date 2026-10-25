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
//! generation ids, that prior occurrence, and the symbol. A file whose every
//! symbol continued unchanged from itself stores no lineage at all: its
//! lineage is implied by the generation's prior, so the evidence of a carried
//! file keeps its bytes across generations, and a file without cross-file
//! evidence needs no segment.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, EdgeAuthorityV1, FileOccurrenceId,
    RelationEdgeKindV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
};
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

use super::resolution_view::ResolutionMemoryChangeV1;
use super::resolution_view::{
    ResolutionFileFactsV1, ResolutionFileV1, reserve_resolution_map, reserve_resolution_vec,
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
    /// Whether `lineage` is the file's whole lineage. Otherwise every symbol
    /// of the file continued unchanged from itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    explicit_lineage: bool,
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
    fn of(file: &'a dyn ResolutionFileFactsV1) -> Self {
        let mut symbols = file.symbols().iter().map(Arc::as_ref).collect::<Vec<_>>();
        symbols.sort_unstable_by(|left, right| left.occurrence.cmp(&right.occurrence));
        let mut references = file.unresolved_references().iter().collect::<Vec<_>>();
        references.sort_unstable();
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

/// Borrowed row tables before their allocation-free canonical sort.
fn charge_file_rows(
    file: &dyn ResolutionFileFactsV1,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    charge(ResolutionMemoryChangeV1::Retain(
        file.symbols()
            .len()
            .saturating_mul(std::mem::size_of::<&LineageSymbolRecordV1>())
            .saturating_add(
                file.unresolved_references()
                    .len()
                    .saturating_mul(std::mem::size_of::<&CodeIndexUnresolvedReferenceV1>()),
            ),
    ))
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
pub(super) fn compact_file_evidence<T: ResolutionFileV1>(
    files: &[T],
    edges: &[CanonicalRelationEdgeV1],
    unresolved_calls: &[CodeIndexUnresolvedReferenceV1],
    lineage: &[SymbolLineageCandidateV1],
    current_generation: &CodeGenerationId,
) -> Result<(Option<CodeGenerationId>, Vec<PersistedFileEvidenceV1>), CodeIndexProductionErrorV1> {
    let rows = files
        .iter()
        .map(|file| Ok(FileRowsV1::of(file.binding()?)))
        .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()?;
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
        let (target_file, target_local) = target;
        per_file[file].edges.push((
            from,
            (
                files[target_file].logical_path(),
                &rows[target_file].symbol(target_local)?.identity,
            ),
            edge,
        ));
    }
    // `Implements` rows are Go implementor gaps, which no file reference
    // carries; restore re-derives them from the persisted method sets.
    for call in unresolved_calls
        .iter()
        .filter(|call| call.kind != RelationEdgeKindV1::Implements)
    {
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
    let persisted = collect_bounded_ordered(&indexed, |(file, inputs)| {
        inputs.compact(&rows[*file], prior_generation.as_ref(), current_generation)
    })?;
    Ok((prior_generation, persisted))
}

/// Compact one file at a time; only borrowed routing rows span files. The
/// visitor receives the live workspace charge so encoding can reserve above
/// it without introducing a second reservation authority.
pub(super) fn for_each_cold_file_evidence<T: ResolutionFileV1>(
    files: &[T],
    edges: &[CanonicalRelationEdgeV1],
    calls: &[CodeIndexUnresolvedReferenceV1],
    generation: &CodeGenerationId,
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    visit: &mut impl FnMut(
        usize,
        &PersistedFileEvidenceV1,
        u64,
        &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let mut live = 0_usize;
    let mut charge = |bytes: ResolutionMemoryChangeV1| {
        bytes.apply_with(&mut live, |live| {
            retain(u64::try_from(live).map_err(|_| contract("file evidence charge exceeds u64"))?)
        })
    };
    let mut owners = HashMap::new();
    for (index, file) in files.iter().enumerate() {
        for symbol in file.binding()?.symbols() {
            if owners.contains_key(&symbol.occurrence) {
                return Err(contract(
                    "one sealed symbol occurrence belongs to multiple files",
                ));
            }
            reserve_resolution_map(&mut owners, &mut charge)?;
            owners.insert(&symbol.occurrence, (index, &symbol.identity));
        }
    }
    charge(ResolutionMemoryChangeV1::Retain(
        files.len().saturating_mul(std::mem::size_of::<(
            Vec<TargetedEdgeV1<'_>>,
            Vec<&CodeIndexUnresolvedReferenceV1>,
        )>()),
    ))?;
    let mut routed = (0..files.len())
        .map(|_| (Vec::new(), Vec::new()))
        .collect::<Vec<_>>();
    for edge in edges
        .iter()
        .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
    {
        let (owner, _) = owners
            .get(&edge.from_occurrence)
            .ok_or_else(|| contract("sealed cross-file edge source is not a generation symbol"))?;
        let (target, identity) = owners
            .get(&edge.to_occurrence)
            .ok_or_else(|| contract("sealed cross-file edge target is not a generation symbol"))?;
        reserve_resolution_vec(&mut routed[*owner].0, 1, &mut charge)?;
        routed[*owner]
            .0
            .push((edge, files[*target].logical_path(), *identity));
    }
    for call in calls
        .iter()
        .filter(|call| call.kind != RelationEdgeKindV1::Implements)
    {
        let (owner, _) = owners
            .get(&call.from_occurrence)
            .ok_or_else(|| contract("sealed call limitation site is not a generation symbol"))?;
        reserve_resolution_vec(&mut routed[*owner].1, 1, &mut charge)?;
        routed[*owner].1.push(call);
    }
    let routing_bytes = live;
    for (index, (edges, calls)) in routed.iter().enumerate() {
        let facts = files[index].binding()?;
        let row_bytes = facts
            .symbols()
            .len()
            .saturating_mul(std::mem::size_of::<&LineageSymbolRecordV1>())
            .saturating_add(
                facts
                    .unresolved_references()
                    .len()
                    .saturating_mul(std::mem::size_of::<&CodeIndexUnresolvedReferenceV1>()),
            )
            .saturating_add(super::resident_bytes::hash_table_bytes::<(
                &SymbolOccurrenceId,
                u32,
            )>(super::helpers::resolution_hash_capacity(
                facts.symbols().len(),
            )))
            .saturating_add(edges.len().saturating_mul(std::mem::size_of::<(
                u32,
                (&str, &SymbolIdentityDigest),
                &CanonicalRelationEdgeV1,
            )>()))
            .saturating_add(
                calls
                    .len()
                    .saturating_mul(std::mem::size_of::<&CodeIndexUnresolvedReferenceV1>()),
            );
        let mut current = routing_bytes.saturating_add(row_bytes);
        retain(u64::try_from(current).map_err(|_| contract("file evidence charge exceeds u64"))?)?;
        let rows = FileRowsV1::of(facts);
        let locals = rows
            .symbols
            .iter()
            .enumerate()
            .map(|(local, symbol)| Ok((&symbol.occurrence, position(local)?)))
            .collect::<Result<HashMap<_, _>, CodeIndexProductionErrorV1>>()?;
        let inputs = PerFileInputsV1 {
            edges: edges
                .iter()
                .map(|(edge, path, identity)| {
                    Ok((
                        *locals.get(&edge.from_occurrence).ok_or_else(|| {
                            contract("sealed cross-file edge source is not a symbol of its file")
                        })?,
                        (*path, *identity),
                        *edge,
                    ))
                })
                .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()?,
            calls: calls.clone(),
            lineage: Vec::new(),
        };
        let persisted = inputs.compact_charged(
            &rows,
            None,
            generation,
            &mut |bytes: ResolutionMemoryChangeV1| {
                bytes.apply_with(&mut current, |current| {
                    retain(
                        u64::try_from(current)
                            .map_err(|_| contract("file evidence charge exceeds u64"))?,
                    )
                })
            },
        )?;
        visit(
            index,
            &persisted,
            u64::try_from(current).map_err(|_| contract("file evidence charge exceeds u64"))?,
            retain,
        )?;
    }
    retain(
        u64::try_from(routing_bytes).map_err(|_| contract("file evidence charge exceeds u64"))?,
    )?;
    Ok(())
}

/// A cross-file edge from one file with its target's logical path and symbol
/// identity.
pub(super) type TargetedEdgeV1<'a> = (
    &'a CanonicalRelationEdgeV1,
    &'a str,
    &'a SymbolIdentityDigest,
);

/// One file's rows from its own evidence: its cross-file edges in canonical
/// edge order, its call limitations in sorted order, and its lineage in
/// current-occurrence order. These are the rows [`compact_file_evidence`]
/// gives the same file inside a whole generation.
pub(super) fn compact_one_file_evidence(
    file: &FileGenerationArtifactsV1,
    edges: &[TargetedEdgeV1<'_>],
    calls: &[&CodeIndexUnresolvedReferenceV1],
    lineage: &[&SymbolLineageCandidateV1],
    prior_generation: Option<&CodeGenerationId>,
    current_generation: &CodeGenerationId,
) -> Result<PersistedFileEvidenceV1, CodeIndexProductionErrorV1> {
    let rows = FileRowsV1::of(file);
    let locals = rows
        .symbols
        .iter()
        .enumerate()
        .map(|(local, symbol)| Ok((&symbol.occurrence, position(local)?)))
        .collect::<Result<HashMap<_, _>, CodeIndexProductionErrorV1>>()?;
    let local = |occurrence: &SymbolOccurrenceId, message: &str| {
        locals
            .get(occurrence)
            .copied()
            .ok_or_else(|| contract(message))
    };
    let inputs = PerFileInputsV1 {
        edges: edges
            .iter()
            .map(|(edge, path, identity)| {
                Ok((
                    local(
                        &edge.from_occurrence,
                        "sealed cross-file edge source is not a symbol of its file",
                    )?,
                    (*path, *identity),
                    *edge,
                ))
            })
            .collect::<Result<_, CodeIndexProductionErrorV1>>()?,
        calls: calls.to_vec(),
        lineage: lineage
            .iter()
            .map(|candidate| {
                Ok((
                    local(
                        &candidate.current_occurrence,
                        "sealed lineage row names a symbol outside its file",
                    )?,
                    *candidate,
                ))
            })
            .collect::<Result<_, CodeIndexProductionErrorV1>>()?,
    };
    inputs.compact(&rows, prior_generation, current_generation)
}

#[derive(Default)]
struct PerFileInputsV1<'a> {
    edges: Vec<(
        u32,
        (&'a str, &'a SymbolIdentityDigest),
        &'a CanonicalRelationEdgeV1,
    )>,
    calls: Vec<&'a CodeIndexUnresolvedReferenceV1>,
    lineage: Vec<(u32, &'a SymbolLineageCandidateV1)>,
}

impl PerFileInputsV1<'_> {
    fn compact(
        &self,
        file_rows: &FileRowsV1<'_>,
        prior_generation: Option<&CodeGenerationId>,
        current_generation: &CodeGenerationId,
    ) -> Result<PersistedFileEvidenceV1, CodeIndexProductionErrorV1> {
        self.compact_charged(file_rows, prior_generation, current_generation, &mut |_| {
            Ok(())
        })
    }

    fn compact_charged(
        &self,
        file_rows: &FileRowsV1<'_>,
        prior_generation: Option<&CodeGenerationId>,
        current_generation: &CodeGenerationId,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<PersistedFileEvidenceV1, CodeIndexProductionErrorV1> {
        let mut persisted = PersistedFileEvidenceV1::default();
        let mut paths = HashMap::<&str, u32>::new();
        let mut targets = HashMap::<(&str, &SymbolIdentityDigest), u32>::new();
        for &(from, (target_path, target_identity), edge) in &self.edges {
            if !targets.contains_key(&(target_path, target_identity)) {
                reserve_resolution_map(&mut targets, charge)?;
            }
            let target = match targets.entry((target_path, target_identity)) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    if !paths.contains_key(target_path) {
                        reserve_resolution_map(&mut paths, charge)?;
                    }
                    let path = match paths.entry(target_path) {
                        Entry::Occupied(path) => *path.get(),
                        Entry::Vacant(path) => {
                            let at = position(persisted.target_paths.len())?;
                            reserve_resolution_vec(&mut persisted.target_paths, 1, charge)?;
                            charge(ResolutionMemoryChangeV1::Retain(target_path.len()))?;
                            persisted.target_paths.push(target_path.to_owned());
                            *path.insert(at)
                        }
                    };
                    let at = position(persisted.targets.len())?;
                    reserve_resolution_vec(&mut persisted.targets, 1, charge)?;
                    charge(ResolutionMemoryChangeV1::Retain(
                        target_identity.as_str().len(),
                    ))?;
                    persisted.targets.push((path, target_identity.clone()));
                    *entry.insert(at)
                }
            };
            reserve_resolution_vec(&mut persisted.edges, 1, charge)?;
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
            reserve_resolution_vec(&mut persisted.unresolved_calls, 1, charge)?;
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
        let identity_lineage = match persisted.lineage.as_slice() {
            [] => file_rows.symbols.is_empty(),
            [PersistedLineageRowV1::Unchanged { start: 0, count }] => {
                usize::try_from(*count).ok() == Some(file_rows.symbols.len())
            }
            _ => false,
        };
        if prior_generation.is_some() && identity_lineage {
            persisted.lineage.clear();
        } else {
            persisted.explicit_lineage = prior_generation.is_some();
        }
        Ok(persisted)
    }
}

/// The lineage a file without explicit rows carries under `prior_generation`:
/// every symbol continued unchanged from itself.
pub(super) fn identity_lineage(
    file: &FileGenerationArtifactsV1,
    prior_generation: Option<&CodeGenerationId>,
    current_generation: &CodeGenerationId,
) -> Result<Vec<SymbolLineageCandidateV1>, CodeIndexProductionErrorV1> {
    let Some(prior_generation) = prior_generation else {
        return Ok(Vec::new());
    };
    FileRowsV1::of(file)
        .symbols
        .into_iter()
        .map(|symbol| {
            SymbolLineageCandidateV1::exact_unchanged(
                prior_generation,
                current_generation,
                &symbol.occurrence,
                symbol,
            )
            .map_err(CodeIndexProductionErrorV1::Lineage)
        })
        .collect()
}

impl PersistedFileEvidenceV1 {
    pub(super) fn is_empty(&self) -> bool {
        self.edges.is_empty() && self.unresolved_calls.is_empty() && !self.explicit_lineage
    }

    pub(super) fn has_explicit_lineage(&self) -> bool {
        self.explicit_lineage
    }

    /// The same evidence with every symbol's lineage implicit: what a file
    /// carried unchanged into a successor generation seals.
    pub(super) fn with_identity_lineage(mut self) -> Self {
        self.explicit_lineage = false;
        self.lineage.clear();
        self
    }

    /// Whether a sealed cross-file edge lands in one of `paths`.
    pub(super) fn targets_any(&self, paths: &HashSet<&str>) -> bool {
        self.target_paths
            .iter()
            .any(|path| paths.contains(path.as_str()))
    }

    pub(super) fn cross_file_edge_count(&self) -> usize {
        self.edges.len()
    }

    /// The cross-file edges `file` sealed, in sealed order, each still named
    /// by its target's logical path and symbol identity.
    pub(super) fn for_each_sealed_edge(
        &self,
        file: &dyn ResolutionFileFactsV1,
        operation: &ResidentMemoryOperationV1,
        visit: &mut impl FnMut(
            &SymbolOccurrenceId,
            RelationEdgeKindV1,
            SourceSpan,
            &str,
            &SymbolIdentityDigest,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let mut memory = super::ConstructionMemoryV1::new(operation.clone())?;
        charge_file_rows(file, &mut |change| {
            memory.resize(change.apply_to_u64(memory.charged_bytes)?)
        })?;
        let rows = FileRowsV1::of(file);
        for (from, target, kind, start_byte, end_byte) in &self.edges {
            let (target_path, target_identity) = usize::try_from(*target)
                .ok()
                .and_then(|target| self.targets.get(target))
                .and_then(|(path, identity)| {
                    let path = self.target_paths.get(usize::try_from(*path).ok()?)?;
                    Some((path, identity))
                })
                .ok_or_else(|| contract("sealed cross-file edge names an unknown target"))?;
            visit(
                &rows.symbol(*from)?.occurrence,
                *kind,
                SourceSpan {
                    start_byte: *start_byte,
                    end_byte: *end_byte,
                },
                target_path,
                target_identity,
            )?;
        }
        Ok(())
    }

    /// The call limitations `file` sealed, in sorted order. Only borrowed
    /// references reach the visitor; the row-table reservation cannot escape.
    pub(super) fn for_each_unresolved_call<'f>(
        &self,
        file: &'f dyn ResolutionFileFactsV1,
        operation: &ResidentMemoryOperationV1,
        visit: &mut impl FnMut(
            &'f CodeIndexUnresolvedReferenceV1,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let mut memory = super::ConstructionMemoryV1::new(operation.clone())?;
        charge_file_rows(file, &mut |change| {
            memory.resize(change.apply_to_u64(memory.charged_bytes)?)
        })?;
        let rows = FileRowsV1::of(file);
        for call in &self.unresolved_calls {
            let reference = usize::try_from(*call)
                .ok()
                .and_then(|call| rows.references.get(call).copied())
                .ok_or_else(|| {
                    contract("sealed call limitation names a reference outside its file")
                })?;
            visit(reference)?;
        }
        Ok(())
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
        if !self.explicit_lineage {
            if !self.lineage.is_empty() {
                return Err(contract(
                    "sealed file evidence has lineage rows it does not claim",
                ));
            }
            evidence.lineage = identity_lineage(file, prior_generation, current_generation)?;
            return Ok(evidence);
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
