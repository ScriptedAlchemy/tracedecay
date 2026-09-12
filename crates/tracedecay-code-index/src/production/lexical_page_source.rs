use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
    sync::Arc,
};

use sha2::{Digest, Sha256};
use tracedecay_domain::{CodeGenerationSourceCommitmentsV1, ExactTechnicalTermV1};

use crate::{capabilities::expected_seal_digest, intake::INTAKE_DIGEST_SEPARATOR};

use super::partitioned_codec::PartitionedLexicalFileSourceV1;
use super::sealed_codec::{
    MINIMUM_SEALED_GENERATION_FORMAT_REVISION, MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION,
    PersistedFileGenerationArtifactsV1, superseded_sealed_generation_revision,
};
use super::{FileGenerationArtifactsV1, *};

const PAGE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-page.v1\0";
const SOURCE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source.v1\0";
const IMPORT_DICTIONARY_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-import-dictionary.v1\0";
const IMPORT_RECORD_DOMAIN: &[u8] = b"import\0";
const SOURCE_CHAIN_RECORD_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source-chain.v1\0";
const SYMBOL_DISPLAY_RECORD_DOMAIN: &[u8] = b"symbol-display\0";
const SOURCE_SYMBOL_DISPLAY_CHAIN_RECORD_DOMAIN: &[u8] =
    b"tracedecay.sealed-lexical-symbol-display-chain.v1\0";
const IMPORT_DICTIONARY_CHAIN_RECORD_DOMAIN: &[u8] =
    b"tracedecay.sealed-lexical-import-dictionary-chain.v1\0";
const CURSOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-cursor.v1\0";
const INVALID_CURSOR_POSITION_DETAIL: &str =
    "sealed lexical cursor is not a valid position in its next file";
const LAYOUT_PROGRESS_INTERVAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LEXICAL_GENERATION_METADATA_BYTES: u64 = 64 * 1024 * 1024;
/// Concurrent exact-read/decode window. Same 64 MiB retain cap as one
/// lexical page batch, so prefetch cannot exceed a window the builder
/// already admits for staged pages.
/// Bound on the sealed file bytes read ahead of one decode window: the
/// lexical source's admitted-file prefetch and the partitioned decoder's
/// segment window share it so neither holds more than this in raw segment
/// bytes while the pool decodes them.
pub(super) const LEXICAL_FILE_PREFETCH_BYTES_V1: u64 = 64 * 1024 * 1024;

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

/// Classified failure while restoring an authenticated lexical cursor.
///
/// Only a position that cannot exist in the authenticated next file is
/// staging incompatibility. All other failures retain their production error
/// so callers cannot discard staging for an unrelated contract or authority
/// failure.
#[derive(Debug)]
pub enum VerifiedSealedLexicalCursorRestoreErrorV1 {
    IncompatiblePosition,
    Production(CodeIndexProductionErrorV1),
}

impl VerifiedSealedLexicalCursorRestoreErrorV1 {
    fn into_production_error(self) -> CodeIndexProductionErrorV1 {
        match self {
            Self::IncompatiblePosition => {
                CodeIndexProductionErrorV1::Contract(INVALID_CURSOR_POSITION_DETAIL.to_owned())
            }
            Self::Production(error) => error,
        }
    }
}

impl From<CodeIndexProductionErrorV1> for VerifiedSealedLexicalCursorRestoreErrorV1 {
    fn from(error: CodeIndexProductionErrorV1) -> Self {
        Self::Production(error)
    }
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

/// Compact parser-attested display identity for one symbol-backed chunk.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct VerifiedSealedLexicalSymbolDisplayV1 {
    occurrence: SymbolOccurrenceId,
    simple_name: String,
    qualified_name: String,
    kind: String,
}

impl VerifiedSealedLexicalSymbolDisplayV1 {
    pub fn occurrence(&self) -> &SymbolOccurrenceId {
        &self.occurrence
    }

    pub fn simple_name(&self) -> &str {
        &self.simple_name
    }

    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn retained_owned_bytes(&self) -> usize {
        self.occurrence
            .as_str()
            .len()
            .saturating_add(self.simple_name.capacity())
            .saturating_add(self.qualified_name.capacity())
            .saturating_add(self.kind.capacity())
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
    symbol_displays: Vec<Option<VerifiedSealedLexicalSymbolDisplayV1>>,
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

    pub fn symbol_displays(&self) -> &[Option<VerifiedSealedLexicalSymbolDisplayV1>] {
        &self.symbol_displays
    }

    pub fn symbol_display_capacity(&self) -> usize {
        self.symbol_displays.capacity()
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
        if self.symbol_displays.len() != self.chunks.len() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical symbol-display cardinality does not match its chunks".to_owned(),
            ));
        }
        for (admitted, display) in self.chunks.iter().zip(&self.symbol_displays) {
            let serialized = serde_json::to_vec(admitted.chunk()).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical chunk serialization failed: {error}"
                ))
            })?;
            hash_record(&mut page_hasher, &serialized)?;
            cumulative_digest =
                advance_digest(&cumulative_digest, SOURCE_CHAIN_RECORD_DOMAIN, &serialized)?;
            match (&admitted.chunk().anchor.symbol_occurrence_id, display) {
                (Some(occurrence), Some(display)) if occurrence == display.occurrence() => {
                    let serialized_display = serde_json::to_vec(display).map_err(|error| {
                        CodeIndexProductionErrorV1::Contract(format!(
                            "sealed lexical symbol display serialization failed: {error}"
                        ))
                    })?;
                    hash_symbol_display_record(&mut page_hasher, &serialized_display)?;
                    cumulative_digest = advance_digest(
                        &cumulative_digest,
                        SOURCE_SYMBOL_DISPLAY_CHAIN_RECORD_DOMAIN,
                        &serialized_display,
                    )?;
                }
                (None, None) => {}
                _ => {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed lexical symbol display does not match its chunk anchor".to_owned(),
                    ));
                }
            }
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
        let symbol_display_bytes = self.symbol_displays.iter().fold(
            self.symbol_displays
                .capacity()
                .saturating_mul(std::mem::size_of::<
                    Option<VerifiedSealedLexicalSymbolDisplayV1>,
                >()),
            |bytes, display| {
                bytes.saturating_add(display.as_ref().map_or(
                    0,
                    VerifiedSealedLexicalSymbolDisplayV1::retained_owned_bytes,
                ))
            },
        );
        self.imports.iter().fold(
            chunk_bytes
                .saturating_add(symbol_display_bytes)
                .saturating_add(digest_bytes)
                .saturating_add(
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
    symbol_displays: Vec<Option<VerifiedSealedLexicalSymbolDisplayV1>>,
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
    Complete {
        receipt: VerifiedSealedLexicalSourceReceiptV1,
        cursor: VerifiedSealedLexicalCursorV1,
    },
}

#[allow(clippy::large_enum_variant)] // staged pages dominate this private batch result
enum StagedSealedLexicalPageBatchReadV1 {
    Pages(Vec<VerifiedSealedLexicalPageV1>),
    Complete {
        receipt: VerifiedSealedLexicalSourceReceiptV1,
        cursor: VerifiedSealedLexicalCursorV1,
    },
}

/// Seekable, bounded lexical projection source over a verified v5/v6 seal.
///
/// Opening performs a streaming structural scan and verifies the exact raw
/// generation digest. Layout records every file byte range so source_scan
/// can exact-read and decode on the indexing pool instead of walking the
/// files array a second time one byte at a time. Page minting stays serial
/// because the cumulative digest is a chain. Raw sealed bytes never cross
/// this interface.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageSourceV1<R> {
    reader: R,
    file_count: u64,
    first_file_offset: u64,
    files_end_offset: u64,
    file_ranges: Vec<(u64, u64)>,
    partitioned_lexical_byte_offsets: Option<Vec<u64>>,
    total_lexical_units: u64,
    maximum_file_bytes: u64,
    source_state_digest: ManifestDigest,
    format_revision: u32,
    metadata: VerifiedSealedTextGenerationMetadataV1,
    maximum_page_chunks: usize,
    maximum_page_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    admitted_window: BTreeMap<u64, Arc<AdmittedSealedLexicalFileV1>>,
    /// Durable partitioned descriptors or same-process published file authority.
    /// Partitioned sources load only the next bounded admission window; their
    /// cursors retain stable file ordinals across process restarts.
    file_source: Option<SealedLexicalFilesV1>,
}

