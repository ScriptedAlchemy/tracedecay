use std::collections::{BTreeMap, BTreeSet};

use roaring::RoaringBitmap;
use sha2::{Digest, Sha256};
use tracedecay_code_index::clones::{
    CloneExactKeyV1, CloneFingerprintPositionV1, CloneNormalizationClassV1, CodeIndexCloneBodyV1,
};
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, VerifiedSealedLexicalPageV1};
use tracedecay_domain::{ExactFieldV1, ManifestDigest};

use super::super::{
    CodeLexicalProjectionMetadataV1, ProjectedChunkV1, canonical_projected_exact_term,
    exact_field_for_kind, normalized_search_text,
};
use super::format::{
    ArtifactRowV1, BASE_SECTION_NAMES, PageBaseSectionReceiptBuilderV1, encode_exact_field,
    encode_field, encode_ngram_bitmap, encode_page_base_sections_receipt, ngram_page_digest,
};
use super::postings::{NGRAM_NORMALIZED, NGRAM_RAW_OVERRIDE, document_ngrams};
use super::row_codec::{RowDictionaryTableV1, encode_artifact_row};
use super::schema::LexicalArtifactLayoutV1;
use super::{
    CodeLexicalArtifactErrorV1, NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1, checkpoint,
};

/// Amortized per-entry b-tree node overhead charged on top of each dictionary
/// entry's key/value payload (same constant the builder ledger uses).
const BTREE_MAP_ENTRY_OVERHEAD_BYTES: usize = 16;

#[derive(Debug)]
pub struct PreparedCodeLexicalArtifactPageV1 {
    pub(super) page_ordinal: u64,
    pub(super) page_digest: ManifestDigest,
    pub(super) cumulative_digest: ManifestDigest,
    pub(super) chunk_count: u64,
    pub(super) payload_bytes: u64,
    pub(super) import_count: u64,
    pub(super) import_payload_bytes: u64,
    pub(super) import_dictionary_digest: ManifestDigest,
    pub(super) previous_cursor: Option<Vec<u8>>,
    pub(super) next_cursor: Vec<u8>,
    pub(super) imports: Vec<PreparedImportV1>,
    pub(super) clone_bodies: Vec<PreparedCloneBodyV1>,
    pub(super) documents: Vec<PreparedDocumentV1>,
    pub(super) ngram_shards: Vec<PreparedNgramShardV1>,
    pub(super) ngram_digest: ManifestDigest,
    pub(super) base_sections_receipt: Vec<u8>,
    /// Every dictionary entry this page's rows reference (revision 14);
    /// empty for layouts whose rows carry their strings inline.
    pub(super) row_dictionary: RowDictionaryTableV1,
    source_retained_bytes: usize,
    prepared_retained_bytes: usize,
    preparation_scratch_bytes: usize,
    estimated_write_rows: usize,
    estimated_write_bytes: usize,
}

impl PreparedCodeLexicalArtifactPageV1 {
    pub fn page_ordinal(&self) -> u64 {
        self.page_ordinal
    }

    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    pub fn source_retained_bytes(&self) -> usize {
        self.source_retained_bytes
    }

    pub fn retained_owned_bytes(&self) -> usize {
        self.prepared_retained_bytes
    }

    pub fn preparation_scratch_bytes(&self) -> usize {
        self.preparation_scratch_bytes
    }

    pub fn estimated_write_rows(&self) -> usize {
        self.estimated_write_rows
    }

    pub fn estimated_write_bytes(&self) -> usize {
        self.estimated_write_bytes
    }

    pub fn ledger_charge_bytes(&self) -> Result<usize, CodeLexicalArtifactErrorV1> {
        self.source_retained_bytes
            .checked_add(self.prepared_retained_bytes)
            .and_then(|bytes| bytes.checked_add(self.preparation_scratch_bytes))
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical page ledger charge overflowed".to_owned(),
                )
            })
    }
}

#[derive(Debug)]
pub(super) struct PreparedImportV1 {
    pub(super) canonical: Vec<u8>,
    pub(super) integrity_digest: ManifestDigest,
}

