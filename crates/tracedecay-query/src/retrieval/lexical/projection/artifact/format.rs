use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkId,
    ExactTechnicalTermV1, FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest,
    RepositoryId, SourceFreshness, SourceSpan, SymbolOccurrenceId,
};

use super::super::{CodeLexicalProjectionMetadataV1, LexicalFieldV1, ProjectedChunkV1};
use super::CodeLexicalArtifactErrorV1;

/// Revision 2 adds durable finalization/integrity state. Revision 1 artifacts
/// are branch-only staging files and must fail as incompatible rather than be
/// partially interpreted against this schema.
// Revision 3 replaces the branch-local computed finalization cursor with
// native table keys. Revision 4 adds document-leading indexes so verifying
// one document's receipt never scans an unrelated generation.
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1: u32 = 4;
const ARTIFACT_DIGEST_DOMAIN: &[u8] = b"tracedecay.code-lexical-artifact.v4\0";
pub(super) const RECEIPT_RESERVATION_BYTES: usize = 16 * 1024;
pub(super) const SECTION_NAMES: [&str; 11] = [
    "source_pages",
    "document_integrity",
    "import_integrity",
    "import_evidence",
    "rows",
    "term_postings",
    "exact_postings",
    "ngram_postings",
    "field_stats",
    "term_stats",
    "vocabulary",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalArtifactSectionDigestV1 {
    pub name: String,
    pub row_count: u64,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCodeLexicalArtifactV1 {
    format_revision: u32,
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    metadata_digest: ManifestDigest,
    source_state_digest: ManifestDigest,
    source_cumulative_digest: ManifestDigest,
    source_format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: ManifestDigest,
    artifact_digest: ManifestDigest,
    section_digests: Vec<CodeLexicalArtifactSectionDigestV1>,
    file_size_bytes: u64,
}

impl VerifiedCodeLexicalArtifactV1 {
    pub fn artifact_digest(&self) -> &ManifestDigest {
        &self.artifact_digest
    }

    pub fn file_size_bytes(&self) -> u64 {
        self.file_size_bytes
    }

    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    pub fn repository_id(&self) -> Option<&RepositoryId> {
        self.repository_id.as_ref()
    }

    pub fn freshness(&self) -> &SourceFreshness {
        &self.freshness
    }

    pub fn source_state_digest(&self) -> &ManifestDigest {
        &self.source_state_digest
    }

    pub fn source_cumulative_digest(&self) -> &ManifestDigest {
        &self.source_cumulative_digest
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

    pub fn section_digests(&self) -> &[CodeLexicalArtifactSectionDigestV1] {
        &self.section_digests
    }

    pub(super) fn format_revision(&self) -> u32 {
        self.format_revision
    }

    pub(super) fn metadata_digest(&self) -> &ManifestDigest {
        &self.metadata_digest
    }

    pub(super) fn source_format_revision(&self) -> u32 {
        self.source_format_revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLexicalArtifactOccurrenceV1 {
    pub generation: CodeGenerationId,
    pub file: FileOccurrenceId,
    pub symbol: Option<SymbolOccurrenceId>,
    pub chunk: CodeSearchChunkId,
    pub source_span: SourceSpan,
    pub logical_path: String,
    pub sanitized_text: BoundedSanitizedText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLexicalImportMembershipWitnessV1 {
    pub artifact_digest: ManifestDigest,
    pub import_dictionary_digest: ManifestDigest,
    pub evidence: CodeIndexImportEvidenceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactRowV1 {
    pub id: CodeSearchChunkId,
    pub anchor: CodeSearchChunkAnchorV1,
    pub language_descriptor_revision: LanguageDescriptorRevision,
    pub exact_terms: Vec<ExactTechnicalTermV1>,
    pub sanitized_text: BoundedSanitizedText,
    pub logical_path: String,
    pub field_lengths: BTreeMap<LexicalFieldV1, usize>,
    pub normalized_text: String,
}

impl From<ProjectedChunkV1> for ArtifactRowV1 {
    fn from(row: ProjectedChunkV1) -> Self {
        Self {
            id: row.id,
            anchor: row.anchor,
            language_descriptor_revision: row.language_descriptor_revision,
            exact_terms: row.exact_terms,
            sanitized_text: row.sanitized_text,
            logical_path: row.logical_path,
            field_lengths: row.field_lengths,
            normalized_text: row.normalized_text,
        }
    }
}

pub(super) fn manifest_digest<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
            .to_le_bytes(),
    );
    hasher.update(bytes);
    ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn metadata_digest(
    metadata: &CodeLexicalProjectionMetadataV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(b"tracedecay.code-lexical-artifact-metadata.v1\0", metadata)
}

#[allow(clippy::too_many_arguments)] // one committed digest tuple, spelled once
pub(super) fn artifact_digest(
    metadata_digest: &ManifestDigest,
    source_state_digest: &ManifestDigest,
    source_format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: &ManifestDigest,
    source_cumulative_digest: &ManifestDigest,
    sections: &[CodeLexicalArtifactSectionDigestV1],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(
        ARTIFACT_DIGEST_DOMAIN,
        &(
            metadata_digest.as_str(),
            source_state_digest.as_str(),
            source_format_revision,
            page_count,
            total_chunks,
            total_payload_bytes,
            total_imports,
            import_payload_bytes,
            import_dictionary_digest.as_str(),
            source_cumulative_digest.as_str(),
            sections,
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        ),
    )
}

pub(super) fn encode_field(field: LexicalFieldV1) -> Result<String, CodeLexicalArtifactErrorV1> {
    serde_json::to_string(&field)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn padded_receipt(
    receipt: &VerifiedCodeLexicalArtifactV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut bytes = serde_json::to_vec(receipt)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    if bytes.len() > RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact receipt exceeds its fixed reservation".to_owned(),
        ));
    }
    bytes.resize(RECEIPT_RESERVATION_BYTES, 0);
    Ok(bytes)
}

pub(super) fn decode_padded_receipt(
    bytes: &[u8],
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    if bytes.len() != RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt reservation has the wrong length".to_owned(),
        ));
    }
    let end = bytes.iter().position(|byte| *byte == 0).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is missing its reserved zero tail".to_owned(),
        )
    })?;
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt has nonzero bytes after its canonical payload".to_owned(),
        ));
    }
    if end == 0 {
        return Ok(None);
    }
    let receipt = serde_json::from_slice(&bytes[..end])
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if padded_receipt(&receipt)? != bytes {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is not canonically encoded".to_owned(),
        ));
    }
    Ok(Some(receipt))
}

