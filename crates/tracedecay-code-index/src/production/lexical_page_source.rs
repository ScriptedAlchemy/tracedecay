use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc};

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CodeGenerationSourceCommitmentsV1, CodeSearchChunkGrainV1, CodeSearchChunkV1,
    ExactTechnicalTermV1,
};

use crate::{
    capabilities::expected_seal_digest, clones::CodeIndexCloneBodyV1,
    intake::INTAKE_DIGEST_SEPARATOR, lineage::LineageSymbolRecordV1,
};

use super::partitioned_codec::PartitionedLexicalFileSourceV1;
use super::{FileGenerationArtifactsV1, *};

const PAGE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-page.v1\0";
const SOURCE_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source.v1\0";
const IMPORT_DICTIONARY_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-import-dictionary.v1\0";
const IMPORT_RECORD_DOMAIN: &[u8] = b"import\0";
const CLONE_BODY_RECORD_DOMAIN: &[u8] = b"clone-body\0";
const SOURCE_CHAIN_RECORD_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-source-chain.v1\0";
const SYMBOL_DISPLAY_RECORD_DOMAIN: &[u8] = b"symbol-display\0";
const SOURCE_SYMBOL_DISPLAY_CHAIN_RECORD_DOMAIN: &[u8] =
    b"tracedecay.sealed-lexical-symbol-display-chain.v1\0";
const IMPORT_DICTIONARY_CHAIN_RECORD_DOMAIN: &[u8] =
    b"tracedecay.sealed-lexical-import-dictionary-chain.v1\0";
const CURSOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.sealed-lexical-cursor.v1\0";
const INVALID_CURSOR_POSITION_DETAIL: &str =
    "sealed lexical cursor is not a valid position in its next file";
/// Concurrent exact-read/decode window. Same 64 MiB retain cap as one
/// lexical page batch, so prefetch cannot exceed a window the builder
/// already admits for staged pages.
/// Bound on the sealed file bytes read ahead of one decode window: the
/// lexical source's admitted-file prefetch and the partitioned decoder's
/// segment window share it so neither holds more than this in decoded
/// segment bytes while the pool decodes them.
pub(super) const LEXICAL_FILE_PREFETCH_BYTES_V1: u64 = 64 * 1024 * 1024;
/// Files admitted per worker in one restore window, mirroring the encode
/// side's `SEALED_ENCODE_WINDOW_FILES_PER_WORKER_V1`. A bare `workers`-sized
/// window forces a fresh `install()` fan-out (and its barrier/dispatch
/// overhead) every `workers` files during a drain; multiplying it lets the
/// byte budget above (`LEXICAL_FILE_PREFETCH_BYTES_V1`) be the binding
/// constraint far more often, without changing how many bytes are held in
/// flight at once (the byte cap still applies on top of this file cap).
pub(super) const LEXICAL_DECODE_WINDOW_FILES_PER_WORKER_V1: usize = 4;

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
    next_clone_body_ordinal: u64,
    next_page_ordinal: u64,
    emitted_chunks: u64,
    emitted_payload_bytes: u64,
    emitted_imports: u64,
    emitted_import_payload_bytes: u64,
    emitted_clone_bodies: u64,
    emitted_clone_body_payload_bytes: u64,
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
            next_clone_body_ordinal: 0,
            next_page_ordinal: 0,
            emitted_chunks: 0,
            emitted_payload_bytes: 0,
            emitted_imports: 0,
            emitted_import_payload_bytes: 0,
            emitted_clone_bodies: 0,
            emitted_clone_body_payload_bytes: 0,
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
            self.next_clone_body_ordinal,
            self.next_page_ordinal,
            self.emitted_chunks,
            self.emitted_payload_bytes,
            self.next_import_ordinal,
            self.emitted_imports,
            self.emitted_import_payload_bytes,
            self.emitted_clone_bodies,
            self.emitted_clone_body_payload_bytes,
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
            next_clone_body_ordinal,
            next_page_ordinal,
            emitted_chunks,
            emitted_payload_bytes,
            next_import_ordinal,
            emitted_imports,
            emitted_import_payload_bytes,
            emitted_clone_bodies,
            emitted_clone_body_payload_bytes,
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
            next_clone_body_ordinal,
            next_page_ordinal,
            emitted_chunks,
            emitted_payload_bytes,
            emitted_imports,
            emitted_import_payload_bytes,
            emitted_clone_bodies,
            emitted_clone_body_payload_bytes,
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
            && self.next_clone_body_ordinal == 0
            && self.next_page_ordinal == 0
            && self.emitted_chunks == 0
            && self.emitted_payload_bytes == 0
            && self.emitted_imports == 0
            && self.emitted_import_payload_bytes == 0
            && self.emitted_clone_bodies == 0
            && self.emitted_clone_body_payload_bytes == 0
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
        hasher.update(self.next_clone_body_ordinal.to_le_bytes());
        hasher.update(self.next_page_ordinal.to_le_bytes());
        hasher.update(self.emitted_chunks.to_le_bytes());
        hasher.update(self.emitted_payload_bytes.to_le_bytes());
        hasher.update(self.emitted_imports.to_le_bytes());
        hasher.update(self.emitted_import_payload_bytes.to_le_bytes());
        hasher.update(self.emitted_clone_bodies.to_le_bytes());
        hasher.update(self.emitted_clone_body_payload_bytes.to_le_bytes());
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