#[derive(Debug)]
pub(super) struct PreparedCloneBodyV1 {
    pub(super) payload_digest: String,
    pub(super) payload: Vec<u8>,
    pub(super) occurrence: Vec<u8>,
    pub(super) symbol_occurrence_id: String,
    pub(super) path: String,
    pub(super) body_start: u64,
    pub(super) body_end: u64,
    pub(super) exact_keys: Vec<CloneExactKeyV1>,
    pub(super) fingerprint_stream: Option<PreparedCloneFingerprintStreamV1>,
}

#[derive(Debug)]
pub(super) struct PreparedCloneFingerprintStreamV1 {
    pub(super) language: String,
    pub(super) class: CloneNormalizationClassV1,
    pub(super) normalization_revision: u16,
    pub(super) body_digest: String,
    pub(super) positions: Vec<CloneFingerprintPositionV1>,
}

#[derive(Debug)]
pub(super) struct PreparedDocumentV1 {
    pub(super) document_id: i64,
    pub(super) chunk_id: String,
    pub(super) row: Vec<u8>,
    pub(super) term_postings: Vec<PreparedTermPostingV1>,
    pub(super) exact_postings: Vec<(String, Vec<u8>)>,
    /// Tagged hex form persisted by layouts up to 13.
    pub(super) integrity_digest: ManifestDigest,
    /// The same digest as the 32 raw bytes revision 14 persists.
    pub(super) integrity_digest_bytes: [u8; 32],
}

#[derive(Debug)]
pub(super) struct PreparedNgramShardV1 {
    pub(super) kind: i64,
    pub(super) ngram: i64,
    pub(super) documents: Vec<u8>,
    pub(super) cardinality: u64,
}

#[derive(Debug)]
pub(super) struct PreparedTermPostingV1 {
    pub(super) field: String,
    pub(super) term: String,
    pub(super) frequency: i64,
}