#[derive(Debug)]
pub(super) enum SealedLexicalFilesV1 {
    Published(Vec<Arc<FileGenerationArtifactsV1>>),
    Partitioned(PartitionedLexicalFileSourceV1),
}

/// Authenticated generation metadata needed by exact and lexical serving.
///
/// The full sealed generation can be gigabytes. This projection retains only
/// the manifest and sanitized snapshot header that precede the files array;
/// the layout scan authenticates the complete content-addressed seal before
/// this value becomes observable.
#[derive(Clone, Debug)]
pub struct VerifiedSealedTextGenerationMetadataV1 {
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    statistics: Option<CodeIndexGenerationStatisticsV1>,
}

impl VerifiedSealedTextGenerationMetadataV1 {
    pub fn from_published_generation(generation: &CodeIndexPublishedGenerationV1) -> Self {
        Self {
            manifest: generation.manifest().clone(),
            snapshot: generation.snapshot().clone(),
            statistics: Some(generation.statistics.clone()),
        }
    }

    pub(super) fn from_partitioned_manifest(
        manifest: CodeGenerationManifestV1,
        snapshot: SanitizedCodeSnapshotV1,
        statistics: Option<CodeIndexGenerationStatisticsV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if manifest.source_commitments.is_none() {
            return Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable);
        }
        snapshot
            .validate()
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let snapshot_digest = canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &snapshot))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if snapshot_digest != manifest.snapshot_digest
            || expected_seal_digest(&manifest)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?
                != manifest.seal.expected_digest
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "partitioned sealed text metadata does not verify".to_owned(),
            ));
        }
        Ok(Self {
            manifest,
            snapshot,
            statistics,
        })
    }

    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.manifest
    }

    /// Compare every owner-controlled input represented by the bounded
    /// manifest and snapshot. Chunk policy census still requires the full
    /// generation's chunk corpus.
    pub fn manifest_compatibility_with(
        &self,
        config: &CodeIndexProductionConfigV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        CodeIndexGenerationCompatibilityV1::for_metadata(&self.manifest, &self.snapshot, config)
    }

    pub fn source_commitments(
        &self,
    ) -> Result<&CodeGenerationSourceCommitmentsV1, CodeIndexProductionErrorV1> {
        self.manifest
            .source_commitments
            .as_ref()
            .ok_or(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable)
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.snapshot
    }

    pub fn generation_statistics(&self) -> Option<&CodeIndexGenerationStatisticsV1> {
        self.statistics.as_ref()
    }
}

