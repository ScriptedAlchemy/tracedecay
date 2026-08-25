use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use sha2::{Digest, Sha256};
use tracedecay_domain::ExactTechnicalTermV1;

use super::sealed_codec::{
    LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION, PersistedFileGenerationArtifactsV1,
};
use super::*;

const PAGE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-page.v1\0";
const SOURCE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source.v1\0";
const IMPORT_DICTIONARY_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-import-dictionary.v1\0";
const IMPORT_RECORD_DOMAIN: &[u8] = b"import\0";
const SOURCE_CHAIN_RECORD_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source-chain.v1\0";
const IMPORT_DICTIONARY_CHAIN_RECORD_DOMAIN: &[u8] =
    b"tracedecay.sealed-lexical-import-dictionary-chain.v1\0";
const CURSOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-cursor.v1\0";

type PersistedSealedLexicalCursorFields = (
    String,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    String,
    String,
    String,
);

/// Resume position after one fully admitted lexical page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSealedLexicalCursorV1 {
    source_state_digest: ManifestDigest,
    next_file_ordinal: u64,
    next_file_offset: u64,
    next_chunk_ordinal: u64,
    next_import_ordinal: u64,
    next_page_ordinal: u64,
    emitted_chunks: u64,
    emitted_payload_bytes: u64,
    emitted_imports: u64,
    emitted_import_payload_bytes: u64,
    import_dictionary_digest: ManifestDigest,
    cumulative_digest: ManifestDigest,
}

impl VerifiedSealedLexicalCursorV1 {
    fn initial(
        source_state_digest: ManifestDigest,
        next_file_offset: u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        Ok(Self {
            source_state_digest,
            next_file_ordinal: 0,
            next_file_offset,
            next_chunk_ordinal: 0,
            next_import_ordinal: 0,
            next_page_ordinal: 0,
            emitted_chunks: 0,
            emitted_payload_bytes: 0,
            emitted_imports: 0,
            emitted_import_payload_bytes: 0,
            import_dictionary_digest: initial_digest(IMPORT_DICTIONARY_DIGEST_DOMAIN)?,
            cumulative_digest: initial_digest(SOURCE_DIGEST_DOMAIN)?,
        })
    }

    pub fn persisted_bytes(&self) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        serde_json::to_vec(&(
            self.source_state_digest.as_str(),
            self.next_file_ordinal,
            self.next_file_offset,
            self.next_chunk_ordinal,
            self.next_page_ordinal,
            self.emitted_chunks,
            self.emitted_payload_bytes,
            self.next_import_ordinal,
            self.emitted_imports,
            self.emitted_import_payload_bytes,
            self.import_dictionary_digest.as_str(),
            self.cumulative_digest.as_str(),
            self.integrity_digest()?.as_str(),
        ))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }

    pub fn restore_persisted(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        let (
            source_state_digest,
            next_file_ordinal,
            next_file_offset,
            next_chunk_ordinal,
            next_page_ordinal,
            emitted_chunks,
            emitted_payload_bytes,
            next_import_ordinal,
            emitted_imports,
            emitted_import_payload_bytes,
            import_dictionary_digest,
            cumulative_digest,
            integrity_digest,
        ): PersistedSealedLexicalCursorFields = serde_json::from_slice(bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "persisted sealed lexical cursor is invalid: {error}"
            ))
        })?;
        let cursor = Self {
            source_state_digest: ManifestDigest::new(source_state_digest)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
            next_file_ordinal,
            next_file_offset,
            next_chunk_ordinal,
            next_import_ordinal,
            next_page_ordinal,
            emitted_chunks,
            emitted_payload_bytes,
            emitted_imports,
            emitted_import_payload_bytes,
            import_dictionary_digest: ManifestDigest::new(import_dictionary_digest)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
            cumulative_digest: ManifestDigest::new(cumulative_digest)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
        };
        let integrity_digest = ManifestDigest::new(integrity_digest)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if cursor.integrity_digest()? != integrity_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "persisted sealed lexical cursor integrity digest does not verify".to_owned(),
            ));
        }
        Ok(cursor)
    }

    fn is_initial(&self) -> Result<bool, CodeIndexProductionErrorV1> {
        Ok(self.next_file_ordinal == 0
            && self.next_chunk_ordinal == 0
            && self.next_import_ordinal == 0
            && self.next_page_ordinal == 0
            && self.emitted_chunks == 0
            && self.emitted_payload_bytes == 0
            && self.emitted_imports == 0
            && self.emitted_import_payload_bytes == 0
            && self.import_dictionary_digest == initial_digest(IMPORT_DICTIONARY_DIGEST_DOMAIN)?
            && self.cumulative_digest == initial_digest(SOURCE_DIGEST_DOMAIN)?)
    }

    fn integrity_digest(&self) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
        let mut hasher = Sha256::new();
        hasher.update(CURSOR_DIGEST_DOMAIN);
        hash_record(&mut hasher, self.source_state_digest.as_str().as_bytes())?;
        hasher.update(self.next_file_ordinal.to_le_bytes());
        hasher.update(self.next_file_offset.to_le_bytes());
        hasher.update(self.next_chunk_ordinal.to_le_bytes());
        hasher.update(self.next_import_ordinal.to_le_bytes());
        hasher.update(self.next_page_ordinal.to_le_bytes());
        hasher.update(self.emitted_chunks.to_le_bytes());
        hasher.update(self.emitted_payload_bytes.to_le_bytes());
        hasher.update(self.emitted_imports.to_le_bytes());
        hasher.update(self.emitted_import_payload_bytes.to_le_bytes());
        hash_record(
            &mut hasher,
            self.import_dictionary_digest.as_str().as_bytes(),
        )?;
        hash_record(&mut hasher, self.cumulative_digest.as_str().as_bytes())?;
        digest_hasher(hasher)
    }

    fn verify_source(
        &self,
        source_state_digest: &ManifestDigest,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if &self.source_state_digest != source_state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical cursor does not belong to this source state".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn next_file_ordinal(&self) -> u64 {
        self.next_file_ordinal
    }

    pub fn next_chunk_ordinal(&self) -> u64 {
        self.next_chunk_ordinal
    }

    pub fn next_import_ordinal(&self) -> u64 {
        self.next_import_ordinal
    }

    pub fn next_page_ordinal(&self) -> u64 {
        self.next_page_ordinal
    }

    pub fn emitted_chunks(&self) -> u64 {
        self.emitted_chunks
    }

    pub fn emitted_payload_bytes(&self) -> u64 {
        self.emitted_payload_bytes
    }

    pub fn emitted_imports(&self) -> u64 {
        self.emitted_imports
    }

    pub fn emitted_import_payload_bytes(&self) -> u64 {
        self.emitted_import_payload_bytes
    }

    pub fn import_dictionary_digest(&self) -> &ManifestDigest {
        &self.import_dictionary_digest
    }

    pub fn cumulative_digest(&self) -> &ManifestDigest {
        &self.cumulative_digest
    }
}

/// One bounded page of parser-backed, sanitized search chunks.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageV1 {
    page_ordinal: u64,
    chunk_count: u64,
    payload_bytes: u64,
    import_count: u64,
    import_payload_bytes: u64,
    page_digest: ManifestDigest,
    cumulative_digest: ManifestDigest,
    next_cursor: VerifiedSealedLexicalCursorV1,
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    previous_cursor: VerifiedSealedLexicalCursorV1,
}

impl VerifiedSealedLexicalPageV1 {
    pub fn page_ordinal(&self) -> u64 {
        self.page_ordinal
    }

    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    pub fn import_count(&self) -> u64 {
        self.import_count
    }

    pub fn import_payload_bytes(&self) -> u64 {
        self.import_payload_bytes
    }

    pub fn page_digest(&self) -> &ManifestDigest {
        &self.page_digest
    }

    pub fn cumulative_digest(&self) -> &ManifestDigest {
        &self.cumulative_digest
    }

    pub fn next_cursor(&self) -> &VerifiedSealedLexicalCursorV1 {
        &self.next_cursor
    }

    pub fn chunks(&self) -> &[ExtractionAdmittedCodeSearchChunkV1] {
        &self.chunks
    }

    pub fn chunk_capacity(&self) -> usize {
        self.chunks.capacity()
    }

    pub fn imports(&self) -> &[CodeIndexImportEvidenceV1] {
        &self.imports
    }

    pub fn import_capacity(&self) -> usize {
        self.imports.capacity()
    }

