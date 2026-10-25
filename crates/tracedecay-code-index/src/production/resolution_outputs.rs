//! Canonical graph resolution outputs derived while a generation seals.

use std::collections::HashMap;
use tracedecay_domain::{CanonicalRelationEdgeV1, SourceSpan, SymbolOccurrenceId};

use crate::chunks::CodeIndexUnresolvedReferenceV1;
use crate::graph_projection::unresolved_call_limitations;

use super::CodeIndexProductionErrorV1;
use super::resolution_view::ResolutionMemoryChangeV1;
use super::resolution_view::{FileSymbolsByNameV1, ResolutionFileV1};

/// Caller gaps are derived per file, so their borrowed site tables and owned
/// projection rows cannot grow with the whole corpus at once.
pub(super) fn unresolved_calls_for_edges_charged<T: ResolutionFileV1>(
    files: &[T],
    edges: &[CanonicalRelationEdgeV1],
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, CodeIndexProductionErrorV1> {
    let mut index_bytes = 0_usize;
    let mut charge = |bytes: ResolutionMemoryChangeV1| {
        bytes.apply_with(&mut index_bytes, |index_bytes| {
            retain(u64::try_from(index_bytes).map_err(|_| {
                CodeIndexProductionErrorV1::Contract("call index charge exceeds u64".to_owned())
            })?)
        })
    };
    let by_name = FileSymbolsByNameV1::new_charged(files, &mut charge)?;
    let mut modules = super::helpers::ResolutionModulesV1::new_charged(files, &mut charge)?;
    let mut owners = HashMap::new();
    for (index, file) in files.iter().enumerate() {
        for symbol in file.binding()?.symbols() {
            super::resolution_view::reserve_resolution_map(&mut owners, &mut charge)?;
            owners.insert(&symbol.occurrence, index);
        }
    }
    charge(ResolutionMemoryChangeV1::Retain(
        files
            .len()
            .saturating_mul(std::mem::size_of::<Vec<&CanonicalRelationEdgeV1>>()),
    ))?;
    let mut routed = (0..files.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    for edge in edges {
        if let Some(index) = owners.get(&edge.from_occurrence) {
            super::resolution_view::reserve_resolution_vec(&mut routed[*index], 1, &mut charge)?;
            routed[*index].push(edge);
        }
    }
    let mut calls = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let facts = file.binding()?;
        let base = index_bytes.saturating_add(super::resident_bytes::vec_bytes(
            &calls,
            calls.capacity(),
            super::resident_bytes::unresolved_heap_bytes,
        ));
        let selected_bytes = facts.unresolved_references().len().saturating_mul(
            std::mem::size_of::<(usize, &CodeIndexUnresolvedReferenceV1)>()
                + std::mem::size_of::<(&str, &CodeIndexUnresolvedReferenceV1)>()
                + std::mem::size_of::<usize>(),
        );
        let mut current = base.saturating_add(selected_bytes);
        retain(u64::try_from(current).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("call workspace exceeds u64".to_owned())
        })?)?;
        let selection = vec![(
            index,
            (0..facts.unresolved_references().len()).collect::<Vec<_>>(),
        )];
        let imports = super::helpers::unresolved_import_calls_with_modules(
            files,
            &by_name,
            &modules,
            Some(&selection),
            &mut |bytes: ResolutionMemoryChangeV1| {
                bytes.apply_with(&mut current, |current| {
                    retain(u64::try_from(current).map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "call workspace exceeds u64".to_owned(),
                        )
                    })?)
                })
            },
        )?;
        let references = facts
            .unresolved_references()
            .iter()
            .map(|reference| (file.logical_path(), reference))
            .collect::<Vec<_>>();
        // At most every source reference survives as a call limitation. The
        // existing projector may append import gaps, already owned above.
        current = current
            .saturating_add(super::resident_bytes::vec_bytes(
                facts.unresolved_references(),
                facts.unresolved_references().len(),
                super::resident_bytes::unresolved_heap_bytes,
            ))
            .saturating_add(super::resident_bytes::btree_bytes(
                facts.unresolved_references().len(),
                std::mem::size_of::<((&SymbolOccurrenceId, SourceSpan), Option<&str>)>(),
            ))
            .saturating_add(super::resident_bytes::btree_bytes(
                routed[index].len(),
                std::mem::size_of::<((&SymbolOccurrenceId, SourceSpan), &str)>(),
            ))
            .saturating_add(super::resident_bytes::btree_bytes(
                routed[index].len(),
                std::mem::size_of::<(&SymbolOccurrenceId, SourceSpan)>(),
            ));
        retain(u64::try_from(current).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("call workspace exceeds u64".to_owned())
        })?)?;
        let result = unresolved_call_limitations(
            &references,
            routed[index].iter().copied(),
            imports,
            &|| Ok(()),
        )
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        super::resolution_view::reserve_resolution_vec(
            &mut calls,
            result.len(),
            &mut |bytes: ResolutionMemoryChangeV1| {
                bytes.apply_with(&mut current, |current| {
                    retain(u64::try_from(current).map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "call workspace exceeds u64".to_owned(),
                        )
                    })?)
                })
            },
        )?;
        calls.extend(result);
        modules.release_file_memo(index);
    }
    drop(modules);
    drop(by_name);
    retain(
        u64::try_from(super::resident_bytes::vec_bytes(
            &calls,
            calls.capacity(),
            super::resident_bytes::unresolved_heap_bytes,
        ))
        .map_err(|_| CodeIndexProductionErrorV1::Contract("call outputs exceed u64".to_owned()))?,
    )?;
    Ok(calls)
}