pub(super) fn prepare_page(
    layout: LexicalArtifactLayoutV1,
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
    previous_cursor: Option<Vec<u8>>,
    preparation_scratch_bytes: usize,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<PreparedCodeLexicalArtifactPageV1, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let first_document = page
        .next_cursor()
        .emitted_chunks()
        .checked_sub(page.chunk_count())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "sealed lexical page cursor regressed its chunk count".to_owned(),
            )
        })?;
    let mut documents = Vec::with_capacity(page.chunks().len());
    let mut row_dictionary = RowDictionaryTableV1::new();
    let mut ngram_documents = BTreeMap::<(i64, i64), RoaringBitmap>::new();
    let mut logical_ngram_postings = 0usize;
    if page.symbol_displays().len() != page.chunks().len() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "sealed lexical symbol-display cardinality does not match its chunks".to_owned(),
        ));
    }
    for (offset, (admitted, display)) in
        page.chunks().iter().zip(page.symbol_displays()).enumerate()
    {
        checkpoint(control)?;
        let document = first_document
            .checked_add(u64::try_from(offset).map_err(contract_number)?)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact prepared document id overflowed".to_owned(),
                )
            })?;
        let (prepared, ngrams) = prepare_document(
            layout,
            metadata,
            i64::try_from(document).map_err(contract_number)?,
            admitted.chunk(),
            display.as_ref(),
            &mut row_dictionary,
            control,
        )?;
        let document = u32::try_from(prepared.document_id).map_err(contract_number)?;
        logical_ngram_postings = logical_ngram_postings
            .checked_add(ngrams.len())
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact ngram aggregation count overflowed".to_owned(),
                )
            })?;
        for (kind, ngram) in ngrams {
            // Pages enumerate documents by their contiguous source ordinal,
            // and `document_ngrams` deduplicates each document first. Preserve
            // that ordering at the bitmap boundary so Roaring can append
            // instead of binary-searching every posting.
            ngram_documents
                .entry((kind, ngram))
                .or_default()
                .try_push(document)
                .map_err(|_| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact ngram documents are not strictly ordered".to_owned(),
                    )
                })?;
        }
        documents.push(prepared);
    }
    let mut imports = Vec::with_capacity(page.imports().len());
    for evidence in page.imports() {
        checkpoint(control)?;
        let canonical = serde_json::to_vec(evidence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let integrity_digest = import_integrity_digest(&canonical, &canonical)?;
        imports.push(PreparedImportV1 {
            canonical,
            integrity_digest,
        });
    }
    let mut clone_bodies = Vec::with_capacity(page.clone_bodies().len());
    for body in page.clone_bodies() {
        checkpoint(control)?;
        clone_bodies.push(prepare_clone_body(layout, body)?);
    }
    let next_cursor = page
        .next_cursor()
        .persisted_bytes()
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut ngram_shards = Vec::with_capacity(ngram_documents.len());
    for ((kind, ngram), documents) in ngram_documents {
        checkpoint(control)?;
        let encoded = encode_ngram_bitmap(layout, &documents)?;
        ngram_shards.push(PreparedNgramShardV1 {
            kind,
            ngram,
            documents: encoded,
            cardinality: documents.len(),
        });
    }
    let ngram_digest = ngram_page_digest(
        page.page_ordinal(),
        ngram_shards.iter().map(|shard| {
            (
                shard.kind,
                shard.ngram,
                shard.documents.as_slice(),
                shard.cardinality,
            )
        }),
    )?;
    let base_sections_receipt = prepare_base_sections_receipt(
        layout,
        page.page_ordinal(),
        &imports,
        &documents,
        &ngram_shards,
        control,
    )?;
    let aggregation_scratch_bytes = logical_ngram_postings
        .checked_mul(NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact ngram aggregation charge overflowed".to_owned(),
            )
        })?;
    let preparation_scratch_bytes = preparation_scratch_bytes
        .checked_add(aggregation_scratch_bytes)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page preparation scratch charge overflowed".to_owned(),
            )
        })?;
    let mut prepared = PreparedCodeLexicalArtifactPageV1 {
        page_ordinal: page.page_ordinal(),
        page_digest: page.page_digest().clone(),
        cumulative_digest: page.cumulative_digest().clone(),
        chunk_count: page.chunk_count(),
        payload_bytes: page.payload_bytes(),
        import_count: page.import_count(),
        import_payload_bytes: page.import_payload_bytes(),
        import_dictionary_digest: page.next_cursor().import_dictionary_digest().clone(),
        previous_cursor,
        next_cursor,
        imports,
        clone_bodies,
        documents,
        ngram_shards,
        ngram_digest,
        base_sections_receipt,
        row_dictionary,
        source_retained_bytes: page.retained_owned_bytes(),
        prepared_retained_bytes: 0,
        preparation_scratch_bytes,
        estimated_write_rows: 0,
        estimated_write_bytes: 0,
    };
    prepared.prepared_retained_bytes = prepared_retained_bytes(&prepared)?;
    (
        prepared.estimated_write_rows,
        prepared.estimated_write_bytes,
    ) = estimated_sqlite_writes(&prepared)?;
    Ok(prepared)
}