/// Parser-attested display fields for one symbol-backed chunk.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct VerifiedSealedLexicalSymbolDisplayV1 {
    occurrence: SymbolOccurrenceId,
    simple_name: String,
    qualified_name: String,
    kind: String,
    signature: Option<String>,
    documentation: Option<String>,
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

    pub fn signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    pub fn retained_owned_bytes(&self) -> usize {
        self.occurrence
            .as_str()
            .len()
            .saturating_add(self.simple_name.capacity())
            .saturating_add(self.qualified_name.capacity())
            .saturating_add(self.kind.capacity())
            .saturating_add(self.signature.as_ref().map_or(0, String::capacity))
            .saturating_add(self.documentation.as_ref().map_or(0, String::capacity))
    }
}

impl From<&LineageSymbolRecordV1> for VerifiedSealedLexicalSymbolDisplayV1 {
    fn from(symbol: &LineageSymbolRecordV1) -> Self {
        Self {
            occurrence: symbol.occurrence.clone(),
            simple_name: symbol.simple_name.clone(),
            qualified_name: symbol.qualified_name.clone(),
            kind: symbol.kind.clone(),
            signature: symbol.signature.clone(),
            documentation: symbol.docstring.clone(),
        }
    }
}

fn symbol_display_for_chunk(
    chunk: &CodeSearchChunkV1,
    displays: &BTreeMap<SymbolOccurrenceId, VerifiedSealedLexicalSymbolDisplayV1>,
) -> Result<Option<VerifiedSealedLexicalSymbolDisplayV1>, CodeIndexProductionErrorV1> {
    let Some(occurrence) = chunk.anchor.symbol_occurrence_id.as_ref() else {
        return Ok(None);
    };
    let mut display = displays.get(occurrence).cloned().ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "sealed lexical symbol chunk has no parser-attested display identity".to_owned(),
        )
    })?;
    if chunk.anchor.grain != CodeSearchChunkGrainV1::SymbolSignature {
        display.signature = None;
        display.documentation = None;
    }
    Ok(Some(display))
}

