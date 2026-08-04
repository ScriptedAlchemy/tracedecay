use std::collections::HashMap;

use tracedecay_domain::{CodeSearchChunkId, FileOccurrenceId, SymbolOccurrenceId};
use tracedecay_query::retrieval::ports::RetrievalPortError;

use super::ServingWarmControl;

/// One-time point-lookup indices over a sealed generation's record vectors.
///
/// Duplicate keys retain their lowest source position, preserving
/// `Iterator::find` equivalence. Adjacency positions remain in source order.
pub(super) struct GenerationRecordIndexV1 {
    files_by_occurrence: HashMap<FileOccurrenceId, usize>,
    chunks_by_id: HashMap<CodeSearchChunkId, usize>,
    symbols_by_occurrence: HashMap<SymbolOccurrenceId, usize>,
    chunk_by_symbol: HashMap<SymbolOccurrenceId, usize>,
    chunk_by_file_symbol: HashMap<(FileOccurrenceId, SymbolOccurrenceId), usize>,
    edges_from: HashMap<SymbolOccurrenceId, Vec<usize>>,
    edges_to: HashMap<SymbolOccurrenceId, Vec<usize>>,
}

impl GenerationRecordIndexV1 {
    pub(super) fn build(
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
        control: &ServingWarmControl,
    ) -> Result<Self, RetrievalPortError> {
        let files = &generation.snapshot().files;
        let mut files_by_occurrence = HashMap::with_capacity(files.len());
        for (position, file) in files.iter().enumerate() {
            control.checkpoint()?;
            files_by_occurrence
                .entry(file.file_occurrence_id.clone())
                .or_insert(position);
        }

        let chunks = generation.chunks().chunks();
        let mut chunks_by_id = HashMap::with_capacity(chunks.len());
        let mut chunk_by_symbol = HashMap::new();
        let mut chunk_by_file_symbol = HashMap::new();
        for (position, chunk) in chunks.iter().enumerate() {
            control.checkpoint()?;
            chunks_by_id.entry(chunk.id.clone()).or_insert(position);
            if let Some(symbol) = chunk.anchor.symbol_occurrence_id.as_ref() {
                chunk_by_symbol.entry(symbol.clone()).or_insert(position);
                chunk_by_file_symbol
                    .entry((chunk.anchor.file_occurrence_id.clone(), symbol.clone()))
                    .or_insert(position);
            }
        }

        let symbols = &generation.symbols().symbols;
        let mut symbols_by_occurrence = HashMap::with_capacity(symbols.len());
        for (position, record) in symbols.iter().enumerate() {
            control.checkpoint()?;
            symbols_by_occurrence
                .entry(record.occurrence.clone())
                .or_insert(position);
        }

        let mut edges_from: HashMap<SymbolOccurrenceId, Vec<usize>> = HashMap::new();
        let mut edges_to: HashMap<SymbolOccurrenceId, Vec<usize>> = HashMap::new();
        for (position, edge) in generation.edges().iter().enumerate() {
            control.checkpoint()?;
            edges_from
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(position);
            edges_to
                .entry(edge.to_occurrence.clone())
                .or_default()
                .push(position);
        }

        Ok(Self {
            files_by_occurrence,
            chunks_by_id,
            symbols_by_occurrence,
            chunk_by_symbol,
            chunk_by_file_symbol,
            edges_from,
            edges_to,
        })
    }

    pub(super) fn file_position(&self, file: &FileOccurrenceId) -> Option<usize> {
        self.files_by_occurrence.get(file).copied()
    }

    pub(super) fn chunk_position(&self, chunk: &CodeSearchChunkId) -> Option<usize> {
        self.chunks_by_id.get(chunk).copied()
    }

    pub(super) fn symbol_position(&self, symbol: &SymbolOccurrenceId) -> Option<usize> {
        self.symbols_by_occurrence.get(symbol).copied()
    }

    pub(super) fn chunk_position_for_symbol(&self, symbol: &SymbolOccurrenceId) -> Option<usize> {
        self.chunk_by_symbol.get(symbol).copied()
    }

    pub(super) fn chunk_position_for_file_symbol(
        &self,
        file: &FileOccurrenceId,
        symbol: &SymbolOccurrenceId,
    ) -> Option<usize> {
        self.chunk_by_file_symbol
            .get(&(file.clone(), symbol.clone()))
            .copied()
    }

    pub(super) fn incident_edge_positions(
        &self,
        symbol: &SymbolOccurrenceId,
        reverse: bool,
    ) -> &[usize] {
        let adjacency = if reverse {
            self.edges_to.get(symbol)
        } else {
            self.edges_from.get(symbol)
        };
        adjacency.map_or(&[][..], Vec::as_slice)
    }
}