/// Decode a fixed-size receipt while honoring the caller's canonical work
/// control. Reopen paths use this version so a corrupt or cold artifact never
/// turns an expired epoch into an unbounded padding scan.
pub(super) fn decode_padded_receipt_with_control(
    bytes: &[u8],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    super::checkpoint(control)?;
    if bytes.len() != RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt reservation has the wrong length".to_owned(),
        ));
    }
    let mut end = None;
    for (ordinal, byte) in bytes.iter().enumerate() {
        if ordinal.is_multiple_of(1_024) {
            super::checkpoint(control)?;
        }
        if *byte == 0 {
            end = Some(ordinal);
            break;
        }
    }
    let end = end.ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is missing its reserved zero tail".to_owned(),
        )
    })?;
    for (ordinal, byte) in bytes[end..].iter().enumerate() {
        if ordinal.is_multiple_of(1_024) {
            super::checkpoint(control)?;
        }
        if *byte != 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact receipt has nonzero bytes after its canonical payload".to_owned(),
            ));
        }
    }
    if end == 0 {
        return Ok(None);
    }
    super::checkpoint(control)?;
    let receipt = serde_json::from_slice(&bytes[..end])
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    super::checkpoint(control)?;
    if padded_receipt(&receipt)? != bytes {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is not canonically encoded".to_owned(),
        ));
    }
    Ok(Some(receipt))
}

pub(super) fn new_verified_receipt(
    metadata: CodeLexicalProjectionMetadataV1,
    metadata_digest: ManifestDigest,
    source: &tracedecay_code_index::production::VerifiedSealedLexicalSourceReceiptV1,
    artifact_digest: ManifestDigest,
    section_digests: Vec<CodeLexicalArtifactSectionDigestV1>,
    file_size_bytes: u64,
) -> VerifiedCodeLexicalArtifactV1 {
    VerifiedCodeLexicalArtifactV1 {
        format_revision: CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        generation: metadata.generation,
        repository_id: metadata.repository_id,
        freshness: metadata.freshness,
        metadata_digest,
        source_state_digest: source.source_state_digest().clone(),
        source_cumulative_digest: source.cumulative_digest().clone(),
        source_format_revision: source.format_revision(),
        page_count: source.page_count(),
        total_chunks: source.total_chunks(),
        total_payload_bytes: source.total_payload_bytes(),
        total_imports: source.total_imports(),
        import_payload_bytes: source.import_payload_bytes(),
        import_dictionary_digest: source.import_dictionary_digest().clone(),
        artifact_digest,
        section_digests,
        file_size_bytes,
    }
}
