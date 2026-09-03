use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, ExactTechnicalTermV1, FileOccurrenceId, LanguageDescriptorRevision,
    SourceSpan, SymbolOccurrenceId,
};

use super::super::LexicalFieldV1;
use super::CodeLexicalArtifactErrorV1;
use super::format::ArtifactRowV1;
use super::schema::LexicalArtifactLayoutV1;

const ROW_CODEC_V11_MAGIC: &[u8] = b"TDLR11\0";

/// Compact row payload: drop identities already stored as columns or
/// generation metadata, and reconstruct ASCII-normalized text on read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactRowCompactV11 {
    file_occurrence_id: FileOccurrenceId,
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
    parent_chunk_id: Option<CodeSearchChunkId>,
    source_span: SourceSpan,
    grain: CodeSearchChunkGrainV1,
    ordinal: u32,
    language_descriptor_revision: LanguageDescriptorRevision,
    exact_terms: Vec<ExactTechnicalTermV1>,
    sanitized_text: BoundedSanitizedText,
    logical_path: String,
    symbol_simple_name: Option<String>,
    symbol_qualified_name: Option<String>,
    symbol_kind: Option<String>,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
}

pub(super) fn encode_artifact_row(
    layout: LexicalArtifactLayoutV1,
    row: &ArtifactRowV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    match layout {
        LexicalArtifactLayoutV1::V10 => serde_json::to_vec(row)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string())),
        LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => encode_compact_v11(row),
    }
}

fn encode_compact_v11(row: &ArtifactRowV1) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let compact = ArtifactRowCompactV11 {
        file_occurrence_id: row.anchor.file_occurrence_id.clone(),
        symbol_occurrence_id: row.anchor.symbol_occurrence_id.clone(),
        parent_chunk_id: row.anchor.parent_chunk_id.clone(),
        source_span: row.anchor.source_span,
        grain: row.anchor.grain,
        ordinal: row.anchor.ordinal,
        language_descriptor_revision: row.language_descriptor_revision.clone(),
        exact_terms: row.exact_terms.clone(),
        sanitized_text: row.sanitized_text.clone(),
        logical_path: row.logical_path.clone(),
        symbol_simple_name: row.symbol_simple_name.clone(),
        symbol_qualified_name: row.symbol_qualified_name.clone(),
        symbol_kind: row.symbol_kind.clone(),
        field_lengths: row.field_lengths.clone(),
    };
    let payload = serde_json::to_vec(&compact)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut bytes = Vec::with_capacity(ROW_CODEC_V11_MAGIC.len().saturating_add(payload.len()));
    bytes.extend_from_slice(ROW_CODEC_V11_MAGIC);
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub(super) fn decode_artifact_row(
    layout: LexicalArtifactLayoutV1,
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    match layout {
        LexicalArtifactLayoutV1::V10 => decode_json_v10(generation, chunk_id, bytes),
        LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
            decode_compact_v11(generation, chunk_id, bytes)
        }
    }
}

fn decode_json_v10(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let row: ArtifactRowV1 = serde_json::from_slice(bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if row.id.as_str() != chunk_id || &row.anchor.generation_id != generation {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row identity does not match its stored coordinates".to_owned(),
        ));
    }
    Ok(row)
}

fn decode_compact_v11(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let payload = bytes.strip_prefix(ROW_CODEC_V11_MAGIC).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row is missing the compact v11 codec tag".to_owned(),
        )
    })?;
    let compact: ArtifactRowCompactV11 = serde_json::from_slice(payload)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let id = CodeSearchChunkId::new(chunk_id.to_owned())
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let normalized_text = compact.sanitized_text.as_str().to_ascii_lowercase();
    Ok(ArtifactRowV1 {
        id,
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: compact.file_occurrence_id,
            symbol_occurrence_id: compact.symbol_occurrence_id,
            parent_chunk_id: compact.parent_chunk_id,
            source_span: compact.source_span,
            grain: compact.grain,
            ordinal: compact.ordinal,
        },
        language_descriptor_revision: compact.language_descriptor_revision,
        exact_terms: compact.exact_terms,
        sanitized_text: compact.sanitized_text,
        logical_path: compact.logical_path,
        symbol_simple_name: compact.symbol_simple_name,
        symbol_qualified_name: compact.symbol_qualified_name,
        symbol_kind: compact.symbol_kind,
        field_lengths: compact.field_lengths,
        normalized_text,
    })
}

#[cfg(test)]
mod tests {
    use super::{ArtifactRowV1, LexicalArtifactLayoutV1, decode_artifact_row, encode_artifact_row};
    use crate::retrieval::lexical::LexicalFieldV1;
    use std::collections::BTreeMap;
    use tracedecay_domain::{
        BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
        CodeSearchChunkId, FileOccurrenceId, LanguageDescriptorRevision, SourceSpan,
    };

    fn sample_row() -> ArtifactRowV1 {
        let sanitized =
            BoundedSanitizedText::new("fn RenderWidget() { return; }").expect("bounded text");
        ArtifactRowV1 {
            id: CodeSearchChunkId::new("chunk.sample").expect("chunk"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: CodeGenerationId::new("gen.sample").expect("generation"),
                file_occurrence_id: FileOccurrenceId::new("file.sample").expect("file"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 8,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            language_descriptor_revision: LanguageDescriptorRevision::new("lang.1")
                .expect("language"),
            exact_terms: Vec::new(),
            sanitized_text: sanitized.clone(),
            logical_path: "src/sample.rs".to_owned(),
            symbol_simple_name: Some("RenderWidget".to_owned()),
            symbol_qualified_name: Some("sample::RenderWidget".to_owned()),
            symbol_kind: Some("function".to_owned()),
            field_lengths: BTreeMap::from([(LexicalFieldV1::BodyText, 4)]),
            normalized_text: sanitized.as_str().to_ascii_lowercase(),
        }
    }

    #[test]
    fn compact_v11_round_trip_is_byte_equivalent_to_logical_row() {
        let row = sample_row();
        let encoded = encode_artifact_row(LexicalArtifactLayoutV1::V11, &row).expect("encode");
        assert!(
            encoded.starts_with(b"TDLR11\0"),
            "v11 rows must carry the compact codec tag"
        );
        let decoded = decode_artifact_row(
            LexicalArtifactLayoutV1::V11,
            &row.anchor.generation_id,
            row.id.as_str(),
            &encoded,
        )
        .expect("decode");
        assert_eq!(decoded, row);
        assert!(
            encoded.len() < serde_json::to_vec(&row).expect("json").len(),
            "compact rows must drop repeated identities"
        );
    }

    #[test]
    fn compact_decoder_fails_closed_without_the_v11_tag() {
        let row = sample_row();
        let json = serde_json::to_vec(&row).expect("json");
        let error = decode_artifact_row(
            LexicalArtifactLayoutV1::V11,
            &row.anchor.generation_id,
            row.id.as_str(),
            &json,
        )
        .expect_err("untagged JSON is not a v11 row");
        assert!(error.to_string().contains("compact v11"));
    }
}
