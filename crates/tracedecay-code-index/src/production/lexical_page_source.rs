use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

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

/// Resume position after one fully admitted lexical page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSealedLexicalCursorV1 {
    next_file_ordinal: u64,
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
    pub fn persisted_bytes(&self) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        serde_json::to_vec(&(
            self.next_file_ordinal,
            self.next_chunk_ordinal,
            self.next_page_ordinal,
            self.emitted_chunks,
            self.emitted_payload_bytes,
            self.next_import_ordinal,
            self.emitted_imports,
            self.emitted_import_payload_bytes,
            self.import_dictionary_digest.as_str(),
            self.cumulative_digest.as_str(),
        ))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }

    pub fn restore_persisted(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        let (
            next_file_ordinal,
            next_chunk_ordinal,
            next_page_ordinal,
            emitted_chunks,
            emitted_payload_bytes,
            next_import_ordinal,
            emitted_imports,
            emitted_import_payload_bytes,
            import_dictionary_digest,
            cumulative_digest,
        ): (u64, u64, u64, u64, u64, u64, u64, u64, String, String) = serde_json::from_slice(bytes)
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "persisted sealed lexical cursor is invalid: {error}"
                ))
            })?;
        Ok(Self {
            next_file_ordinal,
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
        })
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
            && self.import_dictionary_digest == digest_hasher(import_dictionary_hasher())?
            && self.cumulative_digest == digest_hasher(source_hasher())?)
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
    cumulative_hasher_before: Sha256,
    import_dictionary_hasher_before: Sha256,
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
    /// The hidden pre-page hash states are minted only by the verified sealed
    /// source. Binding them back to `previous` preserves the page-boundary-
    /// independent source and import dictionary identities while still making
    /// every builder admission independently fail closed.
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
        if digest_hasher(self.cumulative_hasher_before.clone())?
            != self.previous_cursor.cumulative_digest
            || digest_hasher(self.import_dictionary_hasher_before.clone())?
                != self.previous_cursor.import_dictionary_digest
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page hash state does not match its prior cursor".to_owned(),
            ));
        }

        let mut page_hasher = page_hasher(self.page_ordinal);
        let mut cumulative_hasher = self.cumulative_hasher_before.clone();
        let mut import_dictionary_hasher = self.import_dictionary_hasher_before.clone();
        let mut payload_bytes = 0u64;
        for admitted in &self.chunks {
            let serialized = serde_json::to_vec(admitted.chunk()).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical chunk serialization failed: {error}"
                ))
            })?;
            hash_record(&mut page_hasher, &serialized)?;
            hash_record(&mut cumulative_hasher, &serialized)?;
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
            hash_import_record(&mut cumulative_hasher, &serialized)?;
            hash_record(&mut import_dictionary_hasher, &serialized)?;
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
        let expected_cumulative = digest_hasher(cumulative_hasher)?;
        let expected_import_dictionary = digest_hasher(import_dictionary_hasher)?;
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

    /// Heap bytes retained by this page's chunk and import vectors.
    ///
    /// Vector and `String` storage uses actual capacities. Immutable typed-ID,
    /// exact-term, and sanitized-text payloads expose lengths rather than
    /// capacities, so their byte-exact payload size is counted once alongside
    /// the owning vector slots. Allocator metadata and fixed inline fields are
    /// deliberately excluded.
    pub fn retained_owned_bytes(&self) -> usize {
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
            chunk_bytes.saturating_add(
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
                && self.import_dictionary_digest == digest_hasher(import_dictionary_hasher())?
                && self.cumulative_digest == digest_hasher(source_hasher())?
            {
                return Ok(());
            }
            return Err(CodeIndexProductionErrorV1::Contract(
                "nonempty sealed lexical receipt has no final cursor".to_owned(),
            ));
        };
        if self.page_count != cursor.next_page_ordinal
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
pub enum VerifiedSealedLexicalPageReadV1 {
    Page(VerifiedSealedLexicalPageV1),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

/// Seekable, bounded lexical projection source over a verified v5/v6 seal.
///
/// Opening performs a streaming structural scan and verifies the exact raw
/// generation digest. It retains only file byte ranges. Each read decodes and
/// exact-admits one file at a time, then emits at most the configured chunk and
/// serialized-payload bounds. Raw sealed bytes never cross this interface.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageSourceV1<R> {
    reader: R,
    file_ranges: Vec<Range<u64>>,
    source_state_digest: ManifestDigest,
    format_revision: u32,
    maximum_page_chunks: usize,
    maximum_page_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    cumulative_hasher: Sha256,
    import_dictionary_hasher: Sha256,
}

impl<R: Read + Seek> VerifiedSealedLexicalPageSourceV1<R> {
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
        let layout = scan_layout(&mut reader, admitted_len, control)?;
        if layout.state_digest != expected_state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match the admitted source".to_owned(),
            ));
        }
        let cumulative_hasher = source_hasher();
        let import_dictionary_hasher = import_dictionary_hasher();
        let cursor = VerifiedSealedLexicalCursorV1 {
            next_file_ordinal: 0,
            next_chunk_ordinal: 0,
            next_import_ordinal: 0,
            next_page_ordinal: 0,
            emitted_chunks: 0,
            emitted_payload_bytes: 0,
            emitted_imports: 0,
            emitted_import_payload_bytes: 0,
            import_dictionary_digest: digest_hasher(import_dictionary_hasher.clone())?,
            cumulative_digest: digest_hasher(cumulative_hasher.clone())?,
        };
        Ok(Self {
            reader,
            file_ranges: layout.file_ranges,
            source_state_digest: layout.state_digest,
            format_revision: layout.format_revision,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            cumulative_hasher,
            import_dictionary_hasher,
        })
    }

    pub fn cursor(&self) -> &VerifiedSealedLexicalCursorV1 {
        &self.cursor
    }

    pub fn next_page(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedSealedLexicalPageReadV1, CodeIndexProductionErrorV1> {
        checkpoint(control)?;
        let mut cursor = self.cursor.clone();
        let mut cumulative_hasher = self.cumulative_hasher.clone();
        let mut import_dictionary_hasher = self.import_dictionary_hasher.clone();
        let mut page_hasher = page_hasher(cursor.next_page_ordinal);
        let mut chunks = Vec::new();
        let mut page_bytes = 0usize;
        let mut imports = Vec::new();
        let mut import_bytes = 0usize;

        while usize::try_from(cursor.next_file_ordinal)
            .ok()
            .is_some_and(|ordinal| ordinal < self.file_ranges.len())
        {
            checkpoint(control)?;
            let file_ordinal = usize::try_from(cursor.next_file_ordinal).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file cursor exceeds the platform limit".to_owned(),
                )
            })?;
            let admitted = self.read_admitted_file(file_ordinal)?;
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
                        chunks,
                        page_bytes,
                        imports,
                        import_bytes,
                        cursor,
                        page_hasher,
                        cumulative_hasher,
                        import_dictionary_hasher,
                    );
                }
                hash_record(&mut page_hasher, &serialized)?;
                hash_record(&mut cumulative_hasher, &serialized)?;
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
                        chunks,
                        page_bytes,
                        imports,
                        import_bytes,
                        cursor,
                        page_hasher,
                        cumulative_hasher,
                        import_dictionary_hasher,
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
                        chunks,
                        page_bytes,
                        imports,
                        import_bytes,
                        cursor,
                        page_hasher,
                        cumulative_hasher,
                        import_dictionary_hasher,
                    );
                }
                hash_import_record(&mut page_hasher, &serialized)?;
                hash_import_record(&mut cumulative_hasher, &serialized)?;
                hash_record(&mut import_dictionary_hasher, &serialized)?;
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
            cursor.next_file_ordinal =
                cursor.next_file_ordinal.checked_add(1).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file ordinal overflowed".to_owned(),
                    )
                })?;
            cursor.next_chunk_ordinal = 0;
            cursor.next_import_ordinal = 0;
        }

        if !chunks.is_empty() || !imports.is_empty() {
            return self.commit_page(
                chunks,
                page_bytes,
                imports,
                import_bytes,
                cursor,
                page_hasher,
                cumulative_hasher,
                import_dictionary_hasher,
            );
        }
        Ok(VerifiedSealedLexicalPageReadV1::Complete(
            VerifiedSealedLexicalSourceReceiptV1 {
                source_state_digest: self.source_state_digest.clone(),
                format_revision: self.format_revision,
                page_count: self.cursor.next_page_ordinal,
                total_chunks: self.cursor.emitted_chunks,
                total_payload_bytes: self.cursor.emitted_payload_bytes,
                total_imports: self.cursor.emitted_imports,
                import_payload_bytes: self.cursor.emitted_import_payload_bytes,
                import_dictionary_digest: self.cursor.import_dictionary_digest.clone(),
                cumulative_digest: self.cursor.cumulative_digest.clone(),
            },
        ))
    }

    fn read_admitted_file(
        &mut self,
        file_ordinal: usize,
    ) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
        let range = self.file_ranges.get(file_ordinal).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical file cursor is outside the admitted source".to_owned(),
            )
        })?;
        let byte_len = range.end.checked_sub(range.start).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical file range is not canonical".to_owned(),
            )
        })?;
        let byte_len = usize::try_from(byte_len).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical file range exceeds the platform limit".to_owned(),
            )
        })?;
        if byte_len > self.maximum_page_bytes {
            return Err(CodeIndexProductionErrorV1::Contract(
                "one sealed lexical file exceeds the bounded decode window".to_owned(),
            ));
        }
        let mut bytes = vec![0; byte_len];
        self.reader
            .seek(SeekFrom::Start(range.start))
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical source seek failed: {error}"
                ))
            })?;
        self.reader.read_exact(&mut bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed lexical file read failed: {error}"
            ))
        })?;
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
        Ok(AdmittedSealedLexicalFileV1 { chunks, imports })
    }

    fn commit_page(
        &mut self,
        chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
        page_bytes: usize,
        imports: Vec<CodeIndexImportEvidenceV1>,
        import_bytes: usize,
        mut cursor: VerifiedSealedLexicalCursorV1,
        mut page_hasher: Sha256,
        cumulative_hasher: Sha256,
        import_dictionary_hasher: Sha256,
    ) -> Result<VerifiedSealedLexicalPageReadV1, CodeIndexProductionErrorV1> {
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
        cursor.import_dictionary_digest = digest_hasher(import_dictionary_hasher.clone())?;
        cursor.cumulative_digest = digest_hasher(cumulative_hasher.clone())?;
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
            previous_cursor: self.cursor.clone(),
            cumulative_hasher_before: self.cumulative_hasher.clone(),
            import_dictionary_hasher_before: self.import_dictionary_hasher.clone(),
        };
        self.cursor = cursor;
        self.cumulative_hasher = cumulative_hasher;
        self.import_dictionary_hasher = import_dictionary_hasher;
        Ok(VerifiedSealedLexicalPageReadV1::Page(page))
    }
}