    /// Recompute the exact page payload, import, and cumulative transition.
    ///
    /// The cursor digest chains are persisted authorities, so a reopened
    /// source can continue them without replaying accepted pages. Binding the
    /// transition to `previous` preserves page-boundary-independent source and
    /// import dictionary identities while every builder admission remains
    /// independently fail closed.
    pub fn verify_transition(
        &self,
        previous: Option<&VerifiedSealedLexicalCursorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        match previous {
            Some(previous) if previous != &self.previous_cursor => {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical page does not continue the persisted cursor".to_owned(),
                ));
            }
            None if !self.previous_cursor.is_initial()? => {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "first sealed lexical page does not start at the canonical cursor".to_owned(),
                ));
            }
            Some(_) | None => {}
        }
        let mut page_hasher = page_hasher(self.page_ordinal);
        let mut cumulative_digest = self.previous_cursor.cumulative_digest.clone();
        let mut import_dictionary_digest = self.previous_cursor.import_dictionary_digest.clone();
        let mut payload_bytes = 0u64;
        for admitted in &self.chunks {
            let serialized = serde_json::to_vec(admitted.chunk()).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical chunk serialization failed: {error}"
                ))
            })?;
            hash_record(&mut page_hasher, &serialized)?;
            cumulative_digest =
                advance_digest(&cumulative_digest, SOURCE_CHAIN_RECORD_DOMAIN, &serialized)?;
            payload_bytes = payload_bytes
                .checked_add(u64::try_from(serialized.len()).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical chunk payload exceeds u64".to_owned(),
                    )
                })?)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical chunk payload overflowed".to_owned(),
                    )
                })?;
        }
        let mut import_payload_bytes = 0u64;
        for evidence in &self.imports {
            let serialized = serde_json::to_vec(evidence).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical import serialization failed: {error}"
                ))
            })?;
            hash_import_record(&mut page_hasher, &serialized)?;
            cumulative_digest =
                advance_digest(&cumulative_digest, IMPORT_RECORD_DOMAIN, &serialized)?;
            import_dictionary_digest = advance_digest(
                &import_dictionary_digest,
                IMPORT_DICTIONARY_CHAIN_RECORD_DOMAIN,
                &serialized,
            )?;
            import_payload_bytes = import_payload_bytes
                .checked_add(u64::try_from(serialized.len()).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical import payload exceeds u64".to_owned(),
                    )
                })?)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical import payload overflowed".to_owned(),
                    )
                })?;
        }
        let chunk_count = u64::try_from(self.chunks.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page chunk count exceeds u64".to_owned(),
            )
        })?;
        let import_count = u64::try_from(self.imports.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page import count exceeds u64".to_owned(),
            )
        })?;
        let expected_cumulative = cumulative_digest;
        let expected_import_dictionary = import_dictionary_digest;
        let expected_next_page = self
            .previous_cursor
            .next_page_ordinal
            .checked_add(1)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical page ordinal overflowed".to_owned(),
                )
            })?;
        if self.page_ordinal != self.previous_cursor.next_page_ordinal
            || self.chunk_count != chunk_count
            || self.payload_bytes != payload_bytes
            || self.import_count != import_count
            || self.import_payload_bytes != import_payload_bytes
            || self.cumulative_digest != expected_cumulative
            || self.next_cursor.next_page_ordinal != expected_next_page
            || self.next_cursor.emitted_chunks
                != self
                    .previous_cursor
                    .emitted_chunks
                    .checked_add(chunk_count)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source chunk count overflowed".to_owned(),
                        )
                    })?
            || self.next_cursor.emitted_payload_bytes
                != self
                    .previous_cursor
                    .emitted_payload_bytes
                    .checked_add(payload_bytes)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source payload overflowed".to_owned(),
                        )
                    })?
            || self.next_cursor.emitted_imports
                != self
                    .previous_cursor
                    .emitted_imports
                    .checked_add(import_count)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source import count overflowed".to_owned(),
                        )
                    })?
            || self.next_cursor.emitted_import_payload_bytes
                != self
                    .previous_cursor
                    .emitted_import_payload_bytes
                    .checked_add(import_payload_bytes)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source import payload overflowed".to_owned(),
                        )
                    })?
            || self.next_cursor.source_state_digest != self.previous_cursor.source_state_digest
            || self.next_cursor.cumulative_digest != expected_cumulative
            || self.next_cursor.import_dictionary_digest != expected_import_dictionary
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page transition is internally inconsistent".to_owned(),
            ));
        }
        hash_cursor(&mut page_hasher, &self.next_cursor)?;
        if digest_hasher(page_hasher)? != self.page_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page digest does not verify".to_owned(),
            ));
        }
        Ok(())
    }

    /// Heap bytes retained by this page's chunk and import vectors plus its
    /// digest identities.
    ///
    /// Vector and `String` storage uses actual capacities. Immutable typed-ID,
    /// exact-term, and sanitized-text payloads expose lengths rather than
    /// capacities, so their byte-exact payload size is counted once alongside
    /// the owning vector slots. The page, cumulative, and both cursor digest
    /// strings are heap-owned and counted at length. Allocator metadata and
    /// fixed inline fields (including hash states) are deliberately excluded.
    pub fn retained_owned_bytes(&self) -> usize {
        let digest_bytes = self
            .page_digest
            .as_str()
            .len()
            .saturating_add(self.cumulative_digest.as_str().len())
            .saturating_add(self.next_cursor.source_state_digest.as_str().len())
            .saturating_add(self.next_cursor.import_dictionary_digest.as_str().len())
            .saturating_add(self.next_cursor.cumulative_digest.as_str().len())
            .saturating_add(self.previous_cursor.source_state_digest.as_str().len())
            .saturating_add(self.previous_cursor.import_dictionary_digest.as_str().len())
            .saturating_add(self.previous_cursor.cumulative_digest.as_str().len());
        let chunk_bytes = self.chunks.iter().fold(
            self.chunks
                .capacity()
                .saturating_mul(std::mem::size_of::<ExtractionAdmittedCodeSearchChunkV1>()),
            |bytes, admitted| {
                let chunk = admitted.chunk();
                let exact_term_bytes = chunk.exact_terms.iter().fold(
                    chunk
                        .exact_terms
                        .capacity()
                        .saturating_mul(std::mem::size_of::<ExactTechnicalTermV1>()),
                    |bytes, term| {
                        bytes
                            .saturating_add(term.original_bytes().len())
                            .saturating_add(term.canonical_bytes().len())
                            .saturating_add(
                                term.symbol_occurrence_id()
                                    .map_or(0, |occurrence| occurrence.as_str().len()),
                            )
                    },
                );
                let subtoken_bytes = chunk.subtokens.iter().fold(
                    chunk
                        .subtokens
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                    |bytes, subtoken| bytes.saturating_add(subtoken.capacity()),
                );
                bytes
                    .saturating_add(chunk.id.as_str().len())
                    .saturating_add(chunk.anchor.generation_id.as_str().len())
                    .saturating_add(chunk.anchor.file_occurrence_id.as_str().len())
                    .saturating_add(
                        chunk
                            .anchor
                            .symbol_occurrence_id
                            .as_ref()
                            .map_or(0, |occurrence| occurrence.as_str().len()),
                    )
                    .saturating_add(
                        chunk
                            .anchor
                            .parent_chunk_id
                            .as_ref()
                            .map_or(0, |parent| parent.as_str().len()),
                    )
                    .saturating_add(chunk.content_digest.as_str().len())
                    .saturating_add(chunk.language_descriptor_revision.as_str().len())
                    .saturating_add(chunk.chunker_revision.as_str().len())
                    .saturating_add(chunk.sanitizer_revision.as_str().len())
                    .saturating_add(chunk.sensitivity.policy_revision.as_str().len())
                    .saturating_add(exact_term_bytes)
                    .saturating_add(subtoken_bytes)
                    .saturating_add(chunk.sanitized_text.as_str().len())
            },
        );
        self.imports.iter().fold(
            chunk_bytes.saturating_add(digest_bytes).saturating_add(
                self.imports
                    .capacity()
                    .saturating_mul(std::mem::size_of::<CodeIndexImportEvidenceV1>()),
            ),
            |bytes, evidence| {
                bytes
                    .saturating_add(evidence.logical_path.capacity())
                    .saturating_add(evidence.file_occurrence_id.as_str().len())
                    .saturating_add(evidence.module_specifier.capacity())
                    .saturating_add(evidence.imported_name.as_ref().map_or(0, String::capacity))
                    .saturating_add(evidence.local_name.as_ref().map_or(0, String::capacity))
            },
        )
    }
}

/// Final proof that all file ranges in one verified seal were exhausted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSealedLexicalSourceReceiptV1 {
    source_state_digest: ManifestDigest,
    format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: ManifestDigest,
    cumulative_digest: ManifestDigest,
}

impl VerifiedSealedLexicalSourceReceiptV1 {
    pub fn source_state_digest(&self) -> &ManifestDigest {
        &self.source_state_digest
    }

    pub fn format_revision(&self) -> u32 {
        self.format_revision
    }

    pub fn page_count(&self) -> u64 {
        self.page_count
    }

    pub fn total_chunks(&self) -> u64 {
        self.total_chunks
    }

    pub fn total_payload_bytes(&self) -> u64 {
        self.total_payload_bytes
    }

    pub fn total_imports(&self) -> u64 {
        self.total_imports
    }

    pub fn import_payload_bytes(&self) -> u64 {
        self.import_payload_bytes
    }

    pub fn import_dictionary_digest(&self) -> &ManifestDigest {
        &self.import_dictionary_digest
    }

    pub fn cumulative_digest(&self) -> &ManifestDigest {
        &self.cumulative_digest
    }

    pub fn verify_completion(
        &self,
        cursor: Option<&VerifiedSealedLexicalCursorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let Some(cursor) = cursor else {
            if self.page_count == 0
                && self.total_chunks == 0
                && self.total_payload_bytes == 0
                && self.total_imports == 0
                && self.import_payload_bytes == 0
                && self.import_dictionary_digest == initial_digest(IMPORT_DICTIONARY_DIGEST_DOMAIN)?
                && self.cumulative_digest == initial_digest(SOURCE_DIGEST_DOMAIN)?
            {
                return Ok(());
            }
            return Err(CodeIndexProductionErrorV1::Contract(
                "nonempty sealed lexical receipt has no final cursor".to_owned(),
            ));
        };
        if self.page_count != cursor.next_page_ordinal
            || self.source_state_digest != cursor.source_state_digest
            || self.total_chunks != cursor.emitted_chunks
            || self.total_payload_bytes != cursor.emitted_payload_bytes
            || self.total_imports != cursor.emitted_imports
            || self.import_payload_bytes != cursor.emitted_import_payload_bytes
            || self.import_dictionary_digest != cursor.import_dictionary_digest
            || self.cumulative_digest != cursor.cumulative_digest
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical source receipt does not match its final cursor".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // typed page-read terminal states; pages dominate reads
pub enum VerifiedSealedLexicalPageReadV1 {
    Page(VerifiedSealedLexicalPageV1),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

/// Deterministic admission limits for one staged lexical-page batch.
///
/// Both limits are required so a caller cannot accidentally turn the verified
/// source into an unbounded retained-page queue. `maximum_retained_bytes`
/// charges the batch vector slots and
/// [`VerifiedSealedLexicalPageV1::retained_owned_bytes`] for every page
/// staged in the batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedSealedLexicalPageBatchBoundsV1 {
    maximum_pages: usize,
    maximum_retained_bytes: usize,
    page_slot_bytes: usize,
}

impl VerifiedSealedLexicalPageBatchBoundsV1 {
    pub fn new(
        maximum_pages: usize,
        maximum_retained_bytes: usize,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if maximum_pages == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page batch count bound must be non-zero".to_owned(),
            ));
        }
        if maximum_retained_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page batch retained-byte bound must be non-zero".to_owned(),
            ));
        }
        let page_slot_bytes = maximum_pages
            .checked_mul(std::mem::size_of::<VerifiedSealedLexicalPageV1>())
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical page batch slot bound overflowed".to_owned(),
                )
            })?;
        if page_slot_bytes > maximum_retained_bytes {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page batch retained-byte bound cannot hold its page slots"
                    .to_owned(),
            ));
        }
        Ok(Self {
            maximum_pages,
            maximum_retained_bytes,
            page_slot_bytes,
        })
    }

    pub fn maximum_pages(&self) -> usize {
        self.maximum_pages
    }

    pub fn maximum_retained_bytes(&self) -> usize {
        self.maximum_retained_bytes
    }

    fn page_slot_bytes(&self) -> usize {
        self.page_slot_bytes
    }
}