impl<R: Read + Seek> VerifiedSealedLexicalPageSourceV1<R> {
    // Every argument is a distinct authority the constructor binds together
    // exactly once: the reader, the manifest, the sanitized snapshot, the
    // optional statistics, the partitioned file source, its state digest, and
    // the two page bounds. Grouping any of them into a parameter struct would
    // invent a type with one construction site and hide which authority a
    // caller failed to supply.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate authority bound once at construction"
    )]
    pub(super) fn open_partitioned_parts(
        reader: R,
        manifest: CodeGenerationManifestV1,
        snapshot: SanitizedCodeSnapshotV1,
        statistics: Option<CodeIndexGenerationStatisticsV1>,
        source: PartitionedLexicalFileSourceV1,
        source_state_digest: ManifestDigest,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if maximum_page_chunks == 0 || maximum_page_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page bounds must be non-zero".to_owned(),
            ));
        }
        let metadata = VerifiedSealedTextGenerationMetadataV1::from_partitioned_manifest(
            manifest, snapshot, statistics,
        )?;
        let file_count = u64::try_from(source.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "partitioned sealed generation file count exceeds u64".to_owned(),
            )
        })?;
        let file_ranges = (0..file_count)
            .map(|file| (file, file.saturating_add(1)))
            .collect::<Vec<_>>();
        let partitioned_lexical_byte_offsets = source.lexical_byte_offsets()?;
        let total_lexical_units = partitioned_lexical_byte_offsets
            .last()
            .copied()
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "partitioned lexical byte offsets are empty".to_owned(),
                )
            })?;
        let maximum_file_bytes = source.maximum_file_bytes();
        let cursor = VerifiedSealedLexicalCursorV1::initial(source_state_digest.clone(), 0)?;
        Ok(Self {
            reader,
            file_count,
            first_file_offset: 0,
            files_end_offset: file_count,
            file_ranges,
            partitioned_lexical_byte_offsets: Some(partitioned_lexical_byte_offsets),
            total_lexical_units,
            maximum_file_bytes,
            source_state_digest,
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            metadata,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_window: BTreeMap::new(),
            file_source: Some(SealedLexicalFilesV1::Partitioned(source)),
        })
    }

    #[hotpath::measure(label = "code_index.restore.open")]
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
        let total_lexical_units = layout
            .files_end_offset
            .checked_sub(layout.first_file_offset)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical files array has an invalid byte span".to_owned(),
                )
            })?;
        let metadata = read_verified_text_metadata(&mut reader, &layout, control)?;
        Ok(Self {
            reader,
            file_count: layout.file_count,
            first_file_offset: layout.first_file_offset,
            files_end_offset: layout.files_end_offset,
            file_ranges: layout.file_ranges,
            partitioned_lexical_byte_offsets: None,
            total_lexical_units,
            maximum_file_bytes: layout.maximum_file_bytes,
            source_state_digest: layout.state_digest,
            format_revision: layout.format_revision,
            metadata,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_window: BTreeMap::new(),
            file_source: None,
        })
    }

    /// Open a durable sealed source through its content address.
    ///
    /// Unlike [`Self::open`], whose caller already holds the envelope's inner
    /// state digest, this journey binds the complete file bytes to the digest
    /// in the durable generation index while the same bounded scan discovers
    /// the lexical layout. The caller can therefore pass a `File` directly;
    /// no whole-generation `Vec` is required merely to authenticate it.
    #[hotpath::measure(label = "code_index.restore.open_content_addressed")]
    pub fn open_content_addressed(
        reader: R,
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
        Self::open_content_addressed_with_progress(
            reader,
            admitted_len,
            expected_file_digest,
            maximum_page_chunks,
            maximum_page_bytes,
            control,
            |_, _| {},
        )
    }

    /// Open a content-addressed source while reporting authenticated scan
    /// bytes. The callback is invoked at zero, bounded byte intervals, and
    /// exactly once with the admitted total before metadata is exposed.
    #[hotpath::measure(label = "code_index.restore.open_content_addressed")]
    pub fn open_content_addressed_with_progress<F>(
        mut reader: R,
        admitted_len: u64,
        expected_file_digest: ManifestDigest,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
        mut progress: F,
    ) -> Result<Self, CodeIndexProductionErrorV1>
    where
        F: FnMut(u64, u64),
    {
        if maximum_page_chunks == 0 || maximum_page_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical page bounds must be non-zero".to_owned(),
            ));
        }
        let layout = scan_layout_with_progress(
            &mut reader,
            admitted_len,
            Some(&expected_file_digest),
            control,
            &mut progress,
        )?;
        let cursor = VerifiedSealedLexicalCursorV1::initial(
            layout.state_digest.clone(),
            layout.first_file_offset,
        )?;
        let total_lexical_units = layout
            .files_end_offset
            .checked_sub(layout.first_file_offset)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical files array has an invalid byte span".to_owned(),
                )
            })?;
        let metadata = read_verified_text_metadata(&mut reader, &layout, control)?;
        Ok(Self {
            reader,
            file_count: layout.file_count,
            first_file_offset: layout.first_file_offset,
            files_end_offset: layout.files_end_offset,
            file_ranges: layout.file_ranges,
            partitioned_lexical_byte_offsets: None,
            total_lexical_units,
            maximum_file_bytes: layout.maximum_file_bytes,
            source_state_digest: layout.state_digest,
            format_revision: layout.format_revision,
            metadata,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_window: BTreeMap::new(),
            file_source: None,
        })
    }

    pub fn metadata(&self) -> &VerifiedSealedTextGenerationMetadataV1 {
        &self.metadata
    }

    pub fn format_revision(&self) -> u32 {
        self.format_revision
    }

    /// Admit later pages from an already-decoded published generation.
    ///
    /// The sealed file remains the layout and cursor authority. This only
    /// replaces per-file JSON decode when the in-memory files match the
    /// scanned ranges one-for-one. Partitioned sources validate the supplied
    /// identity but keep their bounded durable reader, avoiding retention of
    /// the complete decoded generation. Mismatches are rejected.
    pub fn attach_published_files(
        &mut self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if generation.manifest() != self.metadata.manifest()
            || generation.snapshot() != self.metadata.snapshot()
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "published generation does not match the authenticated sealed lexical source"
                    .to_owned(),
            ));
        }
        if generation.files.len() != self.file_ranges.len() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "published generation file count does not match the sealed lexical layout"
                    .to_owned(),
            ));
        }
        // Partitioned readers keep bounded durable file authority after the
        // supplied generation's identity has been checked above.
        if matches!(self.file_source, Some(SealedLexicalFilesV1::Partitioned(_))) {
            return Ok(());
        }
        generation.validate()?;
        self.file_source = Some(SealedLexicalFilesV1::Published(generation.files.clone()));
        Ok(())
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
    pub fn restore_cursor(
        &mut self,
        cursor: &VerifiedSealedLexicalCursorV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        self.restore_cursor_classified(cursor, control)
            .map_err(VerifiedSealedLexicalCursorRestoreErrorV1::into_production_error)
    }

    /// Restore a cursor while preserving the one incompatibility that permits
    /// a derived staging artifact to be superseded and rebuilt.
    #[hotpath::measure(label = "code_index.restore.cursor")]
    pub fn restore_cursor_classified(
        &mut self,
        cursor: &VerifiedSealedLexicalCursorV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), VerifiedSealedLexicalCursorRestoreErrorV1> {
        let production = VerifiedSealedLexicalCursorRestoreErrorV1::Production;
        checkpoint(control).map_err(production)?;
        cursor
            .verify_source(&self.source_state_digest)
            .map_err(production)?;
        self.admitted_window.clear();
        if cursor.next_file_ordinal > self.file_count {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical cursor exceeds the admitted file layout".to_owned(),
            )
            .into());
        }
        if cursor.next_file_ordinal == self.file_count {
            if cursor.next_chunk_ordinal != 0
                || cursor.next_import_ordinal != 0
                || cursor.next_file_offset != self.files_end_offset
            {
                return Err(VerifiedSealedLexicalCursorRestoreErrorV1::Production(
                    CodeIndexProductionErrorV1::Contract(
                        "completed sealed lexical cursor has a non-terminal file position"
                            .to_owned(),
                    ),
                ));
            }
        } else {
            if cursor.next_file_offset < self.first_file_offset
                || cursor.next_file_offset >= self.files_end_offset
            {
                return Err(VerifiedSealedLexicalCursorRestoreErrorV1::Production(
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical cursor byte offset is outside the files array".to_owned(),
                    ),
                ));
            }
            self.ensure_admitted_file(cursor.next_file_offset, control)
                .map_err(production)?;
            let admitted = self
                .admitted_arc(cursor.next_file_offset)
                .map_err(production)?;
            let chunk_count = u64::try_from(admitted.chunks.len()).map_err(|_| {
                VerifiedSealedLexicalCursorRestoreErrorV1::Production(
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file chunk count exceeds u64".to_owned(),
                    ),
                )
            })?;
            let import_count = u64::try_from(admitted.imports.len()).map_err(|_| {
                VerifiedSealedLexicalCursorRestoreErrorV1::Production(
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file import count exceeds u64".to_owned(),
                    ),
                )
            })?;
            if cursor.next_chunk_ordinal > chunk_count
                || cursor.next_import_ordinal > import_count
                || (cursor.next_chunk_ordinal < chunk_count && cursor.next_import_ordinal != 0)
            {
                self.admitted_window.clear();
                return Err(VerifiedSealedLexicalCursorRestoreErrorV1::IncompatiblePosition);
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
    pub fn total_lexical_units(&self) -> u64 {
        self.total_lexical_units
    }

    /// Fully completed file records at the durable source cursor.
    pub fn completed_files(&self) -> u64 {
        self.cursor.next_file_ordinal()
    }

    /// Authenticated files-array bytes fully passed by the durable source
    /// cursor. A partially consumed file counts only after its final chunk and
    /// imports are committed, matching `completed_files`.
    pub fn completed_lexical_units(&self) -> Result<u64, CodeIndexProductionErrorV1> {
        if let Some(offsets) = &self.partitioned_lexical_byte_offsets {
            let completed = usize::try_from(self.cursor.next_file_ordinal()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical completed file count exceeds usize".to_owned(),
                )
            })?;
            return offsets.get(completed).copied().ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor exceeds partitioned byte bounds".to_owned(),
                )
            });
        }
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
        self.admitted_window.clear();
        Ok(())
    }

    /// Serialized input bytes in the bounded prefetch window plus the page
    /// payload. This does not include the decoded object heap or identity
    /// expansion, which can exceed the compact partitioned segment bytes.
    pub fn staging_window_bytes(&self) -> usize {
        let maximum_file_bytes = usize::try_from(self.maximum_file_bytes).unwrap_or(usize::MAX);
        let workers = crate::parallelism::indexing_workers().max(1);
        let prefetch_bytes = maximum_file_bytes
            .saturating_mul(workers)
            .min(LEXICAL_FILE_PREFETCH_BYTES_V1 as usize)
            .max(maximum_file_bytes);
        self.retained_layout_bytes()
            .saturating_add(prefetch_bytes)
            .saturating_add(self.maximum_page_bytes)
    }

    /// Halve the record bound used to mint the next page after a downstream
    /// batch authority refuses the current page. The source cursor is not
    /// advanced until the downstream callback accepts a page, so tightening
    /// here safely re-mints only the refused suffix while preserving every
    /// already admitted page and its cumulative authority.
    ///
    /// The bound caps chunks and imports, so import-only pages also shrink.
    /// A one-record page cannot be subdivided and remains a typed refusal.
    pub fn tighten_page_record_bound(&mut self) -> Option<(usize, usize)> {
        let previous = self.maximum_page_chunks;
        if previous <= 1 {
            return None;
        }
        let tightened = (previous / 2).max(1);
        self.maximum_page_chunks = tightened;
        Some((previous, tightened))
    }

    /// Compact retained file positions and partitioned content identities.
    /// These scale with file count; decoded chunks only occupy the admission window.
    pub fn retained_layout_bytes(&self) -> usize {
        let source_bytes = match &self.file_source {
            Some(SealedLexicalFilesV1::Published(files)) => files
                .capacity()
                .saturating_mul(std::mem::size_of::<Arc<FileGenerationArtifactsV1>>()),
            Some(SealedLexicalFilesV1::Partitioned(source)) => source.retained_layout_bytes(),
            None => 0,
        };
        std::mem::size_of::<u64>()
            .saturating_mul(4)
            .saturating_add(
                self.file_ranges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u64, u64)>()),
            )
            .saturating_add(
                self.partitioned_lexical_byte_offsets
                    .as_ref()
                    .map_or(0, |offsets| {
                        offsets
                            .capacity()
                            .saturating_mul(std::mem::size_of::<u64>())
                    }),
            )
            .saturating_add(source_bytes)
    }

    #[hotpath::measure(label = "code_index.restore.page")]
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
            StagedSealedLexicalPageReadV1::Complete { receipt, cursor } => {
                self.cursor = cursor;
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
                        StagedSealedLexicalPageReadV1::Complete { receipt, cursor } => {
                            if pages.is_empty() {
                                completion = Some((receipt, cursor));
                            }
                            break;
                        }
                    }
                }

                Ok(if let Some((receipt, cursor)) = completion {
                    StagedSealedLexicalPageBatchReadV1::Complete { receipt, cursor }
                } else {
                    StagedSealedLexicalPageBatchReadV1::Pages(pages)
                })
            })()
        })?;

        match staged {
            StagedSealedLexicalPageBatchReadV1::Complete { receipt, cursor } => {
                self.cursor = cursor;
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
        let mut symbol_displays = Vec::new();
        let mut symbol_display_bytes = 0usize;
        let mut imports = Vec::new();
        let mut import_bytes = 0usize;

        while cursor.next_file_ordinal < self.file_count {
            checkpoint(control)?;
            self.ensure_admitted_file(cursor.next_file_offset, control)?;
            let admitted = self.admitted_arc(cursor.next_file_offset)?;
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
                let chunk = admitted.chunks[chunk_ordinal].chunk();
                let display = match chunk.anchor.symbol_occurrence_id.as_ref() {
                    Some(occurrence) => Some(
                        admitted
                            .symbol_displays
                            .get(occurrence)
                            .cloned()
                            .ok_or_else(|| {
                                CodeIndexProductionErrorV1::Contract(
                                    "sealed lexical symbol chunk has no parser-attested display identity"
                                        .to_owned(),
                                )
                            })?,
                    ),
                    None => None,
                };
                let serialized_display = admitted.serialized_displays[chunk_ordinal].clone();
                let serialized = admitted.serialized_chunks[chunk_ordinal].clone();
                let next_symbol_display_bytes = symbol_display_bytes
                    .saturating_add(serialized_display.as_ref().map_or(0, Vec::len));
                if serialized
                    .len()
                    .saturating_add(serialized_display.as_ref().map_or(0, Vec::len))
                    > self.maximum_page_bytes
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "one admitted lexical chunk exceeds the page byte bound".to_owned(),
                    ));
                }
                if (!chunks.is_empty() || !imports.is_empty())
                    && (chunks.len() == self.maximum_page_chunks
                        || page_bytes
                            .saturating_add(next_symbol_display_bytes)
                            .saturating_add(import_bytes)
                            .saturating_add(serialized.len())
                            > self.maximum_page_bytes)
                {
                    return self.commit_page(
                        previous_cursor,
                        PendingSealedLexicalPageV1 {
                            chunks,
                            page_bytes,
                            symbol_displays,
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
                if let Some(serialized_display) = serialized_display.as_deref() {
                    hash_symbol_display_record(&mut page_hasher, serialized_display)?;
                    cursor.cumulative_digest = advance_digest(
                        &cursor.cumulative_digest,
                        SOURCE_SYMBOL_DISPLAY_CHAIN_RECORD_DOMAIN,
                        serialized_display,
                    )?;
                }
                page_bytes = page_bytes.checked_add(serialized.len()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical page byte count overflowed".to_owned(),
                    )
                })?;
                symbol_display_bytes = next_symbol_display_bytes;
                chunks.push(admitted.chunks[chunk_ordinal].clone());
                symbol_displays.push(display);
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
                            symbol_displays,
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
                let serialized = admitted.serialized_imports[import_ordinal].clone();
                if serialized.len() > self.maximum_page_bytes {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "one admitted lexical import exceeds the page byte bound".to_owned(),
                    ));
                }
                if (!chunks.is_empty() || !imports.is_empty())
                    && (chunks.len().saturating_add(imports.len()) >= self.maximum_page_chunks
                        || page_bytes
                            .saturating_add(symbol_display_bytes)
                            .saturating_add(import_bytes)
                            .saturating_add(serialized.len())
                            > self.maximum_page_bytes)
                {
                    return self.commit_page(
                        previous_cursor,
                        PendingSealedLexicalPageV1 {
                            chunks,
                            page_bytes,
                            symbol_displays,
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
            self.admitted_window.remove(&cursor.next_file_offset);
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
                        symbol_displays,
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
                    symbol_displays,
                    imports,
                    import_bytes,
                    cursor,
                    page_hasher,
                },
            );
        }
        Ok(StagedSealedLexicalPageReadV1::Complete {
            receipt: VerifiedSealedLexicalSourceReceiptV1 {
                source_state_digest: self.source_state_digest.clone(),
                format_revision: self.format_revision,
                page_count: cursor.next_page_ordinal,
                total_chunks: cursor.emitted_chunks,
                total_payload_bytes: cursor.emitted_payload_bytes,
                total_imports: cursor.emitted_imports,
                import_payload_bytes: cursor.emitted_import_payload_bytes,
                import_dictionary_digest: cursor.import_dictionary_digest.clone(),
                cumulative_digest: cursor.cumulative_digest.clone(),
            },
            cursor,
        })
    }

    fn admitted_arc(
        &self,
        file_offset: u64,
    ) -> Result<Arc<AdmittedSealedLexicalFileV1>, CodeIndexProductionErrorV1> {
        self.admitted_window
            .get(&file_offset)
            .cloned()
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical admitted-file cache is missing".to_owned(),
                )
            })
    }

    fn file_range_index(&self, file_offset: u64) -> Result<usize, CodeIndexProductionErrorV1> {
        self.file_ranges
            .binary_search_by_key(&file_offset, |(start, _)| *start)
            .map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor does not start at an admitted file range".to_owned(),
                )
            })
    }

    fn ensure_admitted_file(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if self.admitted_window.contains_key(&file_offset) {
            return Ok(());
        }
        // A rejected batch or restored cursor may revisit a range before the
        // current prefetch window. Keep one window, including during retries.
        self.admitted_window.clear();
        match self.file_source {
            Some(SealedLexicalFilesV1::Published(_)) => {
                self.fill_admitted_window_from_memory(file_offset, control)
            }
            Some(SealedLexicalFilesV1::Partitioned(_)) => {
                self.fill_admitted_window_from_segments(file_offset, control)
            }
            None => self.fill_admitted_window(file_offset, control),
        }
    }

    fn fill_admitted_window(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let start_index = self.file_range_index(file_offset)?;
        let workers = crate::parallelism::indexing_workers().max(1);
        let mut prefetch_bytes = 0u64;
        let mut inputs = Vec::new();
        for (index, &(start, end)) in self.file_ranges[start_index..].iter().enumerate() {
            let file_bytes = end.checked_sub(start).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file byte range is invalid".to_owned(),
                )
            })?;
            if index > 0
                && (inputs.len() >= workers
                    || prefetch_bytes
                        .checked_add(file_bytes)
                        .is_some_and(|total| total > LEXICAL_FILE_PREFETCH_BYTES_V1))
            {
                break;
            }
            checkpoint(control)?;
            let bytes = read_file_bytes_at_range(
                &mut self.reader,
                start,
                end,
                self.files_end_offset,
                self.maximum_file_bytes,
                control,
            )?;
            let next_file_offset = self
                .file_ranges
                .get(start_index + index + 1)
                .map(|(next_start, _)| *next_start)
                .unwrap_or(self.files_end_offset);
            prefetch_bytes = prefetch_bytes.saturating_add(file_bytes);
            inputs.push((start, bytes, next_file_offset));
        }
        if inputs.is_empty() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical file range produced no readable files".to_owned(),
            ));
        }
        let admitted =
            super::collect_bounded_ordered(&inputs, |(_start, bytes, next_offset), _| {
                admit_persisted_file_bytes(bytes, *next_offset, control)
            })?;
        for ((start, _, _), admitted) in inputs.into_iter().zip(admitted) {
            self.admitted_window.insert(start, Arc::new(admitted));
        }
        Ok(())
    }

    fn fill_admitted_window_from_memory(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let Some(SealedLexicalFilesV1::Published(files)) = self.file_source.as_ref() else {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical memory admit ran without published files".to_owned(),
            ));
        };
        let start_index = self.file_range_index(file_offset)?;
        let workers = crate::parallelism::indexing_workers().max(1);
        let mut prefetch_bytes = 0u64;
        let mut inputs = Vec::new();
        for (index, file) in files[start_index..].iter().enumerate() {
            let &(start, end) = self.file_ranges.get(start_index + index).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "published generation file is missing a sealed lexical range".to_owned(),
                )
            })?;
            let file_bytes = end.checked_sub(start).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file byte range is invalid".to_owned(),
                )
            })?;
            if index > 0
                && (inputs.len() >= workers
                    || prefetch_bytes
                        .checked_add(file_bytes)
                        .is_some_and(|total| total > LEXICAL_FILE_PREFETCH_BYTES_V1))
            {
                break;
            }
            checkpoint(control)?;
            let next_file_offset = self
                .file_ranges
                .get(start_index + index + 1)
                .map(|(next_start, _)| *next_start)
                .unwrap_or(self.files_end_offset);
            prefetch_bytes = prefetch_bytes.saturating_add(file_bytes);
            inputs.push((start, Arc::clone(file), next_file_offset));
        }
        if inputs.is_empty() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical memory range produced no published files".to_owned(),
            ));
        }
        let admitted =
            super::collect_bounded_ordered(&inputs, |(_start, file, next_offset), _| {
                admit_file_generation_artifacts(file, *next_offset, control)
            })?;
        for ((start, _, _), admitted) in inputs.into_iter().zip(admitted) {
            self.admitted_window.insert(start, Arc::new(admitted));
        }
        Ok(())
    }

    #[hotpath::measure(label = "code_index.restore.partitioned_window")]
    fn fill_admitted_window_from_segments(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let start = self.file_range_index(file_offset)?;
        let Some(SealedLexicalFilesV1::Partitioned(source)) = self.file_source.as_mut() else {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical segment admit ran without segment authority".to_owned(),
            ));
        };
        let files = source.read_window(
            start,
            crate::parallelism::indexing_workers().max(1),
            LEXICAL_FILE_PREFETCH_BYTES_V1,
            control,
        )?;
        let inputs = files.into_iter().enumerate().collect::<Vec<_>>();
        let admitted = super::collect_bounded_ordered(&inputs, |(index, file), _| {
            let next_offset = u64::try_from(start + index + 1).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file ordinal exceeds u64".to_owned(),
                )
            })?;
            admit_file_generation_artifacts(file, next_offset, control)
        })?;
        for (index, file) in admitted.into_iter().enumerate() {
            let offset = u64::try_from(start + index).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file ordinal exceeds u64".to_owned(),
                )
            })?;
            self.admitted_window.insert(offset, Arc::new(file));
        }
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
            symbol_displays,
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
            symbol_displays,
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
    serialized_chunks: Vec<Vec<u8>>,
    symbol_displays: BTreeMap<SymbolOccurrenceId, VerifiedSealedLexicalSymbolDisplayV1>,
    serialized_displays: Vec<Option<Vec<u8>>>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    serialized_imports: Vec<Vec<u8>>,
    next_file_offset: u64,
}

