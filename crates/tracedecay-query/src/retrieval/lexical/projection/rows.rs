use std::collections::BTreeMap;

use tracedecay_domain::{CodeSearchChunkGrainV1, CodeSearchChunkV1, ExactTechnicalTermKindV1};

use super::LexicalFieldV1;

#[derive(Clone, Debug)]
pub(super) struct ProjectedChunkV1 {
    pub(super) document: usize,
    pub(super) logical_path: String,
    pub(super) fields: BTreeMap<LexicalFieldV1, Vec<String>>,
    pub(super) normalized_text: String,
}

impl ProjectedChunkV1 {
    pub(super) fn new(document: usize, chunk: &CodeSearchChunkV1, logical_path: String) -> Self {
        let normalized_text = normalize_lexical(chunk.sanitized_text.as_str());
        let mut fields: BTreeMap<LexicalFieldV1, Vec<String>> = BTreeMap::new();
        let text_field = if chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
            LexicalFieldV1::PreambleText
        } else {
            LexicalFieldV1::BodyText
        };
        fields.insert(text_field, lexical_tokens(chunk.sanitized_text.as_str()));
        fields.insert(LexicalFieldV1::Path, vec![normalize_lexical(&logical_path)]);
        fields.insert(
            LexicalFieldV1::Subtoken,
            chunk
                .subtokens
                .iter()
                .map(|term| normalize_lexical(term))
                .collect(),
        );
        for term in &chunk.exact_terms {
            let Ok(canonical) = std::str::from_utf8(term.canonical_bytes()) else {
                continue;
            };
            let canonical = normalize_lexical(canonical);
            fields
                .entry(LexicalFieldV1::ExactTerm)
                .or_default()
                .push(canonical.clone());
            match term.kind() {
                ExactTechnicalTermKindV1::WholeSymbol
                    if matches!(
                        chunk.anchor.grain,
                        CodeSearchChunkGrainV1::SymbolSignature
                            | CodeSearchChunkGrainV1::SymbolMember
                    ) =>
                {
                    fields
                        .entry(LexicalFieldV1::SymbolName)
                        .or_default()
                        .push(canonical);
                }
                ExactTechnicalTermKindV1::QualifiedName => {
                    fields
                        .entry(LexicalFieldV1::QualifiedName)
                        .or_default()
                        .push(canonical);
                }
                ExactTechnicalTermKindV1::Path => {
                    fields
                        .entry(LexicalFieldV1::Path)
                        .or_default()
                        .push(canonical);
                }
                _ => {}
            }
        }
        Self {
            document,
            logical_path,
            fields,
            normalized_text,
        }
    }
}

pub(super) fn normalize_lexical(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn lexical_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| {
            !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.' | '/'))
        })
        .filter(|term| !term.is_empty())
        .map(normalize_lexical)
        .collect()
}
