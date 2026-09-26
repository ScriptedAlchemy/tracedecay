//! Persisted clone-body rows of one file segment.
//!
//! Token streams dominate a file segment when every token is its own JSON
//! object carrying its syntax kind and text. This row form interns each
//! syntax kind and token text of the file once and writes every stream as
//! integer codes. A rename stream agrees with its conservative stream token
//! for token except where an identifier was renamed, so it is written as
//! those renamed positions. Payload digests and the occurrence fields the
//! file authority already fixes (project, repository, worktree, source
//! generation, path) are not stored: restore recomputes the digests from the
//! tokens and rebinds the occurrence to the authority it is validated
//! against.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_code_extraction::{
    CloneBodyRenameIssueV1, CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1,
    CloneTokenStreamV1,
};
use tracedecay_domain::{ManifestDigest, SourceSpan, SymbolOccurrenceId};

use super::CodeIndexProductionErrorV1;
use crate::clones::{
    CloneBodyEligibilityV1, CloneBodyOccurrenceV1, CloneBodyPayloadPartsV1, CloneBodyPayloadV1,
    CloneBodyRenameStatusV1, CodeIndexCloneBodyV1, ConservativeCloneTokenV1,
};
use crate::extract::ExtractionBatchV1;
use crate::intake::ReceiptBoundCodeFileAuthorityV1;

/// Token codes. `0` closes the innermost open structure; any other code `c`
/// names string `(c - 1) >> 2` as the syntax kind and carries tag
/// `(c - 1) & 3`. A syntax token whose text differs from its kind is followed
/// by the string index of its text.
const CLOSE_INNERMOST: u32 = 0;
const TAG_START: u32 = 0;
const TAG_END: u32 = 1;
const TAG_SYNTAX_KIND_TEXT: u32 = 2;
const TAG_SYNTAX: u32 = 3;

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

#[derive(Serialize)]
pub(super) struct PersistedCloneBodiesRefV1<'a> {
    /// The first body's language and snapshot digest; a body carries its own
    /// only where it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_digest: Option<&'a ManifestDigest>,
    strings: Vec<&'a str>,
    bodies: Vec<PersistedCloneBodyRefV1<'a>>,
}