/// Result of one accepted lexical-page batch prefix.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // staged pages dominate this terminal read result
pub enum VerifiedSealedLexicalPageBatchReadV1 {
    Pages(Vec<VerifiedSealedLexicalPageV1>),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

struct PendingSealedLexicalPageV1 {
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    page_bytes: usize,
    imports: Vec<CodeIndexImportEvidenceV1>,
    import_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    page_hasher: Sha256,
}

struct StagedSealedLexicalPageV1 {
    page: VerifiedSealedLexicalPageV1,
    cursor: VerifiedSealedLexicalCursorV1,
}

#[allow(clippy::large_enum_variant)] // staged pages dominate this private read result
enum StagedSealedLexicalPageReadV1 {
    Page(StagedSealedLexicalPageV1),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

#[allow(clippy::large_enum_variant)] // staged pages dominate this private batch result
enum StagedSealedLexicalPageBatchReadV1 {
    Pages(Vec<VerifiedSealedLexicalPageV1>),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

/// Seekable, bounded lexical projection source over a verified v5/v6 seal.
///
/// Opening performs a streaming structural scan and verifies the exact raw
/// generation digest. It retains only a constant-size files-array boundary;
/// each read discovers and decodes the next file from the persisted byte
/// cursor, then emits at most the configured chunk and serialized-payload
/// bounds. Raw sealed bytes never cross this interface.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageSourceV1<R> {
    reader: R,
    file_count: u64,
    first_file_offset: u64,
    files_end_offset: u64,
    total_lexical_bytes: u64,
    maximum_file_bytes: u64,
    source_state_digest: ManifestDigest,
    format_revision: u32,
    maximum_page_chunks: usize,
    maximum_page_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    admitted_file: Option<(u64, AdmittedSealedLexicalFileV1)>,
}

impl<R: Read + Seek> VerifiedSealedLexicalPageSourceV1<R> {
    #[hotpath::measure]
    pub fn open(
        mut reader: R,
        admitted_len: u64,
        expected_state_digest: ManifestDigest,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if maximum_page_chunks == 0 || maximum_page_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page bounds must be non-zero".to_owned(),
            ));
        }
        let layout = scan_layout(&mut reader, admitted_len, None, control)?;
        if layout.state_digest != expected_state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match the admitted source".to_owned(),
            ));
        }
        let cursor = VerifiedSealedLexicalCursorV1::initial(
            layout.state_digest.clone(),
            layout.first_file_offset,
        )?;
        let total_lexical_bytes = layout
            .files_end_offset
            .checked_sub(layout.first_file_offset)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical files array has an invalid byte span".to_owned(),
                )
            })?;
        Ok(Self {
            reader,
            file_count: layout.file_count,
            first_file_offset: layout.first_file_offset,
            files_end_offset: layout.files_end_offset,
            total_lexical_bytes,
            maximum_file_bytes: layout.maximum_file_bytes,
            source_state_digest: layout.state_digest,
            format_revision: layout.format_revision,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_file: None,
        })
    }

    /// Open a durable sealed source through its content address.
    ///
    /// Unlike [`Self::open`], whose caller already holds the envelope's inner
    /// state digest, this journey binds the complete file bytes to the digest
    /// in the durable generation index while the same bounded scan discovers
    /// the lexical layout. The caller can therefore pass a `File` directly;
    /// no whole-generation `Vec` is required merely to authenticate it.
    #[hotpath::measure]
    pub fn open_content_addressed(
        mut reader: R,
        admitted_len: u64,
        expected_file_digest: ManifestDigest,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if maximum_page_chunks == 0 || maximum_page_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page bounds must be non-zero".to_owned(),
            ));
        }
        let layout = scan_layout(
            &mut reader,
            admitted_len,
            Some(&expected_file_digest),
            control,
        )?;
        let cursor = VerifiedSealedLexicalCursorV1::initial(
            layout.state_digest.clone(),
            layout.first_file_offset,
        )?;
        let total_lexical_bytes = layout
            .files_end_offset
            .checked_sub(layout.first_file_offset)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical files array has an invalid byte span".to_owned(),
                )
            })?;
        Ok(Self {
            reader,
            file_count: layout.file_count,
            first_file_offset: layout.first_file_offset,
            files_end_offset: layout.files_end_offset,
            total_lexical_bytes,
            maximum_file_bytes: layout.maximum_file_bytes,
            source_state_digest: layout.state_digest,
            format_revision: layout.format_revision,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_file: None,
        })
    }

    /// Reopen an authenticated durable source at an accepted persisted cursor.
    ///
    /// The layout scan authenticates the raw content address but does not
    /// deserialize file artifacts. Resume validates only the cursor's next
    /// artifact and emits that page first; previously admitted artifacts are
    /// never decoded or replayed on the reopen path.
    pub fn open_content_addressed_at(
        reader: R,
        admitted_len: u64,
        expected_file_digest: ManifestDigest,
        cursor: VerifiedSealedLexicalCursorV1,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut source = Self::open_content_addressed(
            reader,
            admitted_len,
            expected_file_digest,
            maximum_page_chunks,
            maximum_page_bytes,
            control,
        )?;
        source.restore_cursor(&cursor, control)?;
        Ok(source)
    }

    /// Adopt a persisted cursor after binding it to this source and validating
    /// its first unread file. This deliberately never walks earlier files.
    #[hotpath::measure]
    pub fn restore_cursor(
        &mut self,
        cursor: &VerifiedSealedLexicalCursorV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        checkpoint(control)?;
        cursor.verify_source(&self.source_state_digest)?;
        self.admitted_file = None;
        if cursor.next_file_ordinal > self.file_count {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical cursor exceeds the admitted file layout".to_owned(),
            ));
        }
        if cursor.next_file_ordinal == self.file_count {
            if cursor.next_chunk_ordinal != 0
                || cursor.next_import_ordinal != 0
                || cursor.next_file_offset != self.files_end_offset
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "completed sealed lexical cursor has a non-terminal file position".to_owned(),
                ));
            }
        } else {
            if cursor.next_file_offset < self.first_file_offset
                || cursor.next_file_offset >= self.files_end_offset
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor byte offset is outside the files array".to_owned(),
                ));
            }
            self.ensure_admitted_file(cursor.next_file_offset, control)?;
            let admitted = &self
                .admitted_file
                .as_ref()
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical admitted-file cache is missing".to_owned(),
                    )
                })?
                .1;
            let chunk_count = u64::try_from(admitted.chunks.len()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file chunk count exceeds u64".to_owned(),
                )
            })?;
            let import_count = u64::try_from(admitted.imports.len()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file import count exceeds u64".to_owned(),
                )
            })?;
            if cursor.next_chunk_ordinal > chunk_count
                || cursor.next_import_ordinal > import_count
                || (cursor.next_chunk_ordinal < chunk_count && cursor.next_import_ordinal != 0)
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor is not a valid position in its next file".to_owned(),
                ));
            }
        }
        self.cursor = cursor.clone();
        Ok(())
    }

    pub fn cursor(&self) -> &VerifiedSealedLexicalCursorV1 {
        &self.cursor
    }

    /// Number of authenticated file records in this sealed lexical source.
    pub fn total_files(&self) -> u64 {
        self.file_count
    }

    /// Authenticated files-array byte span available to the lexical source.
    pub fn total_lexical_bytes(&self) -> u64 {
        self.total_lexical_bytes
    }

    /// Fully completed file records at the durable source cursor.
    pub fn completed_files(&self) -> u64 {
        self.cursor.next_file_ordinal()
    }

    /// Authenticated files-array bytes fully passed by the durable source
    /// cursor. A partially consumed file counts only after its final chunk and
    /// imports are committed, matching `completed_files`.
    pub fn completed_lexical_bytes(&self) -> Result<u64, CodeIndexProductionErrorV1> {
        self.cursor
            .next_file_offset
            .checked_sub(self.first_file_offset)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor precedes the files-array start".to_owned(),
                )
            })
    }

    /// Restore this source to its just-opened state so a consumer whose
    /// staging failed after pages were already accepted can replay the same
    /// sealed pages on the same instance instead of terminally blocking.
    ///
    /// The verified structural layout and state digest are kept; only the
    /// cursor and the cumulative and import-dictionary hash authorities are
    /// reset to their canonical initial values.
    pub fn rewind(&mut self) -> Result<(), CodeIndexProductionErrorV1> {
        self.cursor = VerifiedSealedLexicalCursorV1::initial(
            self.source_state_digest.clone(),
            self.first_file_offset,
        )?;
        self.admitted_file = None;
        Ok(())
    }

    /// Serialized bytes this source may stage at once while minting a page:
    /// the largest admitted file's sealed byte range (files are decoded one
    /// at a time) plus the bounded page payload itself. Consumers charge this
    /// window against their memory ledgers before driving the source.
    pub fn staging_window_bytes(&self) -> usize {
        self.retained_layout_bytes()
            .saturating_add(usize::try_from(self.maximum_file_bytes).unwrap_or(usize::MAX))
            .saturating_add(self.maximum_page_bytes)
    }

    /// Fixed retained authority for locating files after the authenticated
    /// opening scan. This remains constant as the generation's file count
    /// grows; individual file boundaries are discovered from the byte cursor.
    pub fn retained_layout_bytes(&self) -> usize {
        std::mem::size_of::<u64>().saturating_mul(4)
    }

    #[hotpath::measure]
    pub fn next_page(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedSealedLexicalPageReadV1, CodeIndexProductionErrorV1> {
        match self.next_page_if(control, |_| Ok::<(), std::convert::Infallible>(()))? {
            Ok(read) => Ok(read),
            Err(never) => match never {},
        }
    }

    /// Stage one verified page and advance only after caller admission.
    ///
    /// Source failures use the outer result. A caller rejection uses the inner
    /// result and leaves the persisted cursor and cumulative hash authorities
    /// unchanged, so retrying yields the same source-minted page.
    pub fn next_page_if<E>(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
        admit: impl FnOnce(&VerifiedSealedLexicalPageV1) -> Result<(), E>,
    ) -> Result<Result<VerifiedSealedLexicalPageReadV1, E>, CodeIndexProductionErrorV1> {
        let cursor = self.cursor.clone();
        match self.stage_next_page_at(&cursor, control)? {
            StagedSealedLexicalPageReadV1::Page(staged) => {
                if let Err(error) = admit(&staged.page) {
                    return Ok(Err(error));
                }
                crate::hotpath_observe::record_pages(staged.cursor.next_page_ordinal());
                self.cursor = staged.cursor;
                Ok(Ok(VerifiedSealedLexicalPageReadV1::Page(staged.page)))
            }
            StagedSealedLexicalPageReadV1::Complete(receipt) => {
                Ok(Ok(VerifiedSealedLexicalPageReadV1::Complete(receipt)))
            }
        }
    }

    /// Stage a bounded ordered page batch and advance only through the prefix
    /// the caller durably accepts. The source cursor remains at its pre-batch
    /// position on source, callback, or accepted-prefix validation failure, so
    /// retrying emits the same ordered page sequence.
    pub fn next_page_batch_if<E>(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
        bounds: VerifiedSealedLexicalPageBatchBoundsV1,
        admit: impl FnOnce(&[VerifiedSealedLexicalPageV1]) -> Result<NonZeroUsize, E>,
    ) -> Result<Result<VerifiedSealedLexicalPageBatchReadV1, E>, CodeIndexProductionErrorV1> {
        let staged = hotpath::measure_block!("code_index.lexical_source.batch_stage", {
            (|| {
                let mut pages = Vec::new();
                pages
                    .try_reserve_exact(bounds.maximum_pages())
                    .map_err(|error| {
                        CodeIndexProductionErrorV1::Contract(format!(
                            "sealed lexical page batch reservation failed: {error}"
                        ))
                    })?;
                let retained_page_slots = pages
                    .capacity()
                    .checked_mul(std::mem::size_of::<VerifiedSealedLexicalPageV1>())
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical page batch reservation overflowed".to_owned(),
                        )
                    })?;
                if retained_page_slots > bounds.maximum_retained_bytes()
                    || retained_page_slots < bounds.page_slot_bytes()
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed lexical page batch reservation exceeds its retained-byte bound"
                            .to_owned(),
                    ));
                }

                let mut working_cursor = self.cursor.clone();
                let mut retained_bytes = retained_page_slots;
                let mut completion = None;
                while pages.len() < bounds.maximum_pages() {
                    match self.stage_next_page_at(&working_cursor, control)? {
                        StagedSealedLexicalPageReadV1::Page(staged) => {
                            let next_retained_bytes = retained_bytes
                                .checked_add(staged.page.retained_owned_bytes())
                                .ok_or_else(|| {
                                    CodeIndexProductionErrorV1::Contract(
                                        "sealed lexical page batch retained bytes overflowed"
                                            .to_owned(),
                                    )
                                })?;
                            if next_retained_bytes > bounds.maximum_retained_bytes() {
                                if pages.is_empty() {
                                    return Err(CodeIndexProductionErrorV1::Contract(
                                        "one sealed lexical page exceeds the batch retained-byte bound"
                                            .to_owned(),
                                    ));
                                }
                                break;
                            }
                            retained_bytes = next_retained_bytes;
                            working_cursor = staged.cursor;
                            pages.push(staged.page);
                        }
                        StagedSealedLexicalPageReadV1::Complete(receipt) => {
                            if pages.is_empty() {
                                completion = Some(receipt);
                            }
                            break;
                        }
                    }
                }

                Ok(if let Some(receipt) = completion {
                    StagedSealedLexicalPageBatchReadV1::Complete(receipt)
                } else {
                    StagedSealedLexicalPageBatchReadV1::Pages(pages)
                })
            })()
        })?;

        match staged {
            StagedSealedLexicalPageBatchReadV1::Complete(receipt) => {
                Ok(Ok(VerifiedSealedLexicalPageBatchReadV1::Complete(receipt)))
            }
            StagedSealedLexicalPageBatchReadV1::Pages(mut pages) => {
                let accepted_prefix = match admit(&pages) {
                    Ok(accepted_prefix) => accepted_prefix,
                    Err(error) => return Ok(Err(error)),
                };
                let accepted_page_count = accepted_prefix.get();
                if accepted_page_count > pages.len() {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed lexical page batch accepted prefix exceeds staged page count"
                            .to_owned(),
                    ));
                }
                let accepted_cursor = pages[accepted_page_count - 1].next_cursor().clone();
                pages.truncate(accepted_page_count);
                crate::hotpath_observe::record_pages(accepted_cursor.next_page_ordinal());
                self.cursor = accepted_cursor;
                Ok(Ok(VerifiedSealedLexicalPageBatchReadV1::Pages(pages)))
            }
        }
    }

    fn stage_next_page_at(
        &mut self,
        previous_cursor: &VerifiedSealedLexicalCursorV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<StagedSealedLexicalPageReadV1, CodeIndexProductionErrorV1> {
        checkpoint(control)?;
        let mut cursor = previous_cursor.clone();
        let mut page_hasher = page_hasher(cursor.next_page_ordinal);
        let mut chunks = Vec::new();
        let mut page_bytes = 0usize;
        let mut imports = Vec::new();
        let mut import_bytes = 0usize;

        while cursor.next_file_ordinal < self.file_count {
            checkpoint(control)?;
            self.ensure_admitted_file(cursor.next_file_offset, control)?;
            let admitted = &self
                .admitted_file
                .as_ref()
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical admitted-file cache is missing".to_owned(),
                    )
                })?
                .1;
            let mut chunk_ordinal = usize::try_from(cursor.next_chunk_ordinal).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical chunk cursor exceeds the platform limit".to_owned(),
                )
            })?;
            if chunk_ordinal > admitted.chunks.len() {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor exceeds its file chunk count".to_owned(),
                ));
            }
            while chunk_ordinal < admitted.chunks.len() {
                checkpoint(control)?;
                let serialized = serde_json::to_vec(admitted.chunks[chunk_ordinal].chunk())
                    .map_err(|error| {
                        CodeIndexProductionErrorV1::Contract(format!(
                            "sealed lexical chunk serialization failed: {error}"
                        ))
                    })?;
                if serialized.len() > self.maximum_page_bytes {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "one admitted lexical chunk exceeds the page byte bound".to_owned(),
                    ));
                }
                if (!chunks.is_empty() || !imports.is_empty())
                    && (chunks.len() == self.maximum_page_chunks
                        || page_bytes
                            .saturating_add(import_bytes)
                            .saturating_add(serialized.len())
                            > self.maximum_page_bytes)
                {
                    return self.commit_page(
                        previous_cursor,
                        PendingSealedLexicalPageV1 {
                            chunks,
                            page_bytes,
                            imports,
                            import_bytes,
                            cursor,
                            page_hasher,
                        },
                    );
                }
                hash_record(&mut page_hasher, &serialized)?;
                cursor.cumulative_digest = advance_digest(
                    &cursor.cumulative_digest,
                    SOURCE_CHAIN_RECORD_DOMAIN,
                    &serialized,
                )?;
                page_bytes = page_bytes.checked_add(serialized.len()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical page byte count overflowed".to_owned(),
                    )
                })?;
                chunks.push(admitted.chunks[chunk_ordinal].clone());
                chunk_ordinal += 1;
                cursor.next_chunk_ordinal = u64::try_from(chunk_ordinal).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical chunk ordinal exceeds u64".to_owned(),
                    )
                })?;
                if chunks.len() == self.maximum_page_chunks {
                    return self.commit_page(
                        previous_cursor,
                        PendingSealedLexicalPageV1 {
                            chunks,
                            page_bytes,
                            imports,
                            import_bytes,
                            cursor,
                            page_hasher,
                        },
                    );
                }
            }
            let mut import_ordinal = usize::try_from(cursor.next_import_ordinal).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical import cursor exceeds the platform limit".to_owned(),
                )
            })?;
            if import_ordinal > admitted.imports.len() {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor exceeds its file import count".to_owned(),
                ));
            }
            while import_ordinal < admitted.imports.len() {
                checkpoint(control)?;
                let evidence = &admitted.imports[import_ordinal];
                let serialized = serde_json::to_vec(evidence).map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "sealed lexical import serialization failed: {error}"
                    ))
                })?;
                if serialized.len() > self.maximum_page_bytes {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "one admitted lexical import exceeds the page byte bound".to_owned(),
                    ));
                }
                if (!chunks.is_empty() || !imports.is_empty())
                    && page_bytes
                        .saturating_add(import_bytes)
                        .saturating_add(serialized.len())
                        > self.maximum_page_bytes
                {
                    return self.commit_page(
                        previous_cursor,
                        PendingSealedLexicalPageV1 {
                            chunks,
                            page_bytes,
                            imports,
                            import_bytes,
                            cursor,
                            page_hasher,
                        },
                    );
                }
                hash_import_record(&mut page_hasher, &serialized)?;
                cursor.cumulative_digest =
                    advance_digest(&cursor.cumulative_digest, IMPORT_RECORD_DOMAIN, &serialized)?;
                cursor.import_dictionary_digest = advance_digest(
                    &cursor.import_dictionary_digest,
                    IMPORT_DICTIONARY_CHAIN_RECORD_DOMAIN,
                    &serialized,
                )?;
                import_bytes = import_bytes.checked_add(serialized.len()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical import page byte count overflowed".to_owned(),
                    )
                })?;
                imports.push(evidence.clone());
                import_ordinal += 1;
                cursor.next_import_ordinal = u64::try_from(import_ordinal).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical import ordinal exceeds u64".to_owned(),
                    )
                })?;
            }
            let next_file_offset = admitted.next_file_offset;
            self.admitted_file = None;
            cursor.next_file_ordinal =
                cursor.next_file_ordinal.checked_add(1).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file ordinal overflowed".to_owned(),
                    )
                })?;
            cursor.next_file_offset = next_file_offset;
            cursor.next_chunk_ordinal = 0;
            cursor.next_import_ordinal = 0;
            // The page contract serializes every chunk before every import.
            // Commit after an importing file so a later file cannot append a
            // chunk after bytes already hashed as import records.
            if !imports.is_empty() {
                return self.commit_page(
                    previous_cursor,
                    PendingSealedLexicalPageV1 {
                        chunks,
                        page_bytes,
                        imports,
                        import_bytes,
                        cursor,
                        page_hasher,
                    },
                );
            }
        }

        if !chunks.is_empty() || !imports.is_empty() {
            return self.commit_page(
                previous_cursor,
                PendingSealedLexicalPageV1 {
                    chunks,
                    page_bytes,
                    imports,
                    import_bytes,
                    cursor,
                    page_hasher,
                },
            );
        }
        Ok(StagedSealedLexicalPageReadV1::Complete(
            VerifiedSealedLexicalSourceReceiptV1 {
                source_state_digest: self.source_state_digest.clone(),
                format_revision: self.format_revision,
                page_count: previous_cursor.next_page_ordinal,
                total_chunks: previous_cursor.emitted_chunks,
                total_payload_bytes: previous_cursor.emitted_payload_bytes,
                total_imports: previous_cursor.emitted_imports,
                import_payload_bytes: previous_cursor.emitted_import_payload_bytes,
                import_dictionary_digest: previous_cursor.import_dictionary_digest.clone(),
                cumulative_digest: previous_cursor.cumulative_digest.clone(),
            },
        ))
    }

    fn read_admitted_file(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
        let (bytes, next_file_offset) = read_next_file_bytes(
            &mut self.reader,
            file_offset,
            self.files_end_offset,
            self.maximum_file_bytes,
            control,
        )?;
        let file: PersistedFileGenerationArtifactsV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical file decoding failed: {error}"
                ))
            })?;
        file.artifacts
            .validate()
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        file.artifacts
            .validate_generation_import_authority(&file.extraction)
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        let document = &file.artifacts.chunks.document;
        if file.extraction.file_occurrence_id != document.file_occurrence_id
            || file.extraction.content_digest != document.content_digest
            || file.authority.content_digest != document.content_digest
            || file
                .artifacts
                .chunks
                .chunks
                .iter()
                .any(|chunk| chunk.anchor.generation_id != file.extraction.generation_id)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical extraction authority does not match its admitted document"
                    .to_owned(),
            ));
        }
        let exact_authority = ExactExtractionAuthorityV1::restore(&file.artifacts.chunks)
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        let imports = file.artifacts.imports;
        let chunks = exact_authority
            .admit_all(file.artifacts.chunks.chunks)
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        Ok(AdmittedSealedLexicalFileV1 {
            chunks,
            imports,
            next_file_offset,
        })
    }

    fn ensure_admitted_file(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if self
            .admitted_file
            .as_ref()
            .is_some_and(|(cached_offset, _)| *cached_offset == file_offset)
        {
            return Ok(());
        }
        let admitted = self.read_admitted_file(file_offset, control)?;
        self.admitted_file = Some((file_offset, admitted));
        Ok(())
    }

    fn commit_page(
        &mut self,
        previous_cursor: &VerifiedSealedLexicalCursorV1,
        pending: PendingSealedLexicalPageV1,
    ) -> Result<StagedSealedLexicalPageReadV1, CodeIndexProductionErrorV1> {
        let PendingSealedLexicalPageV1 {
            chunks,
            page_bytes,
            imports,
            import_bytes,
            mut cursor,
            mut page_hasher,
        } = pending;
        let page_ordinal = cursor.next_page_ordinal;
        let chunk_count = u64::try_from(chunks.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page chunk count exceeds u64".to_owned(),
            )
        })?;
        let payload_bytes = u64::try_from(page_bytes).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page byte count exceeds u64".to_owned(),
            )
        })?;
        let import_count = u64::try_from(imports.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page import count exceeds u64".to_owned(),
            )
        })?;
        let import_payload_bytes = u64::try_from(import_bytes).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page import byte count exceeds u64".to_owned(),
            )
        })?;
        cursor.next_page_ordinal = cursor.next_page_ordinal.checked_add(1).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page ordinal overflowed".to_owned(),
            )
        })?;
        cursor.emitted_chunks =
            cursor
                .emitted_chunks
                .checked_add(chunk_count)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical source chunk count overflowed".to_owned(),
                    )
                })?;
        cursor.emitted_payload_bytes = cursor
            .emitted_payload_bytes
            .checked_add(payload_bytes)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical source byte count overflowed".to_owned(),
                )
            })?;
        cursor.emitted_imports = cursor
            .emitted_imports
            .checked_add(import_count)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical source import count overflowed".to_owned(),
                )
            })?;
        cursor.emitted_import_payload_bytes = cursor
            .emitted_import_payload_bytes
            .checked_add(import_payload_bytes)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical source import byte count overflowed".to_owned(),
                )
            })?;
        hash_cursor(&mut page_hasher, &cursor)?;
        let page = VerifiedSealedLexicalPageV1 {
            page_ordinal,
            chunk_count,
            payload_bytes,
            import_count,
            import_payload_bytes,
            page_digest: digest_hasher(page_hasher)?,
            cumulative_digest: cursor.cumulative_digest.clone(),
            next_cursor: cursor.clone(),
            chunks,
            imports,
            previous_cursor: previous_cursor.clone(),
        };
        Ok(StagedSealedLexicalPageReadV1::Page(
            StagedSealedLexicalPageV1 { page, cursor },
        ))
    }
}