/// One bounded page of parser-backed lexical and clone rows.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageV1 {
    page_ordinal: u64,
    chunk_count: u64,
    payload_bytes: u64,
    import_count: u64,
    import_payload_bytes: u64,
    clone_body_count: u64,
    clone_body_payload_bytes: u64,
    page_digest: ManifestDigest,
    cumulative_digest: ManifestDigest,
    next_cursor: VerifiedSealedLexicalCursorV1,
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    symbol_displays: Vec<Option<VerifiedSealedLexicalSymbolDisplayV1>>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    clone_bodies: Vec<CodeIndexCloneBodyV1>,
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

    pub fn imports(&self) -> &[CodeIndexImportEvidenceV1] {
        &self.imports
    }

    pub fn clone_bodies(&self) -> &[CodeIndexCloneBodyV1] {
        &self.clone_bodies
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
        let clone_body_payload_bytes = Self::verify_clone_body_records(
            &self.clone_bodies,
            &mut page_hasher,
            &mut cumulative_digest,
        )?;
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
        let clone_body_count = u64::try_from(self.clone_bodies.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page clone-body count exceeds u64".to_owned(),
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
            || self.clone_body_count != clone_body_count
            || self.clone_body_payload_bytes != clone_body_payload_bytes
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
            || self.next_cursor.emitted_clone_bodies
                != self
                    .previous_cursor
                    .emitted_clone_bodies
                    .checked_add(clone_body_count)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source clone-body count overflowed".to_owned(),
                        )
                    })?
            || self.next_cursor.emitted_clone_body_payload_bytes
                != self
                    .previous_cursor
                    .emitted_clone_body_payload_bytes
                    .checked_add(clone_body_payload_bytes)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lexical source clone-body payload overflowed".to_owned(),
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

    fn verify_clone_body_records(
        bodies: &[CodeIndexCloneBodyV1],
        page_hasher: &mut Sha256,
        cumulative_digest: &mut ManifestDigest,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        let mut payload_bytes = 0u64;
        for body in bodies {
            let serialized = serde_json::to_vec(body).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed lexical clone-body serialization failed: {error}"
                ))
            })?;
            hash_clone_body_record(page_hasher, &serialized)?;
            *cumulative_digest =
                advance_digest(cumulative_digest, CLONE_BODY_RECORD_DOMAIN, &serialized)?;
            payload_bytes = payload_bytes
                .checked_add(u64::try_from(serialized.len()).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical clone-body payload exceeds u64".to_owned(),
                    )
                })?)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical clone-body payload overflowed".to_owned(),
                    )
                })?;
        }
        Ok(payload_bytes)
    }

    /// Heap bytes retained by this page's chunk, import, and clone vectors plus its
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
        let import_bytes = self.imports.iter().fold(
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
        );
        self.clone_bodies.iter().fold(
            import_bytes.saturating_add(
                self.clone_bodies
                    .capacity()
                    .saturating_mul(std::mem::size_of::<CodeIndexCloneBodyV1>()),
            ),
            |bytes, body| bytes.saturating_add(body.retained_owned_bytes()),
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
    total_clone_bodies: u64,
    clone_body_payload_bytes: u64,
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

    pub fn total_clone_bodies(&self) -> u64 {
        self.total_clone_bodies
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
                && self.total_clone_bodies == 0
                && self.clone_body_payload_bytes == 0
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
            || self.total_clone_bodies != cursor.emitted_clone_bodies
            || self.clone_body_payload_bytes != cursor.emitted_clone_body_payload_bytes
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
    clone_bodies: Vec<CodeIndexCloneBodyV1>,
    clone_body_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    page_hasher: Sha256,
}

struct CloneBodyPageStageV1<'a> {
    existing_records: usize,
    existing_bytes: usize,
    clone_bodies: &'a mut Vec<CodeIndexCloneBodyV1>,
    clone_body_bytes: &'a mut usize,
    cursor: &'a mut VerifiedSealedLexicalCursorV1,
    page_hasher: &'a mut Sha256,
}

impl CloneBodyPageStageV1<'_> {
    fn append(
        &mut self,
        admitted: &AdmittedSealedLexicalFileV1,
        maximum_page_records: usize,
        maximum_page_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        let mut ordinal = usize::try_from(self.cursor.next_clone_body_ordinal).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical clone-body cursor exceeds the platform limit".to_owned(),
            )
        })?;
        if ordinal > admitted.clone_bodies.len() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical cursor exceeds its file clone-body count".to_owned(),
            ));
        }
        while ordinal < admitted.clone_bodies.len() {
            checkpoint(control)?;
            let serialized = &admitted.serialized_clone_bodies[ordinal];
            if serialized.len() > maximum_page_bytes {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "one admitted clone body exceeds the page byte bound".to_owned(),
                ));
            }
            if self
                .existing_records
                .saturating_add(self.clone_bodies.len())
                >= maximum_page_records
                || self
                    .existing_bytes
                    .saturating_add(*self.clone_body_bytes)
                    .saturating_add(serialized.len())
                    > maximum_page_bytes
            {
                return Ok(true);
            }
            hash_clone_body_record(self.page_hasher, serialized)?;
            self.cursor.cumulative_digest = advance_digest(
                &self.cursor.cumulative_digest,
                CLONE_BODY_RECORD_DOMAIN,
                serialized,
            )?;
            *self.clone_body_bytes = self
                .clone_body_bytes
                .checked_add(serialized.len())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical clone-body page byte count overflowed".to_owned(),
                    )
                })?;
            self.clone_bodies
                .push(admitted.clone_bodies[ordinal].clone());
            ordinal += 1;
            self.cursor.next_clone_body_ordinal = u64::try_from(ordinal).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical clone-body ordinal exceeds u64".to_owned(),
                )
            })?;
        }
        Ok(false)
    }
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

