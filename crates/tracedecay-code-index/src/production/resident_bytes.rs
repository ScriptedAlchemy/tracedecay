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

use tracedecay_code_extraction::{
    GoMethodSetRowV1, GoMethodSignatureV1, GoTypeTokenV1, GoTypeV1, ImportReexportScopeV1,
};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, ChangedCodeChunkV1, CodeSearchChunkV1, ExactTechnicalTermV1,
    ProjectionKeyV1, SanitizedCodeFileV1, SymbolOccurrenceId,
};

use super::{
    CodeIndexPublishedGenerationV1, DecodedGenerationContentV1, FileGenerationArtifactsV1,
};
use crate::chunks::{
    CodeIndexEdgeAbstentionV1, CodeIndexGoMethodSetRowV1, CodeIndexImportEvidenceV1,
    CodeIndexUnresolvedReferenceV1, CodeSearchEligibilityV1,
};
use crate::lineage::{LineageSymbolRecordV1, SymbolLineageCandidateV1};
use crate::projection::ProjectionPublicationHandoffV1;

/// An `Arc` allocation: two reference counts before the value.
pub(crate) const ARC_HEADER_BYTES: usize = 2 * size_of::<usize>();

pub(crate) fn vec_bytes<T>(values: &[T], capacity: usize, heap: impl Fn(&T) -> usize) -> usize {
    values
        .iter()
        .fold(capacity.saturating_mul(size_of::<T>()), |bytes, value| {
            bytes.saturating_add(heap(value))
        })
}

pub(crate) fn opt(value: Option<&str>) -> usize {
    value.map_or(0, str::len)
}

/// Control bytes a SwissTable probes per group: SSE2 groups on x86, NEON
/// groups on aarch64, one machine word otherwise.
const SWISS_GROUP_WIDTH: usize = if cfg!(all(
    any(target_arch = "x86", target_arch = "x86_64"),
    target_feature = "sse2"
)) {
    16
} else if cfg!(all(target_arch = "aarch64", target_feature = "neon")) {
    8
} else {
    size_of::<usize>()
};

/// The pinned SwissTable capacity class, including its entry-size-dependent
/// minimum allocation. Callers reserve this class before asking std to grow.
pub(crate) fn hash_table_capacity<T>(
    requested: usize,
) -> Result<usize, super::CodeIndexProductionErrorV1> {
    if requested == 0 {
        return Ok(0);
    }
    let minimum = match (SWISS_GROUP_WIDTH, size_of::<T>()) {
        (16, 0..=1) => 14,
        (16, 2..=3) | (8, 0..=1) => 7,
        _ => 3,
    };
    let requested = requested.max(minimum);
    let buckets = if requested < 4 {
        4
    } else if requested < 8 {
        8
    } else if requested < 15 {
        16
    } else {
        requested
            .checked_mul(8)
            .and_then(|value| (value / 7).checked_next_power_of_two())
            .ok_or_else(|| {
                super::CodeIndexProductionErrorV1::Contract(
                    "resolution hash capacity overflowed".to_owned(),
                )
            })?
    };
    // Validate the same padded entry/control layout before callers compute
    // its byte charge or ask the allocator for it.
    let layout_error = |error| {
        super::CodeIndexProductionErrorV1::Contract(format!(
            "resolution hash layout is invalid: {error}"
        ))
    };
    let entries = std::alloc::Layout::array::<T>(buckets).map_err(layout_error)?;
    let controls = buckets.checked_add(SWISS_GROUP_WIDTH).ok_or_else(|| {
        super::CodeIndexProductionErrorV1::Contract("resolution hash layout overflowed".to_owned())
    })?;
    let controls = std::alloc::Layout::array::<u8>(controls).map_err(layout_error)?;
    let _layout = entries
        .align_to(SWISS_GROUP_WIDTH)
        .map_err(layout_error)?
        .pad_to_align()
        .extend(controls)
        .map_err(layout_error)?;
    Ok(if buckets < 8 {
        buckets - 1
    } else {
        buckets / 8 * 7
    })
}