fn prepare_clone_body(
    layout: LexicalArtifactLayoutV1,
    body: &CodeIndexCloneBodyV1,
) -> Result<PreparedCloneBodyV1, CodeLexicalArtifactErrorV1> {
    let fingerprint_stream = if layout.has_clone_fingerprints() {
        body.payload
            .fingerprint_stream(body.occurrence.eligibility)
            .map(|stream| {
                Ok(PreparedCloneFingerprintStreamV1 {
                    language: body.payload.language.clone(),
                    class: stream.class,
                    normalization_revision: stream.normalization_revision,
                    body_digest: body.payload.body_digest.as_str().to_owned(),
                    positions: body
                        .payload
                        .fingerprint_positions(body.occurrence.eligibility)
                        .map_err(CodeLexicalArtifactErrorV1::Contract)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(PreparedCloneBodyV1 {
        payload_digest: body.payload.payload_digest.as_str().to_owned(),
        payload: serde_json::to_vec(&body.payload)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
        occurrence: serde_json::to_vec(&body.occurrence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
        symbol_occurrence_id: body.occurrence.symbol_occurrence_id.as_str().to_owned(),
        path: body.occurrence.path.clone(),
        body_start: body.occurrence.body_span.start_byte,
        body_end: body.occurrence.body_span.end_byte,
        exact_keys: body.payload.exact_keys(body.occurrence.eligibility),
        fingerprint_stream,
    })
}

fn prepare_base_sections_receipt(
    layout: LexicalArtifactLayoutV1,
    page_ordinal: u64,
    imports: &[PreparedImportV1],
    documents: &[PreparedDocumentV1],
    ngram_shards: &[PreparedNgramShardV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut document_integrity =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[0])?;
    let mut import_integrity =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[1])?;
    let mut import_evidence =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[2])?;
    let mut rows = PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[3])?;
    let mut term_postings =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[4])?;
    let mut exact_postings =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[5])?;
    let mut ngram_postings =
        PageBaseSectionReceiptBuilderV1::new(page_ordinal, BASE_SECTION_NAMES[6])?;

    for import in imports {
        checkpoint(control)?;
        import_integrity.begin_row()?;
        import_integrity.blob(&import.canonical)?;
        import_integrity.text(import.integrity_digest.as_str())?;
        import_evidence.begin_row()?;
        import_evidence.blob(&import.canonical)?;
        import_evidence.blob(&import.canonical)?;
    }
    for document in documents {
        checkpoint(control)?;
        document_integrity.begin_row()?;
        document_integrity.integer(document.document_id);
        if layout.stores_document_integrity_bytes() {
            document_integrity.blob(&document.integrity_digest_bytes)?;
        } else {
            document_integrity.text(&document.chunk_id)?;
            document_integrity.text(document.integrity_digest.as_str())?;
        }

        rows.begin_row()?;
        rows.integer(document.document_id);
        rows.text(&document.chunk_id)?;
        rows.blob(&document.row)?;

        for posting in &document.term_postings {
            checkpoint(control)?;
            term_postings.begin_row()?;
            term_postings.text(&posting.field)?;
            term_postings.text(&posting.term)?;
            term_postings.integer(document.document_id);
            term_postings.integer(posting.frequency);
        }
        for (field, term) in &document.exact_postings {
            checkpoint(control)?;
            exact_postings.begin_row()?;
            exact_postings.text(field)?;
            exact_postings.blob(term)?;
            exact_postings.integer(document.document_id);
        }
    }
    for shard in ngram_shards {
        checkpoint(control)?;
        ngram_postings.begin_row()?;
        ngram_postings.integer(i64::try_from(page_ordinal).map_err(contract_number)?);
        ngram_postings.integer(shard.kind);
        ngram_postings.integer(shard.ngram);
        ngram_postings.blob(&shard.documents)?;
        ngram_postings.integer(i64::try_from(shard.cardinality).map_err(contract_number)?);
    }

    encode_page_base_sections_receipt(
        page_ordinal,
        vec![
            document_integrity.finish()?,
            import_integrity.finish()?,
            import_evidence.finish()?,
            rows.finish()?,
            term_postings.finish()?,
            exact_postings.finish()?,
            ngram_postings.finish()?,
        ],
    )
}

fn prepare_document(
    layout: LexicalArtifactLayoutV1,
    metadata: &CodeLexicalProjectionMetadataV1,
    document_id: i64,
    chunk: &tracedecay_domain::CodeSearchChunkV1,
    display: Option<&tracedecay_code_index::production::VerifiedSealedLexicalSymbolDisplayV1>,
    row_dictionary: &mut RowDictionaryTableV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(PreparedDocumentV1, Vec<(i64, i64)>), CodeLexicalArtifactErrorV1> {
    u32::try_from(document_id).map_err(|_| {
        CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact exceeds the posting document-id range".to_owned(),
        )
    })?;
    if chunk.anchor.generation_id != metadata.generation {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "sealed lexical page contains a foreign generation".to_owned(),
        ));
    }
    let logical_path = metadata
        .logical_paths
        .get(&chunk.anchor.file_occurrence_id)
        .cloned()
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact metadata is missing path {}",
                chunk.anchor.file_occurrence_id
            ))
        })?;
    let (row, fields) = ProjectedChunkV1::from_ref(chunk, logical_path, display);
    let mut term_postings = Vec::new();
    for (field, terms) in &fields {
        checkpoint(control)?;
        let encoded_field = encode_field(*field)?;
        let mut frequencies = BTreeMap::<&str, u32>::new();
        for term in terms {
            frequencies
                .entry(term)
                .and_modify(|frequency| *frequency = frequency.saturating_add(1))
                .or_insert(1);
        }
        for (term, frequency) in frequencies {
            term_postings.push(PreparedTermPostingV1 {
                field: encoded_field.clone(),
                term: term.to_owned(),
                frequency: i64::from(frequency),
            });
        }
    }
    term_postings.sort_unstable_by(|left, right| {
        (&left.field, &left.term).cmp(&(&right.field, &right.term))
    });

    let mut exact_postings = BTreeSet::new();
    exact_postings.insert((
        encode_exact_field(ExactFieldV1::Path)?,
        row.logical_path.as_bytes().to_vec(),
    ));
    let mut encoded_fields = BTreeMap::new();
    for term in &row.exact_terms {
        let field = exact_field_for_kind(term.kind());
        let encoded = match encoded_fields.entry(field) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(encode_exact_field(field)?)
            }
            std::collections::btree_map::Entry::Occupied(slot) => slot.into_mut(),
        };
        exact_postings.insert((
            encoded.clone(),
            canonical_projected_exact_term(term).into_owned(),
        ));
    }

    let search_text = normalized_search_text(&row);
    let mut ngram_postings = document_ngrams(search_text.as_bytes(), control)?
        .into_iter()
        .map(|ngram| (NGRAM_NORMALIZED, i64::from(ngram)))
        .collect::<Vec<_>>();
    if row.sanitized_text.as_str().as_bytes() != row.normalized_text.as_bytes() {
        ngram_postings.extend(
            document_ngrams(row.sanitized_text.as_str().as_bytes(), control)?
                .into_iter()
                .map(|ngram| (NGRAM_RAW_OVERRIDE, i64::from(ngram))),
        );
    }
    let artifact_row = ArtifactRowV1::from(row);
    let chunk_id = artifact_row.id.as_str().to_owned();
    let row = encode_artifact_row(layout, &artifact_row, row_dictionary)?;
    let exact_postings = exact_postings.into_iter().collect::<Vec<_>>();
    let (integrity_digest, integrity_digest_bytes) = document_integrity_digest(
        document_id,
        chunk_id.as_bytes(),
        &row,
        &term_postings,
        &exact_postings,
    )?;
    Ok((
        PreparedDocumentV1 {
            document_id,
            chunk_id,
            row,
            term_postings,
            exact_postings,
            integrity_digest,
            integrity_digest_bytes,
        },
        ngram_postings,
    ))
}