struct AdmittedSealedLexicalFileV1 {
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
}

struct SealedLexicalLayoutV1 {
    state_digest: ManifestDigest,
    format_revision: u32,
    file_ranges: Vec<Range<u64>>,
}

fn scan_layout<R: Read + Seek>(
    reader: &mut R,
    admitted_len: u64,
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
        for byte in &buffer[..read] {
            if observed < admitted_len {
                scanner.observe(*byte, observed)?;
            }
            observed = observed.checked_add(1).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical source length overflowed".to_owned(),
                )
            })?;
        }
        remaining -= u64::try_from(read).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical source read exceeds u64".to_owned(),
            )
        })?;
    }
    if observed != admitted_len {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation length does not match its admitted length".to_owned(),
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
    file_ranges: Vec<Range<u64>>,
}

impl LayoutScanner {
    fn observe(&mut self, byte: u8, offset: u64) -> Result<(), CodeIndexProductionErrorV1> {
        if let Some(hasher) = self.generation_hasher.as_mut() {
            hasher.update([byte]);
        }
        if self.in_string {
            if self.escaped {
                self.escaped = false;
                if self.string.len() < 128 {
                    self.string.push(byte);
                } else {
                    self.string_overflowed = true;
                }
                return Ok(());
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
            return Ok(());
        }

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
                    let mut hasher = Sha256::new();
                    hasher.update([byte]);
                    self.generation_hasher = Some(hasher);
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
                if let Some(start) = self.current_file_start {
                    if self
                        .generation_depth
                        .is_some_and(|depth| self.brace_depth == depth + 1)
                    {
                        self.file_ranges.push(start..offset + 1);
                        self.current_file_start = None;
                    }
                }
                if self.generation_depth == Some(self.brace_depth) {
                    let hasher = self.generation_hasher.take().ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed generation digest state is missing".to_owned(),
                        )
                    })?;
                    self.generation_digest = Some(digest_hasher(hasher)?);
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
        Ok(())
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
        Ok(SealedLexicalLayoutV1 {
            state_digest,
            format_revision,
            file_ranges: self.file_ranges,
        })
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

fn source_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_DIGEST_DOMAIN);
    hasher
}

fn import_dictionary_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(IMPORT_DICTIONARY_DIGEST_DOMAIN);
    hasher
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
    ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}