pub(super) struct SealedLexicalLayoutV1 {
    pub(super) state_digest: ManifestDigest,
    pub(super) format_revision: u32,
    file_count: u64,
    first_file_offset: u64,
    files_end_offset: u64,
    file_ranges: Vec<(u64, u64)>,
    maximum_file_bytes: u64,
    manifest_range: Option<(u64, u64)>,
    snapshot_range: Option<(u64, u64)>,
}

#[hotpath::measure(label = "code_index.restore.scan")]
pub(super) fn scan_layout<R: Read + Seek>(
    reader: &mut R,
    admitted_len: u64,
    expected_file_digest: Option<&ManifestDigest>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<SealedLexicalLayoutV1, CodeIndexProductionErrorV1> {
    scan_layout_with_progress(
        reader,
        admitted_len,
        expected_file_digest,
        control,
        &mut |_, _| {},
    )
}

fn scan_layout_with_progress<R: Read + Seek>(
    reader: &mut R,
    admitted_len: u64,
    expected_file_digest: Option<&ManifestDigest>,
    control: &dyn CodeIndexExecutionControlV1,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<SealedLexicalLayoutV1, CodeIndexProductionErrorV1> {
    if admitted_len > MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation exceeds the canonical byte limit".to_owned(),
        ));
    }
    reader.seek(SeekFrom::Start(0)).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed lexical source seek failed: {error}"))
    })?;
    hotpath::gauge!("code_index_lexical_layout_scan_attempts").inc(1);
    hotpath::gauge!("code_index_lexical_layout_bytes_total").set(admitted_len);
    hotpath::gauge!("code_index_lexical_layout_bytes_scanned").set(0);
    progress(0, admitted_len);
    let mut scanner = LayoutScanner::default();
    let mut file_hasher = expected_file_digest.map(|_| Sha256::new());
    let read_limit = admitted_len.checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed generation length overflowed".to_owned())
    })?;
    let mut remaining = read_limit;
    let mut observed = 0u64;
    let mut next_progress = LAYOUT_PROGRESS_INTERVAL_BYTES;
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
        let admitted_observed = observed.min(admitted_len);
        if admitted_observed >= next_progress || admitted_observed == admitted_len {
            hotpath::gauge!("code_index_lexical_layout_bytes_scanned").set(admitted_observed);
            progress(admitted_observed, admitted_len);
            next_progress = admitted_observed.saturating_add(LAYOUT_PROGRESS_INTERVAL_BYTES);
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutKey {
    StateDigest,
    Generation,
    Files,
    FormatRevision,
    Manifest,
    Snapshot,
}

impl LayoutKey {
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"state_digest" => Some(Self::StateDigest),
            b"generation" => Some(Self::Generation),
            b"files" => Some(Self::Files),
            b"format_revision" => Some(Self::FormatRevision),
            b"manifest" => Some(Self::Manifest),
            b"snapshot" => Some(Self::Snapshot),
            _ => None,
        }
    }
}