fn document_integrity_digest(
    document: i64,
    chunk_id: &[u8],
    row: &[u8],
    term_postings: &[PreparedTermPostingV1],
    exact_postings: &[(String, Vec<u8>)],
) -> Result<(ManifestDigest, [u8; 32]), CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-derived-document.v3\0");
    hasher.update(document.to_le_bytes());
    hash_table(&mut hasher, "row", 1, |hasher, _| {
        hash_text(hasher, chunk_id)?;
        hash_blob(hasher, row)
    })?;
    hash_table(
        &mut hasher,
        "term_posting",
        term_postings.len(),
        |hasher, ordinal| {
            let posting = &term_postings[ordinal];
            hash_text(hasher, posting.field.as_bytes())?;
            hash_text(hasher, posting.term.as_bytes())?;
            hash_integer(hasher, posting.frequency);
            Ok(())
        },
    )?;
    hash_table(
        &mut hasher,
        "exact_posting",
        exact_postings.len(),
        |hasher, ordinal| {
            let (field, term) = &exact_postings[ordinal];
            hash_text(hasher, field.as_bytes())?;
            hash_blob(hasher, term)
        },
    )?;
    let bytes: [u8; 32] = hasher.finalize().into();
    let digest = ManifestDigest::from_sha256_bytes(&bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    Ok((digest, bytes))
}