/// The table a std `HashMap<K, V>` or `HashSet<T>` (with `T = (K, V)` or the
/// element) of the given `capacity()` allocates. std's map is a SwissTable
/// whose capacity is all but one of a power-of-two bucket count below eight
/// buckets and seven eighths of it from eight up; the one allocation holds
/// every bucket's entry, padded to the control alignment, then a control
/// byte per bucket and one trailing group.
pub(crate) fn hash_table_bytes<T>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        capacity + 1
    } else {
        capacity / 7 * 8
    };
    let control_align = std::mem::align_of::<T>().max(SWISS_GROUP_WIDTH);
    (size_of::<T>() * buckets).next_multiple_of(control_align) + buckets + SWISS_GROUP_WIDTH
}

/// Allocation bound for the workspace's word-aligned BTreeMap/Set entries.
/// std's degree-six tree has eleven key/value slots per node, at least five
/// occupied slots in every non-root node, and at least one in the root.
/// Leaf headers fit two words; tuple entry sizes also cover separate-array
/// padding. Internal nodes additionally hold twelve child pointers. With N
/// nodes, at most (N + 3) / 6 are internal (two root children, six thereafter).
/// Unlike an average occupancy estimate, this admits the first leaf and splits
/// before they allocate, including the mostly empty root of a small map.
pub(crate) fn btree_bytes(entries: usize, entry_bytes: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    let nodes = entries.saturating_sub(1) / 5 + 1;
    let leaf = size_of::<usize>()
        .saturating_mul(2)
        .saturating_add(entry_bytes.saturating_mul(11));
    nodes.saturating_mul(leaf).saturating_add(
        (nodes.saturating_add(3) / 6)
            .saturating_mul(12)
            .saturating_mul(size_of::<usize>()),
    )
}