/// Bounded lexical projection source over an authenticated partitioned
/// sealed generation.
///
/// Opening authenticates the manifest; file segments are read and verified
/// one bounded admission window at a time and decoded on the indexing pool.
/// File ranges are file ordinals. Page minting stays serial because the
/// cumulative digest is a chain. Raw sealed bytes never cross this interface.
#[derive(Debug)]
pub struct VerifiedSealedLexicalPageSourceV1 {
    file_count: u64,
    first_file_offset: u64,
    files_end_offset: u64,
    file_ranges: Vec<(u64, u64)>,
    lexical_byte_offsets: Vec<u64>,
    total_lexical_units: u64,
    maximum_file_bytes: u64,
    source_state_digest: ManifestDigest,
    format_revision: u32,
    metadata: VerifiedSealedTextGenerationMetadataV1,
    maximum_page_chunks: usize,
    maximum_page_bytes: usize,
    cursor: VerifiedSealedLexicalCursorV1,
    admitted_window: BTreeMap<u64, Arc<AdmittedSealedLexicalFileV1>>,
    /// Durable partitioned descriptors. Only the next bounded admission
    /// window is loaded; cursors retain stable file ordinals across process
    /// restarts.
    file_source: PartitionedLexicalFileSourceV1,
}

/// Authenticated generation metadata needed by exact and lexical serving.
///
/// The full sealed generation can be gigabytes. This projection retains only
/// the manifest, sanitized snapshot, and statistics the partitioned manifest
/// carries; the manifest is authenticated before this value is observable.
#[derive(Clone, Debug)]
pub struct VerifiedSealedTextGenerationMetadataV1 {
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    statistics: CodeIndexGenerationStatisticsV1,
}

impl VerifiedSealedTextGenerationMetadataV1 {
    pub fn from_published_generation(generation: &CodeIndexPublishedGenerationV1) -> Self {
        Self {
            manifest: generation.manifest().clone(),
            snapshot: generation.snapshot().clone(),
            statistics: generation.statistics.clone(),
        }
    }

    pub(super) fn from_partitioned_manifest(
        manifest: CodeGenerationManifestV1,
        snapshot: SanitizedCodeSnapshotV1,
        statistics: CodeIndexGenerationStatisticsV1,
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

    pub fn generation_statistics(&self) -> &CodeIndexGenerationStatisticsV1 {
        &self.statistics
    }
}

impl VerifiedSealedLexicalPageSourceV1 {
    /// The content key of what this source emits: its format and the
    /// content-only identity of every file it reads. Two sources with one
    /// key emit the same records apart from route identity, which a lexical
    /// artifact keeps out of its rows.
    pub fn content_key(&self) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.sealed-lexical-source-content.v1\0");
        hasher.update(self.format_revision.to_le_bytes());
        self.file_source.content_digest(&mut hasher);
        ManifestDigest::from_sha256_bytes(&hasher.finalize())
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }

    // Every argument is a distinct authority the constructor binds together
    // exactly once: the manifest, the sanitized snapshot, the statistics, the
    // partitioned file source, its state digest, and the two page bounds. Grouping any of them into a parameter struct would
    // invent a type with one construction site and hide which authority a
    // caller failed to supply.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate authority bound once at construction"
    )]
    pub(super) fn open_partitioned_parts(
        manifest: CodeGenerationManifestV1,
        snapshot: SanitizedCodeSnapshotV1,
        statistics: CodeIndexGenerationStatisticsV1,
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
        let lexical_byte_offsets = source.lexical_byte_offsets()?;
        let total_lexical_units = lexical_byte_offsets.last().copied().ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "partitioned lexical byte offsets are empty".to_owned(),
            )
        })?;
        let maximum_file_bytes = source.maximum_file_bytes();
        let cursor = VerifiedSealedLexicalCursorV1::initial(source_state_digest.clone(), 0)?;
        Ok(Self {
            file_count,
            first_file_offset: 0,
            files_end_offset: file_count,
            file_ranges,
            lexical_byte_offsets,
            total_lexical_units,
            maximum_file_bytes,
            source_state_digest,
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            metadata,
            maximum_page_chunks,
            maximum_page_bytes,
            cursor,
            admitted_window: BTreeMap::new(),
            file_source: source,
        })
    }

    pub fn metadata(&self) -> &VerifiedSealedTextGenerationMetadataV1 {
        &self.metadata
    }

    pub fn format_revision(&self) -> u32 {
        self.format_revision
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
                || cursor.next_clone_body_ordinal != 0
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
            let clone_body_count = u64::try_from(admitted.clone_bodies.len()).map_err(|_| {
                VerifiedSealedLexicalCursorRestoreErrorV1::Production(
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lexical file clone-body count exceeds u64".to_owned(),
                    ),
                )
            })?;
            if cursor.next_chunk_ordinal > chunk_count
                || cursor.next_import_ordinal > import_count
                || cursor.next_clone_body_ordinal > clone_body_count
                || (cursor.next_chunk_ordinal < chunk_count
                    && (cursor.next_import_ordinal != 0 || cursor.next_clone_body_ordinal != 0))
                || (cursor.next_import_ordinal < import_count
                    && cursor.next_clone_body_ordinal != 0)
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
        let completed = usize::try_from(self.cursor.next_file_ordinal()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical completed file count exceeds usize".to_owned(),
            )
        })?;
        self.lexical_byte_offsets
            .get(completed)
            .copied()
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed lexical cursor exceeds partitioned byte bounds".to_owned(),
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
        let source_bytes = self.file_source.retained_layout_bytes();
        std::mem::size_of::<u64>()
            .saturating_mul(4)
            .saturating_add(
                self.file_ranges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u64, u64)>()),
            )
            .saturating_add(
                self.lexical_byte_offsets
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u64>()),
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
        let mut clone_bodies = Vec::new();
        let mut clone_body_bytes = 0usize;

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
                let display = symbol_display_for_chunk(chunk, &admitted.symbol_displays)?;
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
                if (!chunks.is_empty() || !imports.is_empty() || !clone_bodies.is_empty())
                    && (chunks.len() == self.maximum_page_chunks
                        || page_bytes
                            .saturating_add(next_symbol_display_bytes)
                            .saturating_add(import_bytes)
                            .saturating_add(clone_body_bytes)
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
                            clone_bodies,
                            clone_body_bytes,
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
                            clone_bodies,
                            clone_body_bytes,
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
                if (!chunks.is_empty() || !imports.is_empty() || !clone_bodies.is_empty())
                    && (chunks
                        .len()
                        .saturating_add(imports.len())
                        .saturating_add(clone_bodies.len())
                        >= self.maximum_page_chunks
                        || page_bytes
                            .saturating_add(symbol_display_bytes)
                            .saturating_add(import_bytes)
                            .saturating_add(clone_body_bytes)
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
                            clone_bodies,
                            clone_body_bytes,
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
            let clone_page_full = CloneBodyPageStageV1 {
                existing_records: chunks.len().saturating_add(imports.len()),
                existing_bytes: page_bytes
                    .saturating_add(symbol_display_bytes)
                    .saturating_add(import_bytes),
                clone_bodies: &mut clone_bodies,
                clone_body_bytes: &mut clone_body_bytes,
                cursor: &mut cursor,
                page_hasher: &mut page_hasher,
            }
            .append(
                &admitted,
                self.maximum_page_chunks,
                self.maximum_page_bytes,
                control,
            )?;
            if clone_page_full {
                return self.commit_page(
                    previous_cursor,
                    PendingSealedLexicalPageV1 {
                        chunks,
                        page_bytes,
                        symbol_displays,
                        imports,
                        import_bytes,
                        clone_bodies,
                        clone_body_bytes,
                        cursor,
                        page_hasher,
                    },
                );
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
            cursor.next_clone_body_ordinal = 0;
            // The page contract serializes chunks, then imports, then clone
            // bodies. Commit before a later file restarts that ordering.
            if !imports.is_empty() || !clone_bodies.is_empty() {
                return self.commit_page(
                    previous_cursor,
                    PendingSealedLexicalPageV1 {
                        chunks,
                        page_bytes,
                        symbol_displays,
                        imports,
                        import_bytes,
                        clone_bodies,
                        clone_body_bytes,
                        cursor,
                        page_hasher,
                    },
                );
            }
        }

        if !chunks.is_empty() || !imports.is_empty() || !clone_bodies.is_empty() {
            return self.commit_page(
                previous_cursor,
                PendingSealedLexicalPageV1 {
                    chunks,
                    page_bytes,
                    symbol_displays,
                    imports,
                    import_bytes,
                    clone_bodies,
                    clone_body_bytes,
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
                total_clone_bodies: cursor.emitted_clone_bodies,
                clone_body_payload_bytes: cursor.emitted_clone_body_payload_bytes,
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
        self.fill_admitted_window(file_offset, control)
    }

    #[hotpath::measure(label = "code_index.restore.partitioned_window")]
    fn fill_admitted_window(
        &mut self,
        file_offset: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let snapshot_digest = self.metadata.manifest().snapshot_digest.clone();
        let start = self.file_range_index(file_offset)?;
        let files = self.file_source.read_window(
            start,
            crate::parallelism::indexing_workers()
                .max(1)
                .saturating_mul(LEXICAL_DECODE_WINDOW_FILES_PER_WORKER_V1),
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
            admit_file_generation_artifacts(file, &snapshot_digest, next_offset, control)
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
            clone_bodies,
            clone_body_bytes,
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
        let clone_body_count = u64::try_from(clone_bodies.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page clone-body count exceeds u64".to_owned(),
            )
        })?;
        let clone_body_payload_bytes = u64::try_from(clone_body_bytes).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical page clone-body byte count exceeds u64".to_owned(),
            )
        })?;
        advance_page_cursor(
            &mut cursor,
            chunk_count,
            payload_bytes,
            import_count,
            import_payload_bytes,
            clone_body_count,
            clone_body_payload_bytes,
        )?;
        hash_cursor(&mut page_hasher, &cursor)?;
        let page = VerifiedSealedLexicalPageV1 {
            page_ordinal,
            chunk_count,
            payload_bytes,
            import_count,
            import_payload_bytes,
            clone_body_count,
            clone_body_payload_bytes,
            page_digest: digest_hasher(page_hasher)?,
            cumulative_digest: cursor.cumulative_digest.clone(),
            next_cursor: cursor.clone(),
            chunks,
            symbol_displays,
            imports,
            clone_bodies,
            previous_cursor: previous_cursor.clone(),
        };
        Ok(StagedSealedLexicalPageReadV1::Page(
            StagedSealedLexicalPageV1 { page, cursor },
        ))
    }
}