fn hash_table(
    hasher: &mut Sha256,
    table: &str,
    row_count: usize,
    mut hash_row: impl FnMut(&mut Sha256, usize) -> Result<(), CodeLexicalArtifactErrorV1>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(table.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(table.as_bytes());
    for ordinal in 0..row_count {
        hasher.update(b"row\0");
        hash_row(hasher, ordinal)?;
    }
    hasher.update(b"end\0");
    hasher.update(
        u64::try_from(row_count)
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    Ok(())
}

fn hash_integer(hasher: &mut Sha256, value: i64) {
    hasher.update([1]);
    hasher.update(value.to_le_bytes());
}

fn hash_text(hasher: &mut Sha256, value: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update([3]);
    hash_bytes(hasher, value)
}

fn hash_blob(hasher: &mut Sha256, value: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update([4]);
    hash_bytes(hasher, value)
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

fn import_integrity_digest(
    canonical: &[u8],
    evidence: &[u8],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-derived-import.v1\0");
    hash_bytes(&mut hasher, canonical)?;
    hash_bytes(&mut hasher, evidence)?;
    integrity_digest(hasher)
}

fn integrity_digest(hasher: Sha256) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn prepared_retained_bytes(
    page: &PreparedCodeLexicalArtifactPageV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let clone_body_bytes = prepared_clone_body_retained_bytes(&page.clone_bodies)?;
    let mut bytes = page
        .page_digest
        .as_str()
        .len()
        .checked_add(page.cumulative_digest.as_str().len())
        .and_then(|bytes| bytes.checked_add(page.import_dictionary_digest.as_str().len()))
        .and_then(|bytes| bytes.checked_add(page.next_cursor.capacity()))
        .and_then(|bytes| bytes.checked_add(page.previous_cursor.as_ref().map_or(0, Vec::capacity)))
        .and_then(|bytes| {
            bytes.checked_add(
                page.imports
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PreparedImportV1>()),
            )
        })
        .and_then(|bytes| bytes.checked_add(clone_body_bytes))
        .and_then(|bytes| {
            bytes.checked_add(
                page.documents
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PreparedDocumentV1>()),
            )
        })
        .and_then(|bytes| {
            bytes.checked_add(
                page.ngram_shards
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PreparedNgramShardV1>()),
            )
        })
        .and_then(|bytes| bytes.checked_add(page.ngram_digest.as_str().len()))
        .and_then(|bytes| bytes.checked_add(page.base_sections_receipt.capacity()))
        .ok_or_else(prepared_charge_overflow)?;
    for import in &page.imports {
        bytes = bytes
            .checked_add(import.canonical.capacity())
            .and_then(|bytes| bytes.checked_add(import.integrity_digest.as_str().len()))
            .ok_or_else(prepared_charge_overflow)?;
    }
    for document in &page.documents {
        bytes = bytes
            .checked_add(document.chunk_id.capacity())
            .and_then(|bytes| bytes.checked_add(document.row.capacity()))
            .and_then(|bytes| bytes.checked_add(document.integrity_digest.as_str().len()))
            .and_then(|bytes| bytes.checked_add(document.integrity_digest_bytes.len()))
            .and_then(|bytes| {
                bytes.checked_add(
                    document
                        .term_postings
                        .capacity()
                        .saturating_mul(std::mem::size_of::<PreparedTermPostingV1>()),
                )
            })
            .and_then(|bytes| {
                bytes.checked_add(
                    document
                        .exact_postings
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(String, Vec<u8>)>()),
                )
            })
            .ok_or_else(prepared_charge_overflow)?;
        for posting in &document.term_postings {
            bytes = bytes
                .checked_add(posting.field.capacity())
                .and_then(|bytes| bytes.checked_add(posting.term.capacity()))
                .ok_or_else(prepared_charge_overflow)?;
        }
        for (field, term) in &document.exact_postings {
            bytes = bytes
                .checked_add(field.capacity())
                .and_then(|bytes| bytes.checked_add(term.capacity()))
                .ok_or_else(prepared_charge_overflow)?;
        }
    }
    for shard in &page.ngram_shards {
        bytes = bytes
            .checked_add(shard.documents.capacity())
            .ok_or_else(prepared_charge_overflow)?;
    }
    for entry in page.row_dictionary.values() {
        bytes = bytes
            .checked_add(entry.capacity())
            .and_then(|bytes| {
                bytes.checked_add(
                    std::mem::size_of::<(i64, Vec<u8>)>() + BTREE_MAP_ENTRY_OVERHEAD_BYTES,
                )
            })
            .ok_or_else(prepared_charge_overflow)?;
    }
    Ok(bytes)
}