#[derive(Debug)]
struct AdmittedSealedLexicalFileV1 {
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    next_file_offset: u64,
}

struct SealedLexicalLayoutV1 {
    state_digest: ManifestDigest,
    format_revision: u32,
    file_count: u64,
    first_file_offset: u64,
    files_end_offset: u64,
    maximum_file_bytes: u64,
}

fn scan_layout<R: Read + Seek>(
    reader: &mut R,
    admitted_len: u64,
    expected_file_digest: Option<&ManifestDigest>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<SealedLexicalLayoutV1, CodeIndexProductionErrorV1> {
    if admitted_len > MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation exceeds the canonical byte limit".to_owned(),
        ));
    }
    reader.seek(SeekFrom::Start(0)).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed lexical source seek failed: {error}"))
    })?;
    let mut scanner = LayoutScanner::default();
    let mut file_hasher = expected_file_digest.map(|_| Sha256::new());
    let read_limit = admitted_len.checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed generation length overflowed".to_owned())
    })?;
    let mut remaining = read_limit;
    let mut observed = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        checkpoint(control)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical read window exceeds the platform limit".to_owned(),
            )
        })?;
        let read = reader.read(&mut buffer[..requested]).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed lexical source read failed: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        let read_bytes = u64::try_from(read).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical source read exceeds u64".to_owned(),
            )
        })?;
        // Only bytes below the admitted length are hashed and scanned; split
        // the buffer at that boundary and feed whole slices, not single bytes.
        let admitted = usize::try_from(read_bytes.min(admitted_len.saturating_sub(observed)))
            .map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical read window exceeds the platform limit".to_owned(),
                )
            })?;
        if admitted > 0 {
            if let Some(hasher) = file_hasher.as_mut() {
                hasher.update(&buffer[..admitted]);
            }
            scanner.observe_slice(&buffer[..admitted], observed)?;
        }
        observed = observed.checked_add(read_bytes).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical source length overflowed".to_owned(),
            )
        })?;
        remaining -= read_bytes;
    }
    if observed != admitted_len {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation length does not match its admitted length".to_owned(),
        ));
    }
    if let (Some(expected), Some(hasher)) = (expected_file_digest, file_hasher)
        && digest_hasher(hasher)? != *expected
    {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed lexical source bytes do not match their durable content address".to_owned(),
        ));
    }
    scanner.finish()
}

