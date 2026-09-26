//! What one decoded generation holds in memory.
//!
//! [`CodeIndexPublishedGenerationV1::retained_bytes`] is the charge admission
//! takes before re-materializing a generation and what the resident-memory
//! inventory reports for a held decode, so it walks every owned allocation:
//! struct bodies, the heap behind each string and identity, and each
//! `Arc`-shared chunk and symbol record exactly once (from the
//! generation-wide index). Freshly built and decoded strings carry exact
//! capacities, so lengths are the allocation sizes.

use std::mem::size_of;
use std::sync::Arc;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, ChangedCodeChunkV1, CodeChunkProjectionReceiptV1, CodeSearchChunkV1,
    ExactTechnicalTermV1, ProjectionKeyV1, SanitizedCodeFileV1,
};

use super::{CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1};
use crate::chunks::{
    CodeIndexEdgeAbstentionV1, CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1,
    CodeSearchEligibilityV1,
};
use crate::lineage::{LineageSymbolRecordV1, SymbolLineageCandidateV1};

/// An `Arc` allocation: two reference counts before the value.
const ARC_HEADER_BYTES: usize = 2 * size_of::<usize>();

fn vec_bytes<T>(values: &[T], capacity: usize, heap: impl Fn(&T) -> usize) -> usize {
    values
        .iter()
        .fold(capacity.saturating_mul(size_of::<T>()), |bytes, value| {
            bytes.saturating_add(heap(value))
        })
}

fn opt(value: Option<&str>) -> usize {
    value.map_or(0, str::len)
}

/// A `BTreeMap` allocates leaves of eleven slots (and internal nodes above
/// them); a leaf is, on average, two-thirds full once the map is built.
pub(crate) fn btree_bytes(entries: usize, entry_bytes: usize) -> usize {
    entries
        .saturating_mul(3)
        .div_ceil(2)
        .saturating_mul(entry_bytes)
        .saturating_add(size_of::<usize>().saturating_mul(4))
}

fn chunk_bytes(chunk: &CodeSearchChunkV1) -> usize {
    let anchor = &chunk.anchor;
    ARC_HEADER_BYTES
        .saturating_add(size_of::<CodeSearchChunkV1>())
        .saturating_add(chunk.id.as_str().len())
        .saturating_add(anchor.generation_id.as_str().len())
        .saturating_add(anchor.file_occurrence_id.as_str().len())
        .saturating_add(opt(anchor
            .symbol_occurrence_id
            .as_ref()
            .map(|id| id.as_str())))
        .saturating_add(opt(anchor.parent_chunk_id.as_ref().map(|id| id.as_str())))
        .saturating_add(chunk.content_digest.as_str().len())
        .saturating_add(chunk.language_descriptor_revision.as_str().len())
        .saturating_add(chunk.chunker_revision.as_str().len())
        .saturating_add(chunk.sanitizer_revision.as_str().len())
        .saturating_add(chunk.sensitivity.policy_revision.as_str().len())
        .saturating_add(vec_bytes(
            &chunk.exact_terms,
            chunk.exact_terms.capacity(),
            exact_term_bytes,
        ))
        .saturating_add(vec_bytes(
            &chunk.subtokens,
            chunk.subtokens.capacity(),
            String::capacity,
        ))
        .saturating_add(ARC_HEADER_BYTES)
        .saturating_add(chunk.sanitized_text.as_str().len())
}

fn exact_term_bytes(term: &ExactTechnicalTermV1) -> usize {
    term.original_bytes()
        .len()
        .saturating_add(term.canonical_bytes().len())
        .saturating_add(opt(term.symbol_occurrence_id().map(|id| id.as_str())))
}

fn symbol_bytes(symbol: &LineageSymbolRecordV1) -> usize {
    ARC_HEADER_BYTES
        .saturating_add(size_of::<LineageSymbolRecordV1>())
        .saturating_add(symbol.occurrence.as_str().len())
        .saturating_add(symbol.identity.as_str().len())
        .saturating_add(symbol.qualified_name.capacity())
        .saturating_add(symbol.simple_name.capacity())
        .saturating_add(symbol.kind.capacity())
        .saturating_add(symbol.visibility.capacity())
        .saturating_add(opt(symbol.signature.as_deref()))
        .saturating_add(opt(symbol.docstring.as_deref()))
        .saturating_add(vec_bytes(
            &symbol.derives,
            symbol.derives.capacity(),
            String::capacity,
        ))
        .saturating_add(symbol.file_identity.as_str().len())
        .saturating_add(symbol.content_digest.as_str().len())
}

fn edge_heap_bytes(edge: &CanonicalRelationEdgeV1) -> usize {
    edge.from_occurrence
        .as_str()
        .len()
        .saturating_add(edge.to_occurrence.as_str().len())
}

fn abstention_heap_bytes(abstention: &CodeIndexEdgeAbstentionV1) -> usize {
    abstention
        .source_node_id
        .capacity()
        .saturating_add(abstention.target_node_id.capacity())
}