fn advance_page_cursor(
    cursor: &mut VerifiedSealedLexicalCursorV1,
    chunk_count: u64,
    payload_bytes: u64,
    import_count: u64,
    import_payload_bytes: u64,
    clone_body_count: u64,
    clone_body_payload_bytes: u64,
) -> Result<(), CodeIndexProductionErrorV1> {
    cursor.next_page_ordinal = cursor.next_page_ordinal.checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed lexical page ordinal overflowed".to_owned())
    })?;
    for (total, increment, message) in [
        (
            &mut cursor.emitted_chunks,
            chunk_count,
            "sealed lexical source chunk count overflowed",
        ),
        (
            &mut cursor.emitted_payload_bytes,
            payload_bytes,
            "sealed lexical source byte count overflowed",
        ),
        (
            &mut cursor.emitted_imports,
            import_count,
            "sealed lexical source import count overflowed",
        ),
        (
            &mut cursor.emitted_import_payload_bytes,
            import_payload_bytes,
            "sealed lexical source import byte count overflowed",
        ),
        (
            &mut cursor.emitted_clone_bodies,
            clone_body_count,
            "sealed lexical source clone-body count overflowed",
        ),
        (
            &mut cursor.emitted_clone_body_payload_bytes,
            clone_body_payload_bytes,
            "sealed lexical source clone-body byte count overflowed",
        ),
    ] {
        *total = total
            .checked_add(increment)
            .ok_or_else(|| CodeIndexProductionErrorV1::Contract(message.to_owned()))?;
    }
    Ok(())
}