#[derive(Default)]
struct LayoutScanner {
    brace_depth: usize,
    bracket_depth: usize,
    in_string: bool,
    escaped: bool,
    string: Vec<u8>,
    string_overflowed: bool,
    completed_string: Option<String>,
    pending_key: Option<String>,
    capture_state_digest: bool,
    state_digest: Option<ManifestDigest>,
    format_revision: Option<u32>,
    generation_depth: Option<usize>,
    generation_hasher: Option<Sha256>,
    generation_digest: Option<ManifestDigest>,
    files_depth: Option<usize>,
    current_file_start: Option<u64>,
    first_file_offset: Option<u64>,
    files_end_offset: Option<u64>,
    file_count: u64,
    maximum_file_bytes: u64,
}

/// Transition of the generation-payload hash span produced by one observed
/// byte.
enum GenerationSpanEvent {
    None,
    Opened,
    Closed,
}

impl LayoutScanner {
    /// Observe one contiguous run of admitted bytes starting at `base_offset`.
    ///
    /// The generation hasher receives one update per contiguous in-generation
    /// byte range instead of one update per byte; the hashed bytes and their
    /// order are identical.
    fn observe_slice(
        &mut self,
        bytes: &[u8],
        base_offset: u64,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let mut active_from = self.generation_hasher.is_some().then_some(0usize);
        for (index, &byte) in bytes.iter().enumerate() {
            let offset = u64::try_from(index)
                .ok()
                .and_then(|index| base_offset.checked_add(index))
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical source length overflowed".to_owned(),
                    )
                })?;
            match self.observe(byte, offset)? {
                GenerationSpanEvent::None => {}
                GenerationSpanEvent::Opened => active_from = Some(index),
                GenerationSpanEvent::Closed => {
                    let start = active_from.take().ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed generation digest state is missing".to_owned(),
                        )
                    })?;
                    let mut hasher = self.generation_hasher.take().ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed generation digest state is missing".to_owned(),
                        )
                    })?;
                    hasher.update(&bytes[start..=index]);
                    self.generation_digest = Some(digest_hasher(hasher)?);
                }
            }
        }
        if let Some(hasher) = self.generation_hasher.as_mut() {
            let start = active_from.ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation digest state is missing".to_owned(),
                )
            })?;
            hasher.update(&bytes[start..]);
        }
        Ok(())
    }

    fn observe(
        &mut self,
        byte: u8,
        offset: u64,
    ) -> Result<GenerationSpanEvent, CodeIndexProductionErrorV1> {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
                if self.string.len() < 128 {
                    self.string.push(byte);
                } else {
                    self.string_overflowed = true;
                }
                return Ok(GenerationSpanEvent::None);
            }
            match byte {
                b'\\' => self.escaped = true,
                b'"' => {
                    self.in_string = false;
                    if self.capture_state_digest {
                        let value =
                            String::from_utf8(std::mem::take(&mut self.string)).map_err(|_| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed generation state digest is not UTF-8".to_owned(),
                                )
                            })?;
                        self.state_digest = Some(ManifestDigest::new(value).map_err(|error| {
                            CodeIndexProductionErrorV1::Contract(error.to_string())
                        })?);
                        self.capture_state_digest = false;
                        self.pending_key = None;
                    } else if !self.string_overflowed {
                        self.completed_string = Some(
                            String::from_utf8(std::mem::take(&mut self.string)).map_err(|_| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed generation key is not UTF-8".to_owned(),
                                )
                            })?,
                        );
                    } else {
                        self.string.clear();
                        self.completed_string = None;
                    }
                    self.string_overflowed = false;
                }
                _ => {
                    if self.string.len() < 128 {
                        self.string.push(byte);
                    } else {
                        self.string_overflowed = true;
                    }
                }
            }
            return Ok(GenerationSpanEvent::None);
        }

        let mut event = GenerationSpanEvent::None;
        match byte {
            b'"' => {
                self.in_string = true;
                self.string.clear();
                self.string_overflowed = false;
                self.capture_state_digest =
                    self.pending_key.as_deref() == Some("state_digest") && self.brace_depth == 1;
            }
            b':' => self.pending_key = self.completed_string.take(),
            b'{' => {
                if self.pending_key.as_deref() == Some("generation") && self.brace_depth == 1 {
                    self.generation_depth = Some(self.brace_depth + 1);
                    self.generation_hasher = Some(Sha256::new());
                    event = GenerationSpanEvent::Opened;
                }
                if self.files_depth == Some(self.bracket_depth)
                    && self.generation_depth == Some(self.brace_depth)
                    && self.current_file_start.is_none()
                {
                    self.current_file_start = Some(offset);
                }
                self.brace_depth += 1;
                self.pending_key = None;
            }
            b'}' => {
                if let Some(start) = self.current_file_start
                    && self
                        .generation_depth
                        .is_some_and(|depth| self.brace_depth == depth + 1)
                {
                    let end = offset.checked_add(1).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical file end offset overflowed".to_owned(),
                        )
                    })?;
                    let byte_len = end.checked_sub(start).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical file byte range is invalid".to_owned(),
                        )
                    })?;
                    self.first_file_offset.get_or_insert(start);
                    self.maximum_file_bytes = self.maximum_file_bytes.max(byte_len);
                    self.file_count = self.file_count.checked_add(1).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical file count overflowed".to_owned(),
                        )
                    })?;
                    self.current_file_start = None;
                }
                if self.generation_depth == Some(self.brace_depth) {
                    event = GenerationSpanEvent::Closed;
                }
                self.brace_depth = self.brace_depth.checked_sub(1).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation object nesting is invalid".to_owned(),
                    )
                })?;
                self.pending_key = None;
            }
            b'[' => {
                if self.pending_key.as_deref() == Some("files")
                    && self.generation_depth == Some(self.brace_depth)
                {
                    self.files_depth = Some(self.bracket_depth + 1);
                }
                self.bracket_depth += 1;
                self.pending_key = None;
            }
            b']' => {
                if self.files_depth == Some(self.bracket_depth) {
                    self.files_end_offset = Some(offset);
                    self.files_depth = None;
                }
                self.bracket_depth = self.bracket_depth.checked_sub(1).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation array nesting is invalid".to_owned(),
                    )
                })?;
                self.pending_key = None;
            }
            b'0'..=b'9'
                if self.pending_key.as_deref() == Some("format_revision")
                    && self.generation_depth == Some(self.brace_depth) =>
            {
                self.format_revision = Some(u32::from(byte - b'0'));
                self.pending_key = None;
            }
            b',' => {
                self.completed_string = None;
                self.pending_key = None;
            }
            byte if byte.is_ascii_whitespace() => {}
            _ => self.completed_string = None,
        }
        Ok(event)
    }

    fn finish(self) -> Result<SealedLexicalLayoutV1, CodeIndexProductionErrorV1> {
        if self.in_string
            || self.brace_depth != 0
            || self.bracket_depth != 0
            || self.current_file_start.is_some()
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical source has incomplete JSON structure".to_owned(),
            ));
        }
        let state_digest = self.state_digest.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest is missing".to_owned(),
            )
        })?;
        let generation_digest = self.generation_digest.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("sealed generation payload is missing".to_owned())
        })?;
        if generation_digest != state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match its payload".to_owned(),
            ));
        }
        let format_revision = self.format_revision.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation format revision is missing".to_owned(),
            )
        })?;
        if !matches!(
            format_revision,
            LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION
                | SEALED_GENERATION_FORMAT_REVISION_V1
        ) {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation format revision is incompatible".to_owned(),
            ));
        }
        let files_end_offset = self.files_end_offset.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation files array is missing".to_owned(),
            )
        })?;
        let first_file_offset = self.first_file_offset.unwrap_or(files_end_offset);
        Ok(SealedLexicalLayoutV1 {
            state_digest,
            format_revision,
            file_count: self.file_count,
            first_file_offset,
            files_end_offset,
            maximum_file_bytes: self.maximum_file_bytes,
        })
    }
}