fn import_heap_bytes(import: &CodeIndexImportEvidenceV1) -> usize {
    import
        .logical_path
        .capacity()
        .saturating_add(import.file_occurrence_id.as_str().len())
        .saturating_add(import.module_specifier.capacity())
        .saturating_add(opt(import.imported_name.as_deref()))
        .saturating_add(opt(import.local_name.as_deref()))
}

fn unresolved_heap_bytes(reference: &CodeIndexUnresolvedReferenceV1) -> usize {
    reference
        .from_occurrence
        .as_str()
        .len()
        .saturating_add(reference.reference_name.capacity())
}

fn lineage_heap_bytes(candidate: &SymbolLineageCandidateV1) -> usize {
    let evidence = &candidate.evidence;
    candidate
        .prior_occurrence
        .as_str()
        .len()
        .saturating_add(candidate.current_occurrence.as_str().len())
        .saturating_add(evidence.prior_generation.as_str().len())
        .saturating_add(evidence.current_generation.as_str().len())
        .saturating_add(opt(evidence
            .prior_digest
            .as_ref()
            .map(|digest| digest.as_str())))
        .saturating_add(opt(evidence
            .current_digest
            .as_ref()
            .map(|digest| digest.as_str())))
        .saturating_add(evidence.evidence_digest.as_str().len())
        .saturating_add(vec_bytes(
            &candidate.alternatives,
            candidate.alternatives.capacity(),
            |id| id.as_str().len(),
        ))
        .saturating_add(
            candidate
                .abstention
                .as_ref()
                .map_or(0, |abstention| abstention.reason.capacity()),
        )
}

fn projection_key_bytes(key: &ProjectionKeyV1) -> usize {
    key.schema_revision
        .capacity()
        .saturating_add(key.profile_digest.as_str().len())
}

fn changed_chunk_heap_bytes(change: &ChangedCodeChunkV1) -> usize {
    change
        .chunk_id
        .as_str()
        .len()
        .saturating_add(opt(change
            .prior_digest
            .as_ref()
            .map(|digest| digest.as_str())))
        .saturating_add(opt(change
            .current_digest
            .as_ref()
            .map(|digest| digest.as_str())))
}

fn receipt_heap_bytes(receipt: &CodeChunkProjectionReceiptV1) -> usize {
    projection_key_bytes(&receipt.projection_key)
        .saturating_add(receipt.request_digest.as_str().len())
        .saturating_add(opt(receipt.prior_generation.as_ref().map(|id| id.as_str())))
        .saturating_add(receipt.source_generation.as_str().len())
        .saturating_add(receipt.source_manifest_digest.as_str().len())
        .saturating_add(receipt.chunk_id.as_str().len())
        .saturating_add(opt(receipt
            .prior_chunk_digest
            .as_ref()
            .map(|digest| digest.as_str())))
        .saturating_add(opt(receipt
            .current_chunk_digest
            .as_ref()
            .map(|digest| digest.as_str())))
        .saturating_add(opt(receipt
            .output_digest
            .as_ref()
            .map(|digest| digest.as_str())))
}

fn snapshot_file_heap_bytes(file: &SanitizedCodeFileV1) -> usize {
    file.file_occurrence_id
        .as_str()
        .len()
        .saturating_add(file.logical_path.capacity())
        .saturating_add(opt(file
            .language
            .as_ref()
            .map(|language| language.as_str())))
        .saturating_add(file.content_digest.as_str().len())
}