fn prepared_clone_body_retained_bytes(
    bodies: &[PreparedCloneBodyV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    bodies
        .iter()
        .try_fold(std::mem::size_of_val(bodies), |bytes, body| {
            bytes
                .checked_add(body.payload_digest.capacity())
                .and_then(|bytes| bytes.checked_add(body.payload.capacity()))
                .and_then(|bytes| bytes.checked_add(body.occurrence.capacity()))
                .and_then(|bytes| bytes.checked_add(body.symbol_occurrence_id.capacity()))
                .and_then(|bytes| bytes.checked_add(body.path.capacity()))
                .and_then(|bytes| {
                    bytes.checked_add(
                        body.exact_keys
                            .capacity()
                            .saturating_mul(std::mem::size_of::<CloneExactKeyV1>()),
                    )
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        body.exact_keys
                            .iter()
                            .map(|key| key.digest.as_str().len())
                            .sum::<usize>(),
                    )
                })
                .and_then(|bytes| match &body.fingerprint_stream {
                    Some(stream) => {
                        bytes
                            .checked_add(stream.language.capacity())
                            .and_then(|bytes| bytes.checked_add(stream.body_digest.capacity()))
                            .and_then(|bytes| {
                                bytes.checked_add(stream.positions.capacity().saturating_mul(
                                    std::mem::size_of::<CloneFingerprintPositionV1>(),
                                ))
                            })
                    }
                    None => Some(bytes),
                })
                .ok_or_else(prepared_charge_overflow)
        })
}

fn estimated_sqlite_writes(
    page: &PreparedCodeLexicalArtifactPageV1,
) -> Result<(usize, usize), CodeLexicalArtifactErrorV1> {
    let mut rows = 1usize;
    let mut bytes = estimated_source_page_receipt_write_bytes(
        page.page_digest.as_str(),
        page.cumulative_digest.as_str(),
        page.import_dictionary_digest.as_str(),
        page.ngram_digest.as_str(),
        &page.base_sections_receipt,
        &page.next_cursor,
    )?;
    for import in &page.imports {
        rows = rows.checked_add(2).ok_or_else(prepared_write_overflow)?;
        bytes = bytes
            .checked_add(import.canonical.len().saturating_mul(2))
            .and_then(|bytes| bytes.checked_add(import.integrity_digest.as_str().len()))
            .ok_or_else(prepared_write_overflow)?;
    }
    let (clone_rows, clone_bytes) = estimated_clone_body_writes(&page.clone_bodies)?;
    rows = rows
        .checked_add(clone_rows)
        .ok_or_else(prepared_write_overflow)?;
    bytes = bytes
        .checked_add(clone_bytes)
        .ok_or_else(prepared_write_overflow)?;
    for document in &page.documents {
        rows = rows.checked_add(2).ok_or_else(prepared_write_overflow)?;
        bytes = bytes
            .checked_add(document.chunk_id.len())
            .and_then(|bytes| bytes.checked_add(document.row.len()))
            .and_then(|bytes| bytes.checked_add(document.integrity_digest.as_str().len()))
            .ok_or_else(prepared_write_overflow)?;
        for posting in &document.term_postings {
            rows = rows.checked_add(1).ok_or_else(prepared_write_overflow)?;
            bytes = bytes
                .checked_add(posting.field.len().saturating_add(posting.term.len()))
                .and_then(|bytes| bytes.checked_add(32))
                .ok_or_else(prepared_write_overflow)?;
        }
        for (field, term) in &document.exact_postings {
            rows = rows.checked_add(1).ok_or_else(prepared_write_overflow)?;
            bytes = bytes
                .checked_add(field.len())
                .and_then(|bytes| bytes.checked_add(term.len()))
                .and_then(|bytes| bytes.checked_add(8))
                .ok_or_else(prepared_write_overflow)?;
        }
    }
    rows = rows
        .checked_add(page.ngram_shards.len())
        .ok_or_else(prepared_write_overflow)?;
    for shard in &page.ngram_shards {
        bytes = bytes
            .checked_add(shard.documents.len())
            .and_then(|bytes| bytes.checked_add(32))
            .ok_or_else(prepared_write_overflow)?;
    }
    rows = rows
        .checked_add(page.row_dictionary.len())
        .ok_or_else(prepared_write_overflow)?;
    for entry in page.row_dictionary.values() {
        bytes = bytes
            .checked_add(entry.len())
            .and_then(|bytes| bytes.checked_add(16))
            .ok_or_else(prepared_write_overflow)?;
    }
    Ok((rows, bytes))
}