fn read_next_file_bytes<R: Read + Seek>(
    reader: &mut R,
    file_offset: u64,
    files_end_offset: u64,
    maximum_file_bytes: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(Vec<u8>, u64), CodeIndexProductionErrorV1> {
    if file_offset >= files_end_offset {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed lexical file cursor is outside the admitted source".to_owned(),
        ));
    }
    let maximum_file_bytes = usize::try_from(maximum_file_bytes).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed lexical file window exceeds the platform limit".to_owned(),
        )
    })?;
    reader.seek(SeekFrom::Start(file_offset)).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed lexical source seek failed: {error}"))
    })?;
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut bytes = Vec::new();
    let mut offset = file_offset;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut complete = false;
    let mut saw_separator = false;

    loop {
        checkpoint(control)?;
        let available = reader.fill_buf().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed lexical file read failed: {error}"
            ))
        })?;
        if available.is_empty() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical file ended before the files array closed".to_owned(),
            ));
        }
        let mut consumed = 0usize;
        for &byte in available {
            if offset > files_end_offset {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file crossed the admitted files array".to_owned(),
                ));
            }
            if !complete {
                if bytes.len() == maximum_file_bytes {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file exceeds its admitted decode window".to_owned(),
                    ));
                }
                bytes.push(byte);
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                } else {
                    match byte {
                        b'"' => in_string = true,
                        b'{' => {
                            brace_depth = brace_depth.checked_add(1).ok_or_else(|| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed lexical file nesting overflowed".to_owned(),
                                )
                            })?
                        }
                        b'}' => {
                            brace_depth = brace_depth.checked_sub(1).ok_or_else(|| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed lexical file nesting is invalid".to_owned(),
                                )
                            })?;
                            if brace_depth == 0 {
                                complete = true;
                            }
                        }
                        _ if bytes.len() == 1 => {
                            return Err(CodeIndexProductionErrorV1::Contract(
                                "sealed lexical file does not begin with an object".to_owned(),
                            ));
                        }
                        _ => {}
                    }
                }
            } else if !byte.is_ascii_whitespace() {
                if byte == b',' && !saw_separator {
                    saw_separator = true;
                } else if byte == b'{' && saw_separator {
                    return Ok((bytes, offset));
                } else if byte == b']' && !saw_separator && offset == files_end_offset {
                    return Ok((bytes, files_end_offset));
                } else {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed lexical files array separators are invalid".to_owned(),
                    ));
                }
            }
            consumed += 1;
            offset = offset.checked_add(1).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file cursor overflowed".to_owned(),
                )
            })?;
        }
        reader.consume(consumed);
    }
}

fn checkpoint(control: &dyn CodeIndexExecutionControlV1) -> Result<(), CodeIndexProductionErrorV1> {
    if control.is_cancelled() {
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled,
        ))
    } else if control.is_deadline_exceeded() {
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::DeadlineExceeded,
        ))
    } else {
        Ok(())
    }
}

fn initial_digest(domain: &[u8]) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    digest_hasher(hasher)
}

fn advance_digest(
    previous: &ManifestDigest,
    record_domain: &[u8],
    bytes: &[u8],
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(record_domain);
    hash_record(&mut hasher, previous.as_str().as_bytes())?;
    hash_record(&mut hasher, bytes)?;
    digest_hasher(hasher)
}

fn page_hasher(page_ordinal: u64) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PAGE_DIGEST_DOMAIN);
    hasher.update(page_ordinal.to_le_bytes());
    hasher
}