pub(super) fn chunk_bytes(chunk: &CodeSearchChunkV1) -> usize {
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

pub(super) fn symbol_bytes(symbol: &LineageSymbolRecordV1) -> usize {
    ARC_HEADER_BYTES
        .saturating_add(size_of::<LineageSymbolRecordV1>())
        .saturating_add(symbol_heap_bytes(symbol))
}

/// The strings and lists a symbol record owns, beyond its inline body.
pub(crate) fn symbol_heap_bytes(symbol: &LineageSymbolRecordV1) -> usize {
    symbol
        .occurrence
        .as_str()
        .len()
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

pub(super) fn edge_heap_bytes(edge: &CanonicalRelationEdgeV1) -> usize {
    edge_endpoint_heap_bytes(&edge.from_occurrence, &edge.to_occurrence)
}

pub(super) fn edge_endpoint_heap_bytes(
    from: &SymbolOccurrenceId,
    to: &SymbolOccurrenceId,
) -> usize {
    from.as_str().len().saturating_add(to.as_str().len())
}

pub(super) fn abstention_heap_bytes(abstention: &CodeIndexEdgeAbstentionV1) -> usize {
    abstention
        .source_node_id
        .capacity()
        .saturating_add(abstention.target_node_id.capacity())
}

pub(crate) fn import_heap_bytes(import: &CodeIndexImportEvidenceV1) -> usize {
    import
        .logical_path
        .capacity()
        .saturating_add(import.file_occurrence_id.as_str().len())
        .saturating_add(import.module_specifier.capacity())
        .saturating_add(opt(import.imported_name.as_deref()))
        .saturating_add(opt(import.local_name.as_deref()))
        .saturating_add(match &import.reexport_scope {
            Some(ImportReexportScopeV1::Module(module)) => module.capacity(),
            _ => 0,
        })
}

pub(crate) fn unresolved_heap_bytes(reference: &CodeIndexUnresolvedReferenceV1) -> usize {
    reference
        .from_occurrence
        .as_str()
        .len()
        .saturating_add(reference.reference_name.capacity())
}

pub(super) fn go_method_set_heap_bytes(row: &CodeIndexGoMethodSetRowV1) -> usize {
    fn type_bytes(ty: &GoTypeV1) -> usize {
        vec_bytes(ty, ty.capacity(), |token| match token {
            GoTypeTokenV1::Text(text) | GoTypeTokenV1::Local(text) => text.capacity(),
            GoTypeTokenV1::Qualified { package, name } => {
                package.capacity().saturating_add(name.capacity())
            }
        })
    }
    fn signature_bytes(method: &GoMethodSignatureV1) -> usize {
        method
            .name
            .capacity()
            .saturating_add(vec_bytes(
                &method.params,
                method.params.capacity(),
                type_bytes,
            ))
            .saturating_add(vec_bytes(
                &method.results,
                method.results.capacity(),
                type_bytes,
            ))
    }
    let row_bytes = match &row.row {
        GoMethodSetRowV1::Receiver {
            type_name, method, ..
        } => type_name.capacity().saturating_add(signature_bytes(method)),
        GoMethodSetRowV1::InterfaceMethod { method } => signature_bytes(method),
        GoMethodSetRowV1::Embeds { embedded } | GoMethodSetRowV1::Promotes { embedded } => {
            type_bytes(embedded)
        }
        GoMethodSetRowV1::Field { name } => name.capacity(),
        GoMethodSetRowV1::GenericInterface
        | GoMethodSetRowV1::EmptyInterface
        | GoMethodSetRowV1::NamedType => 0,
    };
    row.occurrence.as_str().len().saturating_add(row_bytes)
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

pub(super) fn changed_chunk_heap_bytes(change: &ChangedCodeChunkV1) -> usize {
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

/// Heap cloned by one replayed public receipt; rows are consumed one at a time.
pub(super) fn projection_receipt_heap_bound(
    request: &tracedecay_domain::ProjectionBatchRequestV1,
) -> usize {
    let header = projection_key_bytes(&request.target_projection_key)
        .saturating_add(request.request_digest.as_str().len())
        .saturating_add(opt(request
            .changes
            .from_generation
            .as_ref()
            .map(|id| id.as_str())))
        .saturating_add(request.changes.to_generation.as_str().len())
        .saturating_add(request.changes.manifest_digest.as_str().len());
    request
        .changes
        .added_or_changed
        .iter()
        .chain(&request.changes.deleted)
        .map(|change| {
            header
                .saturating_add(changed_chunk_heap_bytes(change))
                .saturating_add(opt(change
                    .current_digest
                    .as_ref()
                    .map(|digest| digest.as_str())))
                // The order verifier holds the preceding chunk id beside this row.
                .saturating_add(change.chunk_id.as_str().len())
        })
        .max()
        .unwrap_or(0)
}

/// Workspace for cloning the already pinned intake registry, validating its
/// three lookup rosters, and emitting a capability. All variable terms come
/// from those descriptors; no complete compiled registry is reconstructed.
pub(super) fn capability_construction_workspace(
    descriptors: &[&tracedecay_domain::LanguageDescriptorV1],
    snapshot: &tracedecay_domain::SanitizedCodeSnapshotV1,
    manifest: &tracedecay_domain::CodeGenerationManifestV1,
) -> usize {
    let mut descriptor_heap = 0_usize;
    let mut lookup_heap = 0_usize;
    let mut aliases = 0_usize;
    let mut extensions = 0_usize;
    for descriptor in descriptors {
        descriptor_heap = descriptor_heap
            .saturating_add(descriptor.language.as_str().len())
            .saturating_add(descriptor.descriptor_revision.as_str().len())
            .saturating_add(descriptor.grammar_revision.as_str().len())
            .saturating_add(descriptor.extractor_revision.as_str().len());
        for values in [
            &descriptor.aliases,
            &descriptor.extensions,
            &descriptor.root_markers,
        ] {
            descriptor_heap = descriptor_heap
                .saturating_add(values.len().saturating_mul(size_of::<String>()))
                .saturating_add(values.iter().map(String::len).sum::<usize>());
        }
        aliases = aliases.saturating_add(descriptor.aliases.len());
        extensions = extensions.saturating_add(descriptor.extensions.len());
        lookup_heap = lookup_heap
            .saturating_add(descriptor.language.as_str().len())
            .saturating_add(descriptor.aliases.iter().map(String::len).sum::<usize>())
            .saturating_add(descriptor.extensions.iter().map(String::len).sum::<usize>());
    }
    let tables =
        [descriptors.len(), aliases, extensions]
            .into_iter()
            .fold(0_usize, |bytes, count| {
                bytes
                    .saturating_add(
                        hash_table_bytes::<(String, usize)>(
                            super::helpers::resolution_hash_capacity(count),
                        )
                        // Growing maps can retain their previous table during rehash.
                        .saturating_mul(2),
                    )
                    .saturating_add(btree_bytes(count, size_of::<String>()))
            });
    let receipts = vec_bytes(
        &snapshot.sanitization_receipts,
        snapshot.sanitization_receipts.len(),
        |id| id.as_str().len(),
    );
    let emitter_lists = crate::capabilities::BASE_EXACT_TERM_KINDS
        .len()
        .saturating_mul(size_of::<tracedecay_domain::ExactTechnicalTermKindV1>())
        .saturating_add(
            crate::capabilities::BASE_EDGE_AUTHORITY_CLASSES
                .len()
                .saturating_mul(size_of::<tracedecay_domain::EdgeAuthorityV1>()),
        );
    let emitted = descriptors.iter().fold(0_usize, |bytes, descriptor| {
        bytes
            .saturating_add(size_of::<tracedecay_domain::LanguageId>())
            .saturating_add(size_of::<tracedecay_domain::LanguageDescriptorRevision>())
            .saturating_add(descriptor.language.as_str().len())
            .saturating_add(descriptor.descriptor_revision.as_str().len())
    });
    descriptors
        .len()
        .saturating_add(descriptors.len().div_ceil(2))
        .saturating_mul(size_of::<tracedecay_domain::LanguageDescriptorV1>())
        .saturating_add(descriptor_heap)
        .saturating_add(tables)
        // Validation owns normalized keys beside lookup keys. Static descriptor
        // keys are ASCII; these are the same pinned inputs intake admitted.
        .saturating_add(lookup_heap.saturating_mul(3))
        .saturating_add(receipts.saturating_mul(3))
        .saturating_add(emitter_lists.saturating_mul(2))
        .saturating_add(emitted)
        .saturating_add(
            descriptors
                .len()
                .saturating_mul(2)
                .saturating_mul(size_of::<&tracedecay_domain::LanguageDescriptorV1>()),
        )
        .saturating_add(
            tracedecay_domain::CodeSearchChunkGrainV1::ORDER
                .len()
                .saturating_mul(2)
                .saturating_mul(size_of::<tracedecay_domain::CodeSearchChunkGrainV1>()),
        )
        .saturating_add(
            crate::capabilities::CHUNK_SCHEMA_REVISION_V1
                .len()
                .saturating_mul(2),
        )
        .saturating_add(manifest.generation_id.as_str().len())
        .saturating_add(manifest.chunker_revision.as_str().len())
        .saturating_add(manifest.privacy_domain.as_str().len())
        // Registry revision derivation owns the tagged hash, sixteen hex
        // characters and the prefixed revision concurrently; capability
        // construction also replaces one placeholder tagged digest.
        .saturating_add(71 * 2 + 16 * 2 + ("registry.v1.".len() + 16) * 2)
}

pub(super) fn capability_heap_bytes(
    capability: &tracedecay_domain::CodeIndexCapabilityManifestV1,
) -> usize {
    capability
        .generation_id
        .as_str()
        .len()
        .saturating_add(capability.chunk_schema_revision.capacity())
        .saturating_add(capability.chunker_revision.as_str().len())
        .saturating_add(capability.privacy_domain.as_str().len())
        .saturating_add(capability.manifest_digest.as_str().len())
        .saturating_add(vec_bytes(
            &capability.language_descriptor_revisions,
            capability.language_descriptor_revisions.capacity(),
            |id| id.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            &capability.available_grains,
            capability.available_grains.capacity(),
            |_| 0,
        ))
        .saturating_add(vec_bytes(
            &capability.exact_term_kinds,
            capability.exact_term_kinds.capacity(),
            |_| 0,
        ))
        .saturating_add(vec_bytes(
            &capability.supported_languages,
            capability.supported_languages.capacity(),
            |id| id.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            &capability.edge_authority_classes,
            capability.edge_authority_classes.capacity(),
            |_| 0,
        ))
        .saturating_add(vec_bytes(
            &capability.sanitization_receipts,
            capability.sanitization_receipts.capacity(),
            |id| id.as_str().len(),
        ))
}

/// Allocations retained by the projection capability, including compact
/// restored outputs. Struct bodies are charged by the containing generation.
pub(super) fn projection_heap_bytes(projection: &ProjectionPublicationHandoffV1) -> usize {
    let request = projection.request();
    let changes = &request.changes;
    vec_bytes(
        &changes.added_or_changed,
        changes.added_or_changed.capacity(),
        changed_chunk_heap_bytes,
    )
    .saturating_add(vec_bytes(
        &changes.deleted,
        changes.deleted.capacity(),
        changed_chunk_heap_bytes,
    ))
    .saturating_add(vec_bytes(
        projection.output_overrides(),
        projection.output_override_capacity(),
        |(_, digest)| digest.as_str().len(),
    ))
    .saturating_add(request.request_digest.as_str().len())
    .saturating_add(projection_key_bytes(&request.target_projection_key))
    .saturating_add(
        request
            .previous_projection_key
            .as_ref()
            .map_or(0, projection_key_bytes),
    )
    .saturating_add(opt(changes.from_generation.as_ref().map(|id| id.as_str())))
    .saturating_add(changes.to_generation.as_str().len())
    .saturating_add(changes.manifest_digest.as_str().len())
    .saturating_add(changes.reused_digest.as_str().len())
    .saturating_add(projection.publication_digest().as_str().len())
    .saturating_add(ARC_HEADER_BYTES)
    .saturating_add(projection.shared_value_size())
}

pub(crate) fn snapshot_file_heap_bytes(file: &SanitizedCodeFileV1) -> usize {
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

/// Descriptor allocations that remain live after cold resolution drops its facts.
pub(super) fn file_segment_descriptors_bytes(
    files: &[super::partitioned_codec::PartitionedFileSegmentDescriptorV1],
    capacity: usize,
) -> usize {
    vec_bytes(files, capacity, |file| {
        file.file_occurrence_id
            .as_str()
            .len()
            .saturating_add(file.segment_digest.as_str().len())
            .saturating_add(file.symbol_identities_digest.as_str().len())
    })
}

pub(super) fn staged_resolution_descriptors_bytes(
    parts: &super::sparse_successor::StagedResolutionPartsV1,
) -> usize {
    vec_bytes(
        &parts.file_evidence,
        parts.file_evidence.capacity(),
        |file| {
            file.file_occurrence_id
                .as_str()
                .len()
                .saturating_add(file.segment_digest.as_str().len())
        },
    )
    .saturating_add(vec_bytes(
        &parts.code_graph_pages,
        parts.code_graph_pages.capacity(),
        |page| {
            page.file_occurrence_id
                .as_str()
                .len()
                .saturating_add(page.logical_path.capacity())
                .saturating_add(page.page_digest.as_str().len())
        },
    ))
    .saturating_add(resolution_index_descriptor_bytes(&parts.resolution_index))
}

fn resolution_index_descriptor_bytes(
    index: &super::resolution_index::PartitionedResolutionIndexDescriptorV1,
) -> usize {
    // Cold index sealing reserves each private page vector at its exact
    // final page count. The import-alias singleton is inline.
    let count = index.segments().count();
    count
        .saturating_sub(1)
        .saturating_mul(size_of::<
            super::resolution_index::PartitionedIndexSegmentDescriptorV1,
        >())
        .saturating_add(
            index
                .segments()
                .map(|segment| segment.segment_digest.as_str().len())
                .sum::<usize>(),
        )
}

pub(super) fn generation_evidence_descriptor_bytes(
    evidence: &super::partitioned_codec::PartitionedGenerationEvidenceDescriptorV1,
) -> usize {
    evidence
        .segment_digest
        .as_str()
        .len()
        .saturating_add(vec_bytes(
            &evidence.pages,
            evidence.pages.capacity(),
            |page| page.page_digest.as_str().len(),
        ))
}

pub(super) fn snapshot_heap_bytes(snapshot: &tracedecay_domain::SanitizedCodeSnapshotV1) -> usize {
    snapshot
        .repository
        .as_str()
        .len()
        .saturating_add(opt(snapshot.worktree.as_ref().map(|id| id.as_str())))
        .saturating_add(opt(snapshot.reference.as_ref().map(|id| id.as_str())))
        .saturating_add(opt(snapshot.source_revision.as_ref().map(|id| id.as_str())))
        .saturating_add(snapshot.sanitizer_revision.as_str().len())
        .saturating_add(snapshot.content_identity.as_str().len())
        .saturating_add(vec_bytes(
            &snapshot.files,
            snapshot.files.capacity(),
            snapshot_file_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            &snapshot.sanitization_receipts,
            snapshot.sanitization_receipts.capacity(),
            |id| id.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            &snapshot.omitted_sources,
            snapshot.omitted_sources.capacity(),
            |source| {
                source
                    .git_path
                    .capacity()
                    .saturating_add(match &source.reason {
                        tracedecay_domain::CodeSourceOmissionReasonV1::PrivacyWithheld {
                            detail,
                        } => detail.capacity(),
                        tracedecay_domain::CodeSourceOmissionReasonV1::UnrepresentablePath => 0,
                    })
            },
        ))
}

fn manifest_heap_bytes(manifest: &tracedecay_domain::CodeGenerationManifestV1) -> usize {
    let sources = manifest.source_commitments.as_ref().map_or(0, |sources| {
        sources
            .incremental_manifest_digest
            .as_str()
            .len()
            .saturating_add(sources.full_replay_digest.as_str().len())
            .saturating_add(opt(sources
                .parent_full_replay_digest
                .as_ref()
                .map(|id| id.as_str())))
    });
    manifest
        .project_id
        .as_str()
        .len()
        .saturating_add(manifest.generation_id.as_str().len())
        .saturating_add(manifest.snapshot_digest.as_str().len())
        .saturating_add(manifest.invalidation_digest.as_str().len())
        .saturating_add(manifest.registry_revision.as_str().len())
        .saturating_add(manifest.sanitizer_revision.as_str().len())
        .saturating_add(manifest.chunker_revision.as_str().len())
        .saturating_add(manifest.privacy_domain.as_str().len())
        .saturating_add(opt(manifest
            .parent_generation
            .as_ref()
            .map(|id| id.as_str())))
        .saturating_add(manifest.seal.expected_digest.as_str().len())
        .saturating_add(manifest.seal.planner.as_str().len())
        .saturating_add(sources)
        .saturating_add(vec_bytes(
            &manifest.grammar_revisions,
            manifest.grammar_revisions.capacity(),
            |(language, revision)| {
                language
                    .as_str()
                    .len()
                    .saturating_add(revision.as_str().len())
            },
        ))
        .saturating_add(vec_bytes(
            &manifest.extractor_revisions,
            manifest.extractor_revisions.capacity(),
            |(language, revision)| {
                language
                    .as_str()
                    .len()
                    .saturating_add(revision.as_str().len())
            },
        ))
}

/// Serde's sequence visitors can hold an old vector beside its doubled
/// replacement. Count inline slots separately from string/byte payloads: the
/// typed JSON length bounds those payloads and the unescape scratch buffer.
/// This is a pre-decode bound over this exact manifest, not a JSON multiplier
/// standing in for a decoded object tree.
pub(super) fn partitioned_decode_workspace_bytes(
    generation: &super::partitioned_codec::PartitionedPublishedGenerationRefV1<'_>,
    json_bytes: usize,
    string_scratch: usize,
) -> usize {
    fn slots<T>(values: &[T]) -> usize {
        // Vec's minimum nonzero allocation for these DTO elements is four.
        if values.is_empty() {
            0
        } else {
            values
                .len()
                .max(4)
                .saturating_mul(3)
                .saturating_mul(size_of::<T>())
        }
    }
    let snapshot = generation.snapshot;
    let capability = generation.capability;
    let index_count = generation
        .resolution_index
        .segments()
        .count()
        .saturating_sub(1);
    let inline = slots(&snapshot.files)
        .saturating_add(slots(&snapshot.sanitization_receipts))
        .saturating_add(slots(&snapshot.omitted_sources))
        .saturating_add(
            snapshot
                .omitted_sources
                .iter()
                .map(|source| {
                    if source.git_path.is_empty() {
                        0
                    } else {
                        source.git_path.len().max(8).saturating_mul(3)
                    }
                })
                .sum::<usize>(),
        )
        .saturating_add(slots(&generation.manifest.grammar_revisions))
        .saturating_add(slots(&generation.manifest.extractor_revisions))
        .saturating_add(slots(generation.ignored_source_admissions))
        .saturating_add(slots(generation.file_segments))
        .saturating_add(slots(generation.file_evidence))
        .saturating_add(slots(generation.code_graph_pages))
        .saturating_add(slots(&generation.generation_evidence.pages))
        .saturating_add(
            index_count
                .max(8)
                .saturating_mul(3)
                .saturating_mul(size_of::<
                    super::resolution_index::PartitionedIndexSegmentDescriptorV1,
                >()),
        )
        .saturating_add(slots(&capability.language_descriptor_revisions))
        .saturating_add(slots(&capability.available_grains))
        .saturating_add(slots(&capability.exact_term_kinds))
        .saturating_add(slots(&capability.supported_languages))
        .saturating_add(slots(&capability.edge_authority_classes))
        .saturating_add(slots(&capability.sanitization_receipts));
    let present = generation.file_segments.len();
    let validation = present
        .max(4)
        .saturating_mul(3)
        .saturating_mul(size_of::<(usize, &tracedecay_domain::FileOccurrenceId)>())
        // HashMap rehash can retain the preceding table beside its replacement.
        .saturating_add(
            hash_table_bytes::<(usize, &tracedecay_domain::FileOccurrenceId)>(
                super::helpers::resolution_hash_capacity(present),
            )
            .saturating_mul(2),
        )
        .saturating_add(btree_bytes(
            snapshot.files.len(),
            size_of::<&tracedecay_domain::FileOccurrenceId>(),
        ))
        .saturating_add(btree_bytes(snapshot.files.len(), size_of::<&String>()));
    let cloned_sources = generation
        .ignored_source_admissions
        .len()
        .saturating_mul(size_of::<super::CodeIndexIgnoredSourceAdmissionV1>())
        .saturating_add(
            generation
                .ignored_source_admissions
                .iter()
                .map(|source| source.logical_path.len())
                .sum::<usize>(),
        )
        .saturating_add(opt(generation
            .repository_parse_identity
            .tree
            .as_ref()
            .map(|id| id.as_str())))
        .saturating_add(generation.ignored_source_admissions_digest.as_str().len());
    // All decoded scalar payloads, then one unescape buffer bounded by the
    // largest known DTO row. Its geometric growth can overlap old/new buffers.
    json_bytes
        .saturating_add(string_scratch.saturating_mul(3))
        .saturating_add(inline)
        .saturating_add(validation)
        .saturating_add(cloned_sources)
        .saturating_add(ARC_HEADER_BYTES)
        .saturating_add(size_of::<super::VerifiedSealedTextGenerationMetadataV1>())
        .saturating_add(71) // The newly computed lane digest survives the parsed descriptors.
}

pub(super) fn chunk_policy_heap_bytes(policy: &super::ChunkPolicyRevisionSummaryV1) -> usize {
    match policy {
        super::ChunkPolicyRevisionSummaryV1::Uniform(revision) => revision.as_str().len(),
        super::ChunkPolicyRevisionSummaryV1::Empty | super::ChunkPolicyRevisionSummaryV1::Mixed => {
            0
        }
    }
}

/// All compact metadata is owned beside the sealed bytes and shared projection.
/// The policy summary owns one policy identity when the census is uniform.
pub(super) fn compact_metadata_bytes(
    metadata: &super::VerifiedSealedTextGenerationMetadataV1,
    policy_bytes: usize,
) -> usize {
    ARC_HEADER_BYTES
        .saturating_add(size_of::<super::VerifiedSealedTextGenerationMetadataV1>())
        .saturating_add(manifest_heap_bytes(metadata.manifest()))
        .saturating_add(snapshot_heap_bytes(metadata.snapshot()))
        .saturating_add(opt(metadata
            .repository_parse_identity()
            .tree
            .as_ref()
            .map(|id| id.as_str())))
        .saturating_add(metadata.ignored_source_admissions_digest().as_str().len())
        .saturating_add(vec_bytes(
            metadata.ignored_source_admissions(),
            metadata.ignored_source_admissions().len(),
            |source| source.logical_path.capacity(),
        ))
        .saturating_add(policy_bytes)
}

/// Retained allocations of a compact publication, including its shared
/// construction guard's own Arc allocation. Caller-owned inline bodies are
/// excluded; cloned publications share all three Arc allocations.
pub(super) fn sealed_publication_bytes(
    publication: &super::CodeIndexSealedPublicationV1,
    policy_bytes: usize,
) -> usize {
    projection_heap_bytes(&publication.projection)
        .saturating_add(publication.manifest_bytes.len())
        .saturating_add(ARC_HEADER_BYTES)
        .saturating_add(publication.lane_digest.as_str().len())
        .saturating_add(compact_metadata_bytes(&publication.metadata, policy_bytes))
        .saturating_add(ARC_HEADER_BYTES)
        .saturating_add(size_of::<super::ConstructionMemoryV1>())
}

/// A page's authority, extraction record, and document header: what every
/// page keeps, reduced or not.
pub(super) fn page_identity_bytes(file: &FileGenerationArtifactsV1) -> usize {
    let authority = &file.authority;
    let extraction = &file.extraction;
    let document = &file.artifacts.chunks.document;
    let authority_bytes = authority
        .project_id
        .as_str()
        .len()
        .saturating_add(authority.repository_id.as_str().len())
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
        });
    authority_bytes
        .saturating_add(extraction_bytes)
        .saturating_add(document_bytes)
}

/// One file page, excluding the chunk and symbol records it shares with the
/// generation-wide index.
pub(super) fn file_bytes(file: &FileGenerationArtifactsV1) -> usize {
    let artifacts = &file.artifacts;
    let document = &artifacts.chunks.document;
    let identity_bytes = page_identity_bytes(file).saturating_add(vec_bytes(
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
        .saturating_add(identity_bytes)
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
        .saturating_add(vec_bytes(
            &artifacts.callable_arities,
            artifacts.callable_arities.capacity(),
            |row| row.occurrence.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            &artifacts.go_method_sets,
            artifacts.go_method_sets.capacity(),
            go_method_set_heap_bytes,
        ))
        .saturating_add(exact_authority_bytes)
}

impl CodeIndexPublishedGenerationV1 {
    pub(super) fn measure_resident_bytes(&self) -> usize {
        let snapshot = &self.snapshot;
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
        let projection = projection_heap_bytes(&self.projection);
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
                &self.unresolved_calls,
                self.unresolved_calls.capacity(),
                unresolved_heap_bytes,
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

impl DecodedGenerationContentV1 {
    /// Every page with the chunk and symbol records it holds; a generation
    /// over these pages counts the same records through its own index.
    pub(super) fn measure_resident_bytes(&self) -> usize {
        self.files.iter().fold(
            size_of::<Self>()
                .saturating_add(self.files.capacity().saturating_mul(size_of::<Arc<()>>())),
            |bytes, file| {
                let records = file
                    .artifacts
                    .chunks
                    .chunks
                    .iter()
                    .map(|chunk| chunk_bytes(chunk))
                    .chain(
                        file.artifacts
                            .symbols
                            .iter()
                            .map(|symbol| symbol_bytes(symbol)),
                    )
                    .fold(0_usize, usize::saturating_add);
                bytes
                    .saturating_add(file_bytes(file))
                    .saturating_add(records)
            },
        )
    }
}