/// One file page, excluding the chunk and symbol records it shares with the
/// generation-wide index.
fn file_bytes(file: &FileGenerationArtifactsV1) -> usize {
    let authority = &file.authority;
    let extraction = &file.extraction;
    let artifacts = &file.artifacts;
    let document = &artifacts.chunks.document;
    let authority_bytes = authority
        .project_id
        .as_str()
        .len()
        .saturating_add(authority.repository_id.as_str().len())
        .saturating_add(opt(authority.worktree_id.as_ref().map(|id| id.as_str())))
        .saturating_add(opt(authority.reference.as_ref().map(|id| id.as_str())))
        .saturating_add(authority.logical_path.capacity())
        .saturating_add(authority.content_digest.as_str().len());
    let extraction_bytes = extraction
        .generation_id
        .as_str()
        .len()
        .saturating_add(extraction.file_occurrence_id.as_str().len())
        .saturating_add(extraction.language.as_str().len())
        .saturating_add(extraction.descriptor_revision.as_str().len())
        .saturating_add(extraction.grammar_revision.as_str().len())
        .saturating_add(extraction.extractor_revision.as_str().len())
        .saturating_add(extraction.content_digest.as_str().len())
        .saturating_add(
            size_of::<tracedecay_domain::SourceSpan>().saturating_mul(
                extraction
                    .parsed_ranges
                    .capacity()
                    .saturating_add(extraction.error_ranges.capacity())
                    .saturating_add(extraction.unsupported_ranges.capacity()),
            ),
        )
        .saturating_add(extraction.parser_import_rows_digest.as_str().len())
        .saturating_add(extraction.rows_digest.as_str().len());
    let document_bytes = document
        .generation_id
        .as_str()
        .len()
        .saturating_add(document.file_occurrence_id.as_str().len())
        .saturating_add(document.content_digest.as_str().len())
        .saturating_add(match &document.eligibility {
            CodeSearchEligibilityV1::Eligible => 0,
            CodeSearchEligibilityV1::Excluded { reason }
            | CodeSearchEligibilityV1::Partial { reason } => reason.capacity(),
        })
        .saturating_add(vec_bytes(
            &document.chunk_ids,
            document.chunk_ids.capacity(),
            |id| id.as_str().len(),
        ));
    let shared_handles = size_of::<Arc<CodeSearchChunkV1>>().saturating_mul(
        artifacts
            .chunks
            .chunks
            .capacity()
            .saturating_add(artifacts.symbols.capacity()),
    );
    let clone_bytes = artifacts.clone_bodies.iter().fold(
        artifacts
            .clone_bodies
            .capacity()
            .saturating_mul(size_of::<crate::clones::CodeIndexCloneBodyV1>()),
        |bytes, body| {
            bytes
                .saturating_add(ARC_HEADER_BYTES)
                .saturating_add(size_of::<crate::clones::CloneBodyPayloadV1>())
                .saturating_add(body.retained_owned_bytes())
        },
    );
    let schema_bytes = artifacts.schema_evidence.as_ref().map_or(0, |evidence| {
        evidence.logical_path.capacity().saturating_add(
            evidence
                .facts
                .capacity()
                .saturating_mul(size_of::<tracedecay_code_extraction::ExtractedSchemaFactV1>()),
        )
    });
    let exact_authority_bytes = file.exact_authority.retained_bytes();
    ARC_HEADER_BYTES
        .saturating_add(size_of::<FileGenerationArtifactsV1>())
        .saturating_add(authority_bytes)
        .saturating_add(extraction_bytes)
        .saturating_add(document_bytes)
        .saturating_add(shared_handles)
        .saturating_add(vec_bytes(
            &artifacts.edges,
            artifacts.edges.capacity(),
            edge_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            &artifacts.edge_abstentions,
            artifacts.edge_abstentions.capacity(),
            abstention_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            &artifacts.imports,
            artifacts.imports.capacity(),
            import_heap_bytes,
        ))
        .saturating_add(clone_bytes)
        .saturating_add(schema_bytes)
        .saturating_add(vec_bytes(
            &artifacts.unresolved_references,
            artifacts.unresolved_references.capacity(),
            unresolved_heap_bytes,
        ))
        .saturating_add(exact_authority_bytes)
}

impl CodeIndexPublishedGenerationV1 {
    pub(super) fn measure_resident_bytes(&self) -> usize {
        let snapshot = &self.snapshot;
        let request = self.projection.request();
        let receipt = self.projection.receipt();
        let chunk_records = self.chunks.chunks().iter().fold(0_usize, |bytes, chunk| {
            bytes.saturating_add(chunk_bytes(chunk))
        });
        let symbol_records = self.symbols.symbols.iter().fold(0_usize, |bytes, symbol| {
            bytes.saturating_add(symbol_bytes(symbol))
        });
        let files = self.files.iter().fold(0_usize, |bytes, file| {
            bytes.saturating_add(file_bytes(file))
        });
        let index_handles = size_of::<Arc<CodeSearchChunkV1>>().saturating_mul(
            self.chunks
                .chunks()
                .len()
                .saturating_add(self.symbols.symbols.capacity())
                .saturating_add(self.files.capacity()),
        );
        let projection = vec_bytes(
            &request.changes.added_or_changed,
            request.changes.added_or_changed.capacity(),
            changed_chunk_heap_bytes,
        )
        .saturating_add(vec_bytes(
            &request.changes.deleted,
            request.changes.deleted.capacity(),
            changed_chunk_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            &receipt.receipts,
            receipt.receipts.capacity(),
            receipt_heap_bytes,
        ));
        let snapshot_bytes = vec_bytes(&snapshot.files, snapshot.files.capacity(), |file| {
            snapshot_file_heap_bytes(file)
        })
        .saturating_add(vec_bytes(
            &snapshot.sanitization_receipts,
            snapshot.sanitization_receipts.capacity(),
            |receipt| receipt.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            &self.capability.sanitization_receipts,
            self.capability.sanitization_receipts.capacity(),
            |receipt| receipt.as_str().len(),
        ));
        size_of::<Self>()
            .saturating_add(chunk_records)
            .saturating_add(symbol_records)
            .saturating_add(files)
            .saturating_add(index_handles)
            .saturating_add(vec_bytes(
                &self.edges,
                self.edges.capacity(),
                edge_heap_bytes,
            ))
            .saturating_add(vec_bytes(
                &self.edge_abstentions,
                self.edge_abstentions.capacity(),
                abstention_heap_bytes,
            ))
            .saturating_add(vec_bytes(
                &self.imports,
                self.imports.capacity(),
                import_heap_bytes,
            ))
            .saturating_add(vec_bytes(
                &self.lineage,
                self.lineage.capacity(),
                lineage_heap_bytes,
            ))
            .saturating_add(projection)
            .saturating_add(snapshot_bytes)
    }
}