struct LayoutScanner {
    brace_depth: usize,
    bracket_depth: usize,
    in_string: bool,
    escaped: bool,
    string: [u8; 128],
    string_len: usize,
    string_overflowed: bool,
    completed_key: Option<LayoutKey>,
    pending_key: Option<LayoutKey>,
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
    file_ranges: Vec<(u64, u64)>,
    maximum_file_bytes: u64,
    captured_metadata_object: Option<(LayoutKey, u64, usize)>,
    manifest_range: Option<(u64, u64)>,
    snapshot_range: Option<(u64, u64)>,
}

impl Default for LayoutScanner {
    fn default() -> Self {
        Self {
            brace_depth: 0,
            bracket_depth: 0,
            in_string: false,
            escaped: false,
            string: [0; 128],
            string_len: 0,
            string_overflowed: false,
            completed_key: None,
            pending_key: None,
            capture_state_digest: false,
            state_digest: None,
            format_revision: None,
            generation_depth: None,
            generation_hasher: None,
            generation_digest: None,
            files_depth: None,
            current_file_start: None,
            first_file_offset: None,
            files_end_offset: None,
            file_count: 0,
            file_ranges: Vec::new(),
            maximum_file_bytes: 0,
            captured_metadata_object: None,
            manifest_range: None,
            snapshot_range: None,
        }
    }
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
        let mut index = 0usize;
        while index < bytes.len() {
            if self.in_string && !self.escaped {
                let relative_end = first_json_string_control(&bytes[index..]);
                let end = relative_end.map_or(bytes.len(), |relative| index + relative);
                if end > index {
                    self.observe_string_run(&bytes[index..end]);
                    index = end;
                    if index == bytes.len() {
                        break;
                    }
                }
            }
            let offset = u64::try_from(index)
                .ok()
                .and_then(|index| base_offset.checked_add(index))
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical source length overflowed".to_owned(),
                    )
                })?;
            match self.observe(bytes[index], offset)? {
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
            index += 1;
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

    /// Consume bytes that cannot alter JSON string state in one bounded step.
    /// Only a key or the envelope state digest is retained, and both are
    /// capped at the scanner's existing 128-byte contract.
    fn observe_string_run(&mut self, bytes: &[u8]) {
        let remaining = self.string.len().saturating_sub(self.string_len);
        let retained = remaining.min(bytes.len());
        let retained_end = self.string_len + retained;
        self.string[self.string_len..retained_end].copy_from_slice(&bytes[..retained]);
        self.string_len = retained_end;
        if retained < bytes.len() {
            self.string_overflowed = true;
        }
    }

    fn observe_string_byte(&mut self, byte: u8) {
        if self.string_len < self.string.len() {
            self.string[self.string_len] = byte;
            self.string_len += 1;
        } else {
            self.string_overflowed = true;
        }
    }

    fn observe(
        &mut self,
        byte: u8,
        offset: u64,
    ) -> Result<GenerationSpanEvent, CodeIndexProductionErrorV1> {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
                self.observe_string_byte(byte);
                return Ok(GenerationSpanEvent::None);
            }
            match byte {
                b'\\' => self.escaped = true,
                b'"' => {
                    self.in_string = false;
                    if self.capture_state_digest {
                        let value = String::from_utf8(self.string[..self.string_len].to_vec())
                            .map_err(|_| {
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
                        std::str::from_utf8(&self.string[..self.string_len]).map_err(|_| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed generation key is not UTF-8".to_owned(),
                            )
                        })?;
                        self.completed_key = LayoutKey::from_bytes(&self.string[..self.string_len]);
                    } else {
                        self.completed_key = None;
                    }
                    self.string_len = 0;
                    self.string_overflowed = false;
                }
                _ => self.observe_string_byte(byte),
            }
            return Ok(GenerationSpanEvent::None);
        }

        let mut event = GenerationSpanEvent::None;
        match byte {
            b'"' => {
                self.in_string = true;
                self.string_len = 0;
                self.string_overflowed = false;
                self.capture_state_digest =
                    self.pending_key == Some(LayoutKey::StateDigest) && self.brace_depth == 1;
            }
            b':' => self.pending_key = self.completed_key.take(),
            b'{' => {
                if self.pending_key == Some(LayoutKey::Generation) && self.brace_depth == 1 {
                    self.generation_depth = Some(self.brace_depth + 1);
                    self.generation_hasher = Some(Sha256::new());
                    event = GenerationSpanEvent::Opened;
                }
                if matches!(
                    self.pending_key,
                    Some(LayoutKey::Manifest | LayoutKey::Snapshot)
                ) && self.generation_depth == Some(self.brace_depth)
                {
                    let key = self.pending_key.ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed text metadata key disappeared".to_owned(),
                        )
                    })?;
                    if self.captured_metadata_object.is_some() {
                        return Err(CodeIndexProductionErrorV1::Contract(
                            "sealed text metadata objects overlap".to_owned(),
                        ));
                    }
                    self.captured_metadata_object = Some((key, offset, self.brace_depth + 1));
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
                if let Some((key, start, depth)) = self.captured_metadata_object
                    && depth == self.brace_depth
                {
                    let end = offset.checked_add(1).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed text metadata end offset overflowed".to_owned(),
                        )
                    })?;
                    match key {
                        LayoutKey::Manifest => self.manifest_range = Some((start, end)),
                        LayoutKey::Snapshot => self.snapshot_range = Some((start, end)),
                        _ => {
                            return Err(CodeIndexProductionErrorV1::Contract(
                                "sealed text metadata capture has an invalid key".to_owned(),
                            ));
                        }
                    }
                    self.captured_metadata_object = None;
                }
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
                    self.file_ranges.push((start, end));
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
                if self.pending_key == Some(LayoutKey::Files)
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
                if self.pending_key == Some(LayoutKey::FormatRevision)
                    && self.generation_depth == Some(self.brace_depth) =>
            {
                self.format_revision = Some(u32::from(byte - b'0'));
                self.pending_key = None;
            }
            b',' => {
                self.completed_key = None;
                self.pending_key = None;
            }
            byte if byte.is_ascii_whitespace() => {}
            _ => self.completed_key = None,
        }
        Ok(event)
    }

    fn finish(self) -> Result<SealedLexicalLayoutV1, CodeIndexProductionErrorV1> {
        if self.in_string
            || self.brace_depth != 0
            || self.bracket_depth != 0
            || self.current_file_start.is_some()
            || self.captured_metadata_object.is_some()
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
        // A superseded envelope is refused, not scanned: the caller rebuilds
        // the generation instead of falling through to another decoder that
        // would report these bytes as corrupt.
        if format_revision < MINIMUM_SEALED_GENERATION_FORMAT_REVISION {
            return Err(superseded_sealed_generation_revision(format_revision));
        }
        if format_revision != MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION {
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
        if u64::try_from(self.file_ranges.len()).unwrap_or(u64::MAX) != self.file_count {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical file ranges do not match the admitted file count".to_owned(),
            ));
        }
        Ok(SealedLexicalLayoutV1 {
            state_digest,
            format_revision,
            file_count: self.file_count,
            first_file_offset,
            files_end_offset,
            file_ranges: self.file_ranges,
            maximum_file_bytes: self.maximum_file_bytes,
            manifest_range: self.manifest_range,
            snapshot_range: self.snapshot_range,
        })
    }
}