fn hash_cursor(
    hasher: &mut Sha256,
    cursor: &VerifiedSealedLexicalCursorV1,
) -> Result<(), CodeIndexProductionErrorV1> {
    hash_record(hasher, cursor.source_state_digest.as_str().as_bytes())?;
    hasher.update(cursor.next_file_ordinal.to_le_bytes());
    hasher.update(cursor.next_chunk_ordinal.to_le_bytes());
    hasher.update(cursor.next_import_ordinal.to_le_bytes());
    hasher.update(cursor.next_page_ordinal.to_le_bytes());
    hasher.update(cursor.emitted_chunks.to_le_bytes());
    hasher.update(cursor.emitted_payload_bytes.to_le_bytes());
    hasher.update(cursor.emitted_imports.to_le_bytes());
    hasher.update(cursor.emitted_import_payload_bytes.to_le_bytes());
    hash_record(hasher, cursor.import_dictionary_digest.as_str().as_bytes())?;
    hash_record(hasher, cursor.cumulative_digest.as_str().as_bytes())
}

fn hash_import_record(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), CodeIndexProductionErrorV1> {
    hasher.update(IMPORT_RECORD_DOMAIN);
    hash_record(hasher, bytes)
}

fn hash_record(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), CodeIndexProductionErrorV1> {
    let byte_len = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("sealed lexical digest record exceeds u64".to_owned())
    })?;
    hasher.update(byte_len.to_le_bytes());
    hasher.update(bytes);
    Ok(())
}

fn digest_hasher(hasher: Sha256) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

#[cfg(test)]
mod lexical_page_source_tests {
    use std::{
        collections::BTreeSet,
        io::Cursor,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use tracedecay_domain::{
        ChunkerRevision, FileOccurrenceId, LanguageId, ManifestDigest, PolicyRevisionId,
        PrivacyDomainId, ProjectId, ProjectionKeyV1, ProjectionKindV1, RepositoryDirtyStateV1,
        RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
        SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, UtcMicros,
    };

    use super::*;
    use crate::{
        chunks::content_digest,
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
            CodeIndexGenerationScopeV1, CodeIndexInterruptionV1, CodeIndexProductionConfigV1,
            CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1,
            CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
            CodeIndexRepositoryParseIdentityV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
            ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
        },
    };

    const BATCH_FIXTURE_SOURCE: &str = concat!(
        "pub fn first_batch_page() -> usize { 1 }\n",
        "pub fn retained_batch_page() -> &'static str { ",
        "\"retained-batch-page-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\" }\n",
        "pub fn final_batch_page() -> usize { 3 }\n",
    );

    #[derive(Default)]
    struct TestPublicationStore;