fn estimated_clone_body_writes(
    bodies: &[PreparedCloneBodyV1],
) -> Result<(usize, usize), CodeLexicalArtifactErrorV1> {
    bodies
        .iter()
        .try_fold((0usize, 0usize), |(rows, bytes), body| {
            let rows = rows
                .checked_add(
                    2usize.saturating_add(body.exact_keys.len()).saturating_add(
                        body.fingerprint_stream
                            .as_ref()
                            .map_or(0, |stream| stream.positions.len()),
                    ),
                )
                .ok_or_else(prepared_write_overflow)?;
            let bytes = bytes
                .checked_add(body.payload_digest.len().saturating_mul(2))
                .and_then(|bytes| bytes.checked_add(body.payload.len()))
                .and_then(|bytes| bytes.checked_add(body.occurrence.len()))
                .and_then(|bytes| bytes.checked_add(body.symbol_occurrence_id.len()))
                .and_then(|bytes| bytes.checked_add(body.path.len()))
                .and_then(|bytes| {
                    bytes.checked_add(body.exact_keys.iter().fold(0usize, |total, key| {
                        total
                            .saturating_add(key.digest.as_str().len())
                            .saturating_add(16)
                    }))
                })
                .and_then(|bytes| match &body.fingerprint_stream {
                    Some(stream) => bytes.checked_add(
                        stream.positions.len().saturating_mul(
                            stream
                                .language
                                .len()
                                .saturating_add(stream.body_digest.len())
                                .saturating_add(body.symbol_occurrence_id.len())
                                .saturating_add(body.payload_digest.len())
                                .saturating_add(32),
                        ),
                    ),
                    None => Some(bytes),
                })
                .ok_or_else(prepared_write_overflow)?;
            Ok((rows, bytes))
        })
}

fn estimated_source_page_receipt_write_bytes(
    page_digest: &str,
    cumulative_digest: &str,
    import_dictionary_digest: &str,
    ngram_digest: &str,
    base_sections_receipt: &[u8],
    next_cursor: &[u8],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    page_digest
        .len()
        .checked_add(cumulative_digest.len())
        .and_then(|bytes| bytes.checked_add(import_dictionary_digest.len()))
        .and_then(|bytes| bytes.checked_add(ngram_digest.len()))
        .and_then(|bytes| bytes.checked_add(base_sections_receipt.len()))
        .and_then(|bytes| bytes.checked_add(next_cursor.len()))
        .ok_or_else(prepared_write_overflow)
}

fn prepared_write_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(
        "prepared lexical page estimated SQLite write overflowed".to_owned(),
    )
}

fn prepared_charge_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(
        "prepared lexical page retained-byte charge overflowed".to_owned(),
    )
}

fn contract_number(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(error.to_string())
}