/// Locate the next quote or escape marker with eight-byte candidate probes.
/// Every input byte is still authenticated by the outer SHA-256 stream; this
/// helper only avoids interpreting ordinary string payload bytes one by one.
fn first_json_string_control(bytes: &[u8]) -> Option<usize> {
    const LOW_BITS: u64 = 0x0101_0101_0101_0101;
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
    const QUOTES: u64 = u64::from_ne_bytes([b'"'; 8]);
    const ESCAPES: u64 = u64::from_ne_bytes([b'\\'; 8]);

    fn contains_zero_byte(value: u64) -> bool {
        value.wrapping_sub(LOW_BITS) & !value & HIGH_BITS != 0
    }

    let mut chunks = bytes.chunks_exact(8);
    for (chunk_index, chunk) in chunks.by_ref().enumerate() {
        let word = u64::from_ne_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        if contains_zero_byte(word ^ QUOTES) || contains_zero_byte(word ^ ESCAPES) {
            let base = chunk_index * 8;
            return chunk
                .iter()
                .position(|byte| matches!(*byte, b'"' | b'\\'))
                .map(|relative| base + relative);
        }
    }
    let tail_base = bytes.len() - chunks.remainder().len();
    chunks
        .remainder()
        .iter()
        .position(|byte| matches!(*byte, b'"' | b'\\'))
        .map(|relative| tail_base + relative)
}