#[derive(Debug)]
struct AdmittedSealedLexicalFileV1 {
    chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    serialized_chunks: Vec<Vec<u8>>,
    symbol_displays: BTreeMap<SymbolOccurrenceId, VerifiedSealedLexicalSymbolDisplayV1>,
    serialized_displays: Vec<Option<Vec<u8>>>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    serialized_imports: Vec<Vec<u8>>,
    clone_bodies: Vec<CodeIndexCloneBodyV1>,
    serialized_clone_bodies: Vec<Vec<u8>>,
    next_file_offset: u64,
}

fn admit_file_generation_artifacts(
    file: &FileGenerationArtifactsV1,
    snapshot_digest: &ManifestDigest,
    next_file_offset: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<AdmittedSealedLexicalFileV1, CodeIndexProductionErrorV1> {
    checkpoint(control)?;
    admit_validated_file_parts(
        &file.authority,
        &file.extraction,
        &file.artifacts,
        &file.exact_authority,
        snapshot_digest,
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
    snapshot_digest: &ManifestDigest,
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
        artifacts
            .validate_generation_clone_authority(authority, extraction, snapshot_digest)
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
            let display = VerifiedSealedLexicalSymbolDisplayV1::from(symbol.as_ref());
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
        let clone_bodies = artifacts.clone_bodies.clone();
        let chunks = hotpath::measure_block!(
            "code_index.restore.file_admit.exact_admission",
            exact_authority
                .admit_all(artifacts.chunks.chunks.clone())
                .map_err(CodeIndexProductionErrorV1::Chunk)
        )?;
        let (serialized_chunks, serialized_displays, serialized_imports, serialized_clone_bodies) =
            hotpath::measure_block!("code_index.restore.file_admit.serialize", {
                let mut serialized_chunks = Vec::with_capacity(chunks.len());
                let mut serialized_displays = Vec::with_capacity(chunks.len());
                let mut staging = Vec::new();
                for chunk in &chunks {
                    serialized_chunks.push(serialize_page_row(
                        chunk.chunk(),
                        &mut staging,
                        "sealed lexical chunk serialization failed",
                    )?);
                    let display = symbol_display_for_chunk(chunk.chunk(), &symbol_displays)?;
                    let serialized_display = display
                        .as_ref()
                        .map(|display| {
                            serialize_page_row(
                                display,
                                &mut staging,
                                "sealed lexical symbol display serialization failed",
                            )
                        })
                        .transpose()?;
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
                let serialized_clone_bodies = clone_bodies
                    .iter()
                    .map(|body| {
                        serialize_page_row(
                            body,
                            &mut staging,
                            "sealed lexical clone-body serialization failed",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, CodeIndexProductionErrorV1>((
                    serialized_chunks,
                    serialized_displays,
                    serialized_imports,
                    serialized_clone_bodies,
                ))
            })?;
        Ok(AdmittedSealedLexicalFileV1 {
            chunks,
            serialized_chunks,
            symbol_displays,
            serialized_displays,
            imports,
            serialized_imports,
            clone_bodies,
            serialized_clone_bodies,
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
    hasher.update(cursor.next_clone_body_ordinal.to_le_bytes());
    hasher.update(cursor.next_page_ordinal.to_le_bytes());
    hasher.update(cursor.emitted_chunks.to_le_bytes());
    hasher.update(cursor.emitted_payload_bytes.to_le_bytes());
    hasher.update(cursor.emitted_imports.to_le_bytes());
    hasher.update(cursor.emitted_import_payload_bytes.to_le_bytes());
    hasher.update(cursor.emitted_clone_bodies.to_le_bytes());
    hasher.update(cursor.emitted_clone_body_payload_bytes.to_le_bytes());
    hash_record(hasher, cursor.import_dictionary_digest.as_str().as_bytes())?;
    hash_record(hasher, cursor.cumulative_digest.as_str().as_bytes())
}

fn hash_import_record(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), CodeIndexProductionErrorV1> {
    hasher.update(IMPORT_RECORD_DOMAIN);
    hash_record(hasher, bytes)
}

fn hash_clone_body_record(
    hasher: &mut Sha256,
    bytes: &[u8],
) -> Result<(), CodeIndexProductionErrorV1> {
    hasher.update(CLONE_BODY_RECORD_DOMAIN);
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