#[derive(Serialize)]
struct PersistedCloneBodyRefV1<'a> {
    symbol_occurrence_id: &'a SymbolOccurrenceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_digest: Option<&'a ManifestDigest>,
    symbol_kind: &'a str,
    body_span: SourceSpan,
    eligibility: CloneBodyEligibilityV1,
    token_count: u32,
    conservative_normalization_revision: u16,
    tokenization_status: CloneBodyTokenizationStatusV1,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tokenization_issues: &'a [CloneBodyTokenizationIssueV1],
    conservative_tokens: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_normalization_revision: Option<u16>,
    rename_coverage: CloneBodyRenameStatusV1,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    rename_issues: &'a [CloneBodyRenameIssueV1],
    #[serde(skip_serializing_if = "Option::is_none")]
    rename_tokens: Option<PersistedRenameTokensV1>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum PersistedRenameTokensV1 {
    /// The conservative stream itself.
    Conservative,
    /// The conservative stream with `[position, text]` replacements.
    Renamed(Vec<[u32; 2]>),
    /// A stream that does not align with the conservative one.
    Tokens(Vec<u32>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedCloneBodiesV1 {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    snapshot_digest: Option<ManifestDigest>,
    strings: Vec<String>,
    bodies: Vec<PersistedCloneBodyV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCloneBodyV1 {
    symbol_occurrence_id: SymbolOccurrenceId,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    snapshot_digest: Option<ManifestDigest>,
    symbol_kind: String,
    body_span: SourceSpan,
    eligibility: CloneBodyEligibilityV1,
    token_count: u32,
    conservative_normalization_revision: u16,
    tokenization_status: CloneBodyTokenizationStatusV1,
    #[serde(default)]
    tokenization_issues: Vec<CloneBodyTokenizationIssueV1>,
    conservative_tokens: Vec<u32>,
    #[serde(default)]
    rename_normalization_revision: Option<u16>,
    rename_coverage: CloneBodyRenameStatusV1,
    #[serde(default)]
    rename_issues: Vec<CloneBodyRenameIssueV1>,
    #[serde(default)]
    rename_tokens: Option<PersistedRenameTokensV1>,
}

impl<'a> PersistedCloneBodiesRefV1<'a> {
    /// Refuses a body whose occurrence disagrees with the file authority it
    /// would be rebound to on restore, rather than persisting a row that
    /// restores differently. Rows and the string table follow `bodies` order,
    /// so a caller that wants worktree-independent bytes passes an order that
    /// does not depend on occurrence identities.
    pub(super) fn new(
        authority: &ReceiptBoundCodeFileAuthorityV1,
        extraction: &ExtractionBatchV1,
        bodies: &[&'a CodeIndexCloneBodyV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let language = bodies.first().map(|body| body.payload.language.as_str());
        let snapshot_digest = bodies.first().map(|body| &body.occurrence.snapshot_digest);
        let mut strings = StringTableV1::default();
        let mut rows = Vec::with_capacity(bodies.len());
        for &body in bodies {
            let occurrence = &body.occurrence;
            let payload = &*body.payload;
            if occurrence.project_id != authority.project_id
                || occurrence.repository_id != authority.repository_id
                || occurrence.worktree_id != authority.worktree_id
                || occurrence.path != authority.logical_path
                || occurrence.source_generation != extraction.generation_id
                || occurrence.payload_digest != payload.payload_digest
            {
                return Err(contract(
                    "sealed clone body occurrence disagrees with its file authority",
                ));
            }
            let conservative = payload.conservative_tokens.iter().collect::<Vec<_>>();
            let conservative_tokens = strings.encode(&conservative)?;
            let rename_tokens = payload
                .rename_tokens
                .as_ref()
                .map(|rename| {
                    strings.encode_rename(&conservative, &rename.iter().collect::<Vec<_>>())
                })
                .transpose()?;
            rows.push(PersistedCloneBodyRefV1 {
                symbol_occurrence_id: &occurrence.symbol_occurrence_id,
                language: (Some(payload.language.as_str()) != language)
                    .then_some(payload.language.as_str()),
                snapshot_digest: (Some(&occurrence.snapshot_digest) != snapshot_digest)
                    .then_some(&occurrence.snapshot_digest),
                symbol_kind: &payload.symbol_kind,
                body_span: occurrence.body_span,
                eligibility: occurrence.eligibility,
                token_count: payload.token_count,
                conservative_normalization_revision: payload.conservative_normalization_revision,
                tokenization_status: payload.tokenization_status,
                tokenization_issues: &payload.tokenization_issues,
                conservative_tokens,
                rename_normalization_revision: payload.rename_normalization_revision,
                rename_coverage: payload.rename_coverage,
                rename_issues: &payload.rename_issues,
                rename_tokens,
            });
        }
        Ok(Self {
            language,
            snapshot_digest,
            strings: strings.strings,
            bodies: rows,
        })
    }
}

impl PersistedCloneBodiesV1 {
    pub(super) fn expand(
        self,
        authority: &ReceiptBoundCodeFileAuthorityV1,
        extraction: &ExtractionBatchV1,
    ) -> Result<Vec<CodeIndexCloneBodyV1>, CodeIndexProductionErrorV1> {
        let Self {
            language,
            snapshot_digest,
            strings,
            bodies,
        } = self;
        bodies
            .into_iter()
            .map(|body| {
                let conservative_tokens = decode_tokens(&body.conservative_tokens, &strings)?;
                let rename_tokens = match body.rename_tokens {
                    None => None,
                    Some(PersistedRenameTokensV1::Conservative) => {
                        Some(conservative_tokens.clone())
                    }
                    Some(PersistedRenameTokensV1::Renamed(renamed)) => {
                        let renamed = renamed
                            .iter()
                            .map(|[position, text]| Ok((*position, string(&strings, *text)?)))
                            .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()?;
                        Some(conservative_tokens.renamed(renamed).map_err(|_| {
                            contract(
                                "sealed clone rename renames a position that is not a syntax token",
                            )
                        })?)
                    }
                    Some(PersistedRenameTokensV1::Tokens(codes)) => {
                        Some(decode_tokens(&codes, &strings)?)
                    }
                };
                let payload = CloneBodyPayloadV1::from_parts(CloneBodyPayloadPartsV1 {
                    language: body.language.or_else(|| language.clone()).ok_or_else(|| {
                        contract("sealed clone body omits its language without a file default")
                    })?,
                    symbol_kind: body.symbol_kind,
                    token_count: body.token_count,
                    conservative_normalization_revision: body.conservative_normalization_revision,
                    conservative_tokens,
                    tokenization_status: body.tokenization_status,
                    tokenization_issues: body.tokenization_issues,
                    rename_normalization_revision: body.rename_normalization_revision,
                    rename_tokens,
                    rename_coverage: body.rename_coverage,
                    rename_issues: body.rename_issues,
                })
                .map_err(CodeIndexProductionErrorV1::Contract)?;
                Ok(CodeIndexCloneBodyV1 {
                    occurrence: CloneBodyOccurrenceV1 {
                        project_id: authority.project_id.clone(),
                        repository_id: authority.repository_id.clone(),
                        worktree_id: authority.worktree_id.clone(),
                        source_generation: extraction.generation_id.clone(),
                        snapshot_digest: body
                            .snapshot_digest
                            .or_else(|| snapshot_digest.clone())
                            .ok_or_else(|| {
                                contract(
                                    "sealed clone body omits its snapshot digest without a file default",
                                )
                            })?,
                        symbol_occurrence_id: body.symbol_occurrence_id,
                        path: authority.logical_path.clone(),
                        body_span: body.body_span,
                        payload_digest: payload.payload_digest.clone(),
                        eligibility: body.eligibility,
                    },
                    payload: Arc::new(payload),
                })
            })
            .collect()
    }
}

#[derive(Default)]
struct StringTableV1<'a> {
    strings: Vec<&'a str>,
    index: HashMap<&'a str, u32>,
}

impl<'a> StringTableV1<'a> {
    fn intern(&mut self, value: &'a str) -> Result<u32, CodeIndexProductionErrorV1> {
        if let Some(index) = self.index.get(value) {
            return Ok(*index);
        }
        let index = u32::try_from(self.strings.len())
            .map_err(|_| contract("sealed clone string table exceeds u32"))?;
        self.strings.push(value);
        self.index.insert(value, index);
        Ok(index)
    }

    fn code(&mut self, kind: &'a str, tag: u32) -> Result<u32, CodeIndexProductionErrorV1> {
        self.intern(kind)?
            .checked_mul(4)
            .and_then(|code| code.checked_add(tag + 1))
            .ok_or_else(|| contract("sealed clone token code exceeds u32"))
    }

    fn encode(
        &mut self,
        tokens: &[ConservativeCloneTokenV1<'a>],
    ) -> Result<Vec<u32>, CodeIndexProductionErrorV1> {
        let mut codes = Vec::with_capacity(tokens.len());
        let mut open = Vec::new();
        for token in tokens.iter().copied() {
            match token {
                ConservativeCloneTokenV1::StructureStart { syntax_kind } => {
                    open.push(syntax_kind);
                    codes.push(self.code(syntax_kind, TAG_START)?);
                }
                ConservativeCloneTokenV1::StructureEnd { syntax_kind } => {
                    if open.last() == Some(&syntax_kind) {
                        open.pop();
                        codes.push(CLOSE_INNERMOST);
                    } else {
                        codes.push(self.code(syntax_kind, TAG_END)?);
                    }
                }
                ConservativeCloneTokenV1::Syntax { syntax_kind, text } => {
                    if text == syntax_kind {
                        codes.push(self.code(syntax_kind, TAG_SYNTAX_KIND_TEXT)?);
                    } else {
                        codes.push(self.code(syntax_kind, TAG_SYNTAX)?);
                        codes.push(self.intern(text)?);
                    }
                }
            }
        }
        Ok(codes)
    }

    fn encode_rename(
        &mut self,
        conservative: &[ConservativeCloneTokenV1<'a>],
        rename: &[ConservativeCloneTokenV1<'a>],
    ) -> Result<PersistedRenameTokensV1, CodeIndexProductionErrorV1> {
        if rename == conservative {
            return Ok(PersistedRenameTokensV1::Conservative);
        }
        if rename.len() != conservative.len() {
            return self.encode(rename).map(PersistedRenameTokensV1::Tokens);
        }
        let mut renamed = Vec::new();
        for (position, (left, right)) in conservative.iter().zip(rename).enumerate() {
            match (*left, *right) {
                _ if left == right => {}
                (
                    ConservativeCloneTokenV1::Syntax {
                        syntax_kind: left_kind,
                        ..
                    },
                    ConservativeCloneTokenV1::Syntax { syntax_kind, text },
                ) if left_kind == syntax_kind => {
                    let position = u32::try_from(position)
                        .map_err(|_| contract("sealed clone rename position exceeds u32"))?;
                    renamed.push([position, self.intern(text)?]);
                }
                _ => return self.encode(rename).map(PersistedRenameTokensV1::Tokens),
            }
        }
        Ok(PersistedRenameTokensV1::Renamed(renamed))
    }
}

fn string(strings: &[String], index: u32) -> Result<&str, CodeIndexProductionErrorV1> {
    usize::try_from(index)
        .ok()
        .and_then(|index| strings.get(index))
        .map(String::as_str)
        .ok_or_else(|| contract("sealed clone token names a string outside its table"))
}

fn decode_tokens(
    codes: &[u32],
    strings: &[String],
) -> Result<CloneTokenStreamV1, CodeIndexProductionErrorV1> {
    let mut tokens = Vec::with_capacity(codes.len());
    let mut open = Vec::new();
    let mut codes = codes.iter().copied();
    while let Some(code) = codes.next() {
        let Some(value) = code.checked_sub(1) else {
            let kind = open.pop().ok_or_else(|| {
                contract("sealed clone token stream closes a structure it never opened")
            })?;
            tokens.push(ConservativeCloneTokenV1::StructureEnd { syntax_kind: kind });
            continue;
        };
        let kind = string(strings, value >> 2)?;
        tokens.push(match value & 3 {
            TAG_START => {
                open.push(kind);
                ConservativeCloneTokenV1::StructureStart { syntax_kind: kind }
            }
            TAG_END => ConservativeCloneTokenV1::StructureEnd { syntax_kind: kind },
            TAG_SYNTAX_KIND_TEXT => ConservativeCloneTokenV1::Syntax {
                syntax_kind: kind,
                text: kind,
            },
            _ => {
                let text = codes
                    .next()
                    .ok_or_else(|| contract("sealed clone syntax token is missing its text"))?;
                ConservativeCloneTokenV1::Syntax {
                    syntax_kind: kind,
                    text: string(strings, text)?,
                }
            }
        });
    }
    CloneTokenStreamV1::from_tokens(tokens)
        .map_err(|_| contract("sealed clone token stream exceeds its code space"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(syntax_kind: &'static str) -> ConservativeCloneTokenV1<'static> {
        ConservativeCloneTokenV1::StructureStart { syntax_kind }
    }

    fn end(syntax_kind: &'static str) -> ConservativeCloneTokenV1<'static> {
        ConservativeCloneTokenV1::StructureEnd { syntax_kind }
    }

    fn syntax(syntax_kind: &'static str, text: &'static str) -> ConservativeCloneTokenV1<'static> {
        ConservativeCloneTokenV1::Syntax { syntax_kind, text }
    }

    fn round_trip(tokens: &[ConservativeCloneTokenV1<'static>]) -> (Vec<u32>, Vec<String>) {
        let mut table = StringTableV1::default();
        let codes = table.encode(tokens).expect("encode");
        let strings = table
            .strings
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            decode_tokens(&codes, &strings)
                .expect("decode")
                .iter()
                .collect::<Vec<_>>(),
            tokens,
            "every token stream must restore exactly"
        );
        (codes, strings)
    }

    #[test]
    fn token_streams_restore_exactly_without_storing_kind_equal_text() {
        let tokens = vec![
            start("block"),
            start("let_declaration"),
            syntax("let", "let"),
            syntax("identifier", "value"),
            syntax("=", "="),
            syntax("identifier", "value"),
            end("let_declaration"),
            end("block"),
        ];
        let (codes, strings) = round_trip(&tokens);
        assert_eq!(
            strings,
            [
                "block",
                "let_declaration",
                "let",
                "identifier",
                "value",
                "="
            ],
            "each kind and text is interned once"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == CLOSE_INNERMOST)
                .count(),
            2
        );
        assert_eq!(
            codes.len(),
            tokens.len() + 2,
            "only differing texts add a code"
        );
    }

    #[test]
    fn decoded_grammar_kinds_hold_no_text_bytes() {
        let tokens = vec![
            start("block"),
            syntax("identifier", "value"),
            syntax("(", "("),
            syntax("identifier", "fixture_only_identifier"),
            end("block"),
        ];
        let (codes, strings) = round_trip(&tokens);
        let decoded = decode_tokens(&codes, &strings).expect("decode");

        // A 56-byte header, five token codes and two text codes, and only the
        // two identifier texts (28 bytes) with their ends: `block`,
        // `identifier` and `(` resolve to grammar kind numbers.
        assert_eq!(decoded.token_retained_bytes(), 56 + 7 * 4 + 28 + 2 * 4);
    }

    #[test]
    fn unbalanced_structure_markers_restore_exactly() {
        round_trip(&[
            end("orphan"),
            start("outer"),
            start("inner"),
            end("outer"),
            end("inner"),
            syntax("identifier", "tail"),
        ]);
    }

    #[test]
    fn damaged_streams_are_refused() {
        let strings = vec!["identifier".to_owned()];
        assert!(decode_tokens(&[CLOSE_INNERMOST], &strings).is_err());
        assert!(decode_tokens(&[1 + TAG_SYNTAX], &strings).is_err());
        assert!(decode_tokens(&[1 + (5 << 2)], &strings).is_err());
    }

    #[test]
    fn rename_streams_keep_only_renamed_positions() {
        let conservative = vec![
            syntax("identifier", "input"),
            syntax("(", "("),
            syntax("identifier", "input"),
        ];
        let mut table = StringTableV1::default();
        assert_eq!(
            table
                .encode_rename(&conservative, &conservative)
                .expect("rename"),
            PersistedRenameTokensV1::Conservative
        );
        let renamed = vec![
            syntax("identifier", "$0"),
            syntax("(", "("),
            syntax("identifier", "$0"),
        ];
        let PersistedRenameTokensV1::Renamed(pairs) = table
            .encode_rename(&conservative, &renamed)
            .expect("rename")
        else {
            panic!("an aligned rename keeps only its renamed positions");
        };
        assert_eq!(
            pairs
                .iter()
                .map(|[position, _]| *position)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        let misaligned = vec![syntax("(", "("), syntax("identifier", "$0")];
        assert!(matches!(
            table
                .encode_rename(&conservative, &misaligned)
                .expect("rename"),
            PersistedRenameTokensV1::Tokens(_)
        ));
    }
}