#[hotpath::measure(label = "code_index.restore.metadata")]
fn read_verified_text_metadata<R: Read + Seek>(
    reader: &mut R,
    layout: &SealedLexicalLayoutV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<VerifiedSealedTextGenerationMetadataV1, CodeIndexProductionErrorV1> {
    fn decode_range<T: serde::de::DeserializeOwned, R: Read + Seek>(
        reader: &mut R,
        range: (u64, u64),
        label: &'static str,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<T, CodeIndexProductionErrorV1> {
        checkpoint(control)?;
        let length = range.1.checked_sub(range.0).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata range is invalid"
            ))
        })?;
        if length == 0 || length > MAX_LEXICAL_GENERATION_METADATA_BYTES {
            return Err(CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata exceeds its byte bound"
            )));
        }
        let length = usize::try_from(length).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata exceeds the platform limit"
            ))
        })?;
        reader.seek(SeekFrom::Start(range.0)).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata seek failed: {error}"
            ))
        })?;
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata read failed: {error}"
            ))
        })?;
        checkpoint(control)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed {label} metadata decoding failed: {error}"
            ))
        })
    }

    let manifest: CodeGenerationManifestV1 = hotpath::measure_block!(
        "code_index.restore.metadata.manifest_decode",
        decode_range(
            reader,
            layout.manifest_range.ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation manifest metadata is missing".to_owned(),
                )
            })?,
            "manifest",
            control,
        )
    )?;
    if manifest.source_commitments.is_none() {
        return Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable);
    }
    let snapshot: SanitizedCodeSnapshotV1 = hotpath::measure_block!(
        "code_index.restore.metadata.snapshot_decode",
        decode_range(
            reader,
            layout.snapshot_range.ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation snapshot metadata is missing".to_owned(),
                )
            })?,
            "snapshot",
            control,
        )
    )?;
    hotpath::measure_block!("code_index.restore.metadata.snapshot_validate", {
        snapshot
            .validate()
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    })?;
    hotpath::measure_block!("code_index.restore.metadata.digest_verify", {
        let snapshot_digest = canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &snapshot))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if snapshot_digest != manifest.snapshot_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed text metadata snapshot digest does not match the manifest".to_owned(),
            ));
        }
        let seal_digest = expected_seal_digest(&manifest)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if seal_digest != manifest.seal.expected_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed text metadata manifest seal is invalid".to_owned(),
            ));
        }
        Ok::<_, CodeIndexProductionErrorV1>(())
    })?;
    VerifiedSealedTextGenerationMetadataV1::from_partitioned_manifest(manifest, snapshot, None)
}