    impl CodeIndexAtomicPublicationPort for TestPublicationStore {
        fn load_active(
            &self,
            _scope: &CodeIndexGenerationScopeV1,
        ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1>
        {
            Ok(None)
        }

        fn publish_atomically(
            &mut self,
            _scope: &CodeIndexGenerationScopeV1,
            _expected_active_generation: Option<&tracedecay_domain::CodeGenerationId>,
            _generation: Arc<CodeIndexPublishedGenerationV1>,
        ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct ApplyingProjectionSink;

    impl CodeChunkProjectionSink for ApplyingProjectionSink {
        fn project_changed_chunks(
            &mut self,
            request: &tracedecay_domain::ProjectionBatchRequestV1,
            receipt_builder: ProjectionReceiptBuilderV1<'_>,
        ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
            let mut decisions: Vec<ChunkProjectionDecisionV1> = request
                .changes
                .added_or_changed
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: if change.prior_digest.is_some() {
                        tracedecay_domain::ProjectionOperationV1::Updated
                    } else {
                        tracedecay_domain::ProjectionOperationV1::Added
                    },
                    outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
                    output_digest: Some(
                        change
                            .current_digest
                            .clone()
                            .expect("added or changed chunks have a digest"),
                    ),
                })
                .collect();
            decisions.extend(request.changes.deleted.iter().map(|change| {
                ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: tracedecay_domain::ProjectionOperationV1::Deleted,
                    outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }
            }));
            decisions.extend(request.changes.reused.iter().map(|change| {
                ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: tracedecay_domain::ProjectionOperationV1::Reused,
                    outcome: tracedecay_domain::ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }
            }));
            receipt_builder
                .build(&decisions)
                .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
        }
    }

    struct CancelDuringStaging {
        checks: AtomicUsize,
    }

    impl CancelDuringStaging {
        fn new() -> Self {
            Self {
                checks: AtomicUsize::new(0),
            }
        }
    }

    impl CodeIndexExecutionControlV1 for CancelDuringStaging {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::AcqRel) >= 3
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    struct ActiveControl;

    impl CodeIndexExecutionControlV1 for ActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    struct SealedSourceFixture {
        sealed: Vec<u8>,
        state_digest: ManifestDigest,
    }

    impl SealedSourceFixture {
        fn open(&self) -> VerifiedSealedLexicalPageSourceV1<Cursor<Vec<u8>>> {
            VerifiedSealedLexicalPageSourceV1::open(
                Cursor::new(self.sealed.clone()),
                u64::try_from(self.sealed.len()).expect("sealed fixture length fits u64"),
                self.state_digest.clone(),
                1,
                1024 * 1024,
                &ActiveControl,
            )
            .expect("real sealed fixture source opens")
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OnePageExpectation {
        page_ordinal: u64,
        chunk_count: u64,
        payload_bytes: u64,
        import_count: u64,
        import_payload_bytes: u64,
        page_digest: String,
        next_cursor: Vec<u8>,
        retained_owned_bytes: usize,
    }

    fn fixture() -> SealedSourceFixture {
        fixture_for_source(BATCH_FIXTURE_SOURCE)
    }

    fn fixture_for_source(source: &str) -> SealedSourceFixture {
        let source = source.as_bytes();
        let file = SanitizedCodeFileV1 {
            file_occurrence_id: FileOccurrenceId::new("file.lexical-page-batch")
                .expect("fixture file occurrence ID"),
            logical_path: "src/batch_fixture.rs".to_owned(),
            language: Some(LanguageId::new("rust").expect("fixture language ID")),
            content_digest: content_digest(source),
            disposition: SnapshotFileDispositionV1::Present,
        };
        let snapshot = SanitizedCodeSnapshotV1 {
            repository: RepositoryId::new("repository.lexical-page-batch")
                .expect("fixture repository ID"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: SanitizerRevision::new("sanitizer.lexical-page-batch")
                .expect("fixture sanitizer revision"),
            sanitization_receipts: vec![
                SanitizationReceiptId::new("receipt.lexical-page-batch")
                    .expect("fixture sanitization receipt"),
            ],
            content_identity: content_digest(source),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        };
        let request = CodeIndexBuildRequestV1 {
            snapshot,
            captured_files: vec![CodeIndexCapturedFileV1 {
                file_occurrence_id: file.file_occurrence_id,
                sanitized_bytes: source.to_vec(),
                sensitivity_level: SensitivityLevelV1::Public,
            }],
            changed_files: BTreeSet::new(),
            invalidations: BTreeSet::new(),
            ignored_source_admissions: Vec::new(),
            repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
                tree: None,
                dirty: RepositoryDirtyStateV1::Dirty,
            },
            sealed_at: UtcMicros(1_100_000),
            target_projection_key: ProjectionKeyV1 {
                kind: ProjectionKindV1::Lexical,
                schema_revision: "lexical.v1".to_owned(),
                profile_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                    .expect("fixture projection profile digest"),
            },
        };
        let mut owner = CodeIndexProductionOwnerV1::new(
            CodeIndexProductionConfigV1 {
                project_id: ProjectId::new("project.lexical-page-batch")
                    .expect("fixture project ID"),
                repository: RepositoryId::new("repository.lexical-page-batch")
                    .expect("fixture repository ID"),
                sanitizer_revision: SanitizerRevision::new("sanitizer.lexical-page-batch")
                    .expect("fixture sanitizer revision"),
                policy_revision: PolicyRevisionId::new("policy.lexical-page-batch")
                    .expect("fixture policy revision"),
                chunker_revision: ChunkerRevision::new("chunker.lexical-page-batch")
                    .expect("fixture chunker revision"),
                privacy_domain: PrivacyDomainId::new("privacy.lexical-page-batch")
                    .expect("fixture privacy domain"),
                privacy_key_epoch: 7,
                max_snapshot_age_micros: None,
            },
            TestPublicationStore,
            ApplyingProjectionSink,
        )
        .expect("fixture production owner opens");
        let generation = owner
            .build_and_publish(request, &ActiveControl)
            .expect("fixture generation publishes");
        let sealed = generation
            .encode_sealed()
            .expect("fixture generation seals");
        let envelope: serde_json::Value =
            serde_json::from_slice(&sealed).expect("fixture sealed envelope decodes");
        let state_digest = ManifestDigest::new(
            envelope["state_digest"]
                .as_str()
                .expect("fixture sealed state digest"),
        )
        .expect("fixture state digest is canonical");
        SealedSourceFixture {
            sealed,
            state_digest,
        }
    }

    fn one_page_expectations(fixture: &SealedSourceFixture) -> Vec<OnePageExpectation> {
        let mut source = fixture.open();
        let mut expectations = Vec::new();
        loop {
            match source
                .next_page(&ActiveControl)
                .expect("fixture one-page read")
            {
                VerifiedSealedLexicalPageReadV1::Page(page) => {
                    expectations.push(expectation(&page));
                }
                VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                    receipt
                        .verify_completion(Some(source.cursor()))
                        .expect("fixture one-page receipt verifies");
                    return expectations;
                }
            }
        }
    }

    fn expectation(page: &VerifiedSealedLexicalPageV1) -> OnePageExpectation {
        OnePageExpectation {
            page_ordinal: page.page_ordinal(),
            chunk_count: page.chunk_count(),
            payload_bytes: page.payload_bytes(),
            import_count: page.import_count(),
            import_payload_bytes: page.import_payload_bytes(),
            page_digest: page.page_digest().as_str().to_owned(),
            next_cursor: page
                .next_cursor()
                .persisted_bytes()
                .expect("one-page cursor persists"),
            retained_owned_bytes: page.retained_owned_bytes(),
        }
    }

    fn assert_page_matches(page: &VerifiedSealedLexicalPageV1, expected: &OnePageExpectation) {
        assert_eq!(page.page_ordinal(), expected.page_ordinal);
        assert_eq!(page.chunk_count(), expected.chunk_count);
        assert_eq!(page.payload_bytes(), expected.payload_bytes);
        assert_eq!(page.import_count(), expected.import_count);
        assert_eq!(page.import_payload_bytes(), expected.import_payload_bytes);
        assert_eq!(page.page_digest().as_str(), expected.page_digest.as_str());
        assert_eq!(
            page.next_cursor()
                .persisted_bytes()
                .expect("batch page cursor persists"),
            expected.next_cursor,
        );
    }

    fn bounds_for(expected: &[OnePageExpectation]) -> VerifiedSealedLexicalPageBatchBoundsV1 {
        let page_slots = std::mem::size_of::<VerifiedSealedLexicalPageV1>()
            .checked_mul(expected.len())
            .expect("fixture page-slot bytes do not overflow");
        let retained_bytes = expected.iter().fold(page_slots, |bytes, page| {
            bytes
                .checked_add(page.retained_owned_bytes)
                .expect("fixture retained bytes do not overflow")
        });
        VerifiedSealedLexicalPageBatchBoundsV1::new(expected.len(), retained_bytes)
            .expect("fixture batch bounds are retainable")
    }

    fn pages(read: VerifiedSealedLexicalPageBatchReadV1) -> Vec<VerifiedSealedLexicalPageV1> {
        match read {
            VerifiedSealedLexicalPageBatchReadV1::Pages(pages) => pages,
            VerifiedSealedLexicalPageBatchReadV1::Complete(_) => {
                panic!("fixture must stage lexical pages")
            }
        }
    }

    #[test]
    fn batch_bounds_refuse_limits_that_cannot_retain_a_bounded_page_batch() {
        let page_slot_bytes = std::mem::size_of::<VerifiedSealedLexicalPageV1>();
        for (maximum_pages, maximum_retained_bytes) in
            [(0, 1), (1, 0), (1, page_slot_bytes.saturating_sub(1))]
        {
            let error =
                VerifiedSealedLexicalPageBatchBoundsV1::new(maximum_pages, maximum_retained_bytes)
                    .expect_err("an unbounded or unretainable batch must be refused");
            assert!(matches!(error, CodeIndexProductionErrorV1::Contract(_)));
        }
    }

    #[test]
    fn rejected_batch_keeps_the_exact_cursor_and_retries_the_first_one_page_value() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        assert!(
            expected.len() >= 2,
            "fixture must provide a multi-page source"
        );
        let mut source = fixture.open();
        let cursor_before = source
            .cursor()
            .persisted_bytes()
            .expect("initial cursor persists");
        let rejected = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
                assert_eq!(pages.len(), 2, "fixture stages a full two-page batch");
                Err::<NonZeroUsize, _>("builder rejects the complete batch")
            })
            .expect("source stages the rejected batch");
        assert_eq!(
            rejected.expect_err("callback refusal must be surfaced"),
            "builder rejects the complete batch"
        );
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("rejected cursor persists"),
            cursor_before,
        );

        let retried = match source.next_page(&ActiveControl).expect("one-page retry") {
            VerifiedSealedLexicalPageReadV1::Page(page) => page,
            VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain pages"),
        };
        assert_page_matches(&retried, &expected[0]);
    }

    #[test]
    fn out_of_range_accepted_prefix_keeps_the_exact_cursor_and_retries_the_first_page() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        assert!(
            expected.len() >= 2,
            "fixture must provide a multi-page source"
        );
        let mut source = fixture.open();
        let cursor_before = source
            .cursor()
            .persisted_bytes()
            .expect("initial cursor persists");
        let error = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
                assert_eq!(pages.len(), 2, "fixture stages a full two-page batch");
                Ok::<_, ()>(
                    NonZeroUsize::new(pages.len() + 1)
                        .expect("out-of-range accepted prefix remains non-zero"),
                )
            })
            .expect_err("out-of-range accepted prefix must be refused");
        assert!(matches!(error, CodeIndexProductionErrorV1::Contract(_)));
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("rejected cursor persists"),
            cursor_before,
        );

        let retried = match source.next_page(&ActiveControl).expect("one-page retry") {
            VerifiedSealedLexicalPageReadV1::Page(page) => page,
            VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain pages"),
        };
        assert_page_matches(&retried, &expected[0]);
    }

    #[test]
    fn count_bound_returns_the_first_two_one_page_values_in_order() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        assert!(
            expected.len() >= 2,
            "fixture must provide a multi-page source"
        );
        let mut source = fixture.open();
        let batch = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
                assert_eq!(pages.len(), 2);
                Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
            })
            .expect("source stages a count-bounded batch")
            .expect("callback accepts the count-bounded batch");
        let batch = pages(batch);
        assert_eq!(batch.len(), 2);
        assert_page_matches(&batch[0], &expected[0]);
        assert_page_matches(&batch[1], &expected[1]);
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("batch cursor persists"),
            expected[1].next_cursor,
        );
    }

    #[test]
    fn accepts_only_fifteen_of_sixteen_staged_parser_backed_pages() {
        let source_text = (0..16)
            .map(|index| format!("pub fn batch_prefix_page_{index}() -> usize {{ {index} }}\n"))
            .collect::<String>();
        let fixture = fixture_for_source(&source_text);
        let expected = one_page_expectations(&fixture);
        assert!(
            expected.len() >= 16,
            "parser-backed fixture must expose sixteen one-page values"
        );
        let mut source = fixture.open();
        let accepted = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected[..16]), |pages| {
                assert_eq!(
                    pages.len(),
                    16,
                    "fixture stages sixteen parser-backed pages"
                );
                Ok::<_, ()>(NonZeroUsize::new(15).expect("fifteen is non-zero"))
            })
            .expect("source stages the parser-backed batch")
            .expect("callback accepts a fifteen-page prefix");
        let accepted = pages(accepted);
        assert_eq!(accepted.len(), 15);
        for (page, expected) in accepted.iter().zip(&expected[..15]) {
            assert_page_matches(page, expected);
        }
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("accepted-prefix cursor persists"),
            expected[14].next_cursor,
        );

        let next = match source
            .next_page(&ActiveControl)
            .expect("read the first unaccepted page")
        {
            VerifiedSealedLexicalPageReadV1::Page(page) => page,
            VerifiedSealedLexicalPageReadV1::Complete(_) => {
                panic!("the sixteenth staged page must remain available")
            }
        };
        assert_page_matches(&next, &expected[15]);
    }

    #[test]
    fn retained_byte_bound_stops_before_the_next_larger_one_page_value() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        let (start, first, second) = expected
            .windows(2)
            .enumerate()
            .find_map(|(index, pair)| {
                (pair[0].retained_owned_bytes < pair[1].retained_owned_bytes)
                    .then_some((index, &pair[0], &pair[1]))
            })
            .expect("fixture has an increasing one-page retained-byte boundary");
        let mut source = fixture.open();
        for _ in 0..start {
            let _ = source
                .next_page(&ActiveControl)
                .expect("advance to retained boundary");
        }
        let page_slots = std::mem::size_of::<VerifiedSealedLexicalPageV1>()
            .checked_mul(2)
            .expect("fixture page-slot bytes do not overflow");
        let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(
            2,
            page_slots
                .checked_add(first.retained_owned_bytes)
                .expect("fixture retained bound does not overflow"),
        )
        .expect("first page fits the retained-byte bound");
        let batch = source
            .next_page_batch_if(&ActiveControl, bounds, |pages| {
                assert_eq!(pages.len(), 1, "larger next page must stay unstaged");
                Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
            })
            .expect("source stages the retained-byte-bounded batch")
            .expect("callback accepts the retained-byte-bounded batch");
        let batch = pages(batch);
        assert_eq!(batch.len(), 1);
        assert_page_matches(&batch[0], first);
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("retained-byte cursor persists"),
            first.next_cursor,
        );

        let next = match source
            .next_page(&ActiveControl)
            .expect("read byte-stopped page")
        {
            VerifiedSealedLexicalPageReadV1::Page(page) => page,
            VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain next page"),
        };
        assert_page_matches(&next, second);
    }

    #[test]
    fn completion_follows_the_last_accepted_batch_without_an_empty_callback() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        assert!(!expected.is_empty(), "fixture must provide lexical pages");
        let mut source = fixture.open();
        let accepted = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected), |pages| {
                assert_eq!(pages.len(), expected.len());
                Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
            })
            .expect("source stages the final batch")
            .expect("callback accepts the final batch");
        let accepted = pages(accepted);
        assert_eq!(accepted.len(), expected.len());
        for (page, expected) in accepted.iter().zip(&expected) {
            assert_page_matches(page, expected);
        }

        let mut callback_called = false;
        let complete = source
            .next_page_batch_if(&ActiveControl, bounds_for(&expected), |_| {
                callback_called = true;
                Ok::<_, ()>(NonZeroUsize::MIN)
            })
            .expect("completed source stays readable")
            .expect("completion has no callback error");
        let VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) = complete else {
            panic!("completion follows the last accepted batch")
        };
        assert!(
            !callback_called,
            "completion must not invoke an empty callback"
        );
        receipt
            .verify_completion(Some(source.cursor()))
            .expect("completed receipt matches accepted cursor");
    }

    #[test]
    fn cancellation_during_staging_keeps_the_exact_pre_batch_cursor() {
        let fixture = fixture();
        let expected = one_page_expectations(&fixture);
        assert!(
            expected.len() >= 2,
            "fixture must provide a multi-page source"
        );
        let mut source = fixture.open();
        let cursor_before = source
            .cursor()
            .persisted_bytes()
            .expect("initial cursor persists");
        let control = CancelDuringStaging::new();
        let mut callback_called = false;
        let error = source
            .next_page_batch_if(&control, bounds_for(&expected[..2]), |_| {
                callback_called = true;
                Ok::<_, ()>(NonZeroUsize::MIN)
            })
            .expect_err("cancellation must interrupt batch staging");
        assert!(matches!(
            error,
            CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
        ));
        assert!(
            control.checks.load(Ordering::Acquire) > 1,
            "cancellation must be checked during source staging"
        );
        assert!(
            !callback_called,
            "cancelled staging must not invoke the callback"
        );
        assert_eq!(
            source
                .cursor()
                .persisted_bytes()
                .expect("cancelled cursor persists"),
            cursor_before,
        );
    }
}