fn read_file_bytes_at_range<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
    files_end_offset: u64,
    maximum_file_bytes: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
    checkpoint(control)?;
    if start >= files_end_offset || end > files_end_offset || end <= start {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed lexical file range is outside the admitted source".to_owned(),
        ));
    }
    let file_bytes = end - start;
    if file_bytes > maximum_file_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed lexical file exceeds its admitted decode window".to_owned(),
        ));
    }
    let len = usize::try_from(file_bytes).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed lexical file window exceeds the platform limit".to_owned(),
        )
    })?;
    reader.seek(SeekFrom::Start(start)).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed lexical source seek failed: {error}"))
    })?;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed lexical file read failed: {error}"))
    })?;
    Ok(bytes)
}

fn admit_persisted_file_bytes(
    bytes: &[u8],
    next_file_offset: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
    checkpoint(control)?;
    let file: PersistedFileGenerationArtifactsV1 =
        hotpath::measure_block!("code_index.restore.file_decode", {
            serde_json::from_slice(bytes).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical file decoding failed: {error}"
                ))
            })
        })?;
    let exact_authority = ExactExtractionAuthorityV1::restore(&file.artifacts.chunks)
        .map_err(CodeIndexProductionErrorV1::Chunk)?;
    admit_validated_file_parts(
        &file.authority,
        &file.extraction,
        &file.artifacts,
        &exact_authority,
        next_file_offset,
        control,
    )
}

fn admit_file_generation_artifacts(
    file: &FileGenerationArtifactsV1,
    next_file_offset: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
    checkpoint(control)?;
    admit_validated_file_parts(
        &file.authority,
        &file.extraction,
        &file.artifacts,
        &file.exact_authority,
        next_file_offset,
        control,
    )
}

/// Serialize one retained page row through a reused staging buffer.
///
/// Every chunk, symbol display, and import row is kept for the page it lands
/// in, and `serde_json::to_vec` reaches that length by doubling a fresh
/// buffer: it churned one growing allocation per row and then retained up to
/// the row's length again as unused capacity. Staging the bytes once and
/// copying the exact slice keeps one allocation per row, sized to the row.
fn serialize_page_row<T: serde::Serialize>(
    value: &T,
    staging: &mut Vec<u8>,
    message: &'static str,
) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
    staging.clear();
    serde_json::to_writer(&mut *staging, value)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(format!("{message}: {error}")))?;
    Ok(staging.as_slice().to_vec())
}

fn admit_validated_file_parts(
    authority: &ReceiptBoundCodeFileAuthorityV1,
    extraction: &ExtractionBatchV1,
    artifacts: &CodeFileIndexArtifactsV1,
    exact_authority: &ExactExtractionAuthorityV1,
    next_file_offset: u64,
    _control: &dyn CodeIndexExecutionControlV1,
) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
    hotpath::measure_block!("code_index.restore.file_admit", {
        artifacts
            .validate()
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        artifacts
            .validate_generation_import_authority(extraction)
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        let document = &artifacts.chunks.document;
        if extraction.file_occurrence_id != document.file_occurrence_id
            || extraction.content_digest != document.content_digest
            || authority.content_digest != document.content_digest
            || artifacts
                .chunks
                .chunks
                .iter()
                .any(|chunk| chunk.anchor.generation_id != extraction.generation_id)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical extraction authority does not match its admitted document"
                    .to_owned(),
            ));
        }
        let mut symbol_displays = BTreeMap::new();
        for symbol in &artifacts.symbols {
            let display = VerifiedSealedLexicalSymbolDisplayV1 {
                occurrence: symbol.occurrence.clone(),
                simple_name: symbol.simple_name.clone(),
                qualified_name: symbol.qualified_name.clone(),
                kind: symbol.kind.clone(),
            };
            if symbol_displays
                .insert(symbol.occurrence.clone(), display)
                .is_some()
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed lexical file contains duplicate symbol display identities".to_owned(),
                ));
            }
        }
        let imports = artifacts.imports.clone();
        let chunks = hotpath::measure_block!(
            "code_index.restore.file_admit.exact_admission",
            exact_authority
                .admit_all(artifacts.chunks.chunks.clone())
                .map_err(CodeIndexProductionErrorV1::Chunk)
        )?;
        let (serialized_chunks, serialized_displays, serialized_imports) = hotpath::measure_block!(
            "code_index.restore.file_admit.serialize",
            {
                let mut serialized_chunks = Vec::with_capacity(chunks.len());
                let mut serialized_displays = Vec::with_capacity(chunks.len());
                let mut staging = Vec::new();
                for chunk in &chunks {
                    serialized_chunks.push(serialize_page_row(
                        chunk.chunk(),
                        &mut staging,
                        "sealed lexical chunk serialization failed",
                    )?);
                    let serialized_display =
                        match chunk.chunk().anchor.symbol_occurrence_id.as_ref() {
                            Some(occurrence) => Some(serialize_page_row(
                                symbol_displays.get(occurrence).ok_or_else(|| {
                                    CodeIndexProductionErrorV1::Contract(
                                        "sealed lexical symbol chunk has no parser-attested display identity"
                                            .to_owned(),
                                    )
                                })?,
                                &mut staging,
                                "sealed lexical symbol display serialization failed",
                            )?),
                            None => None,
                        };
                    serialized_displays.push(serialized_display);
                }
                let serialized_imports = imports
                    .iter()
                    .map(|evidence| {
                        serialize_page_row(
                            evidence,
                            &mut staging,
                            "sealed lexical import serialization failed",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, CodeIndexProductionErrorV1>((
                    serialized_chunks,
                    serialized_displays,
                    serialized_imports,
                ))
            }
        )?;
        Ok(AdmittedSealedLexicalFileV1 {
            chunks,
            serialized_chunks,
            symbol_displays,
            serialized_displays,
            imports,
            serialized_imports,
            next_file_offset,
        })
    })
}

pub(super) fn checkpoint(
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeIndexProductionErrorV1> {
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

fn hash_symbol_display_record(
    hasher: &mut Sha256,
    bytes: &[u8],
) -> Result<(), CodeIndexProductionErrorV1> {
    hasher.update(SYMBOL_DISPLAY_RECORD_DOMAIN);
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
#[path = "lexical_page_source_tests.rs"]
mod lexical_page_source_tests;
