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
    ts_provider::grammar_str,
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
            let conservative_tokens = strings.encode(&payload.conservative_tokens)?;
            let rename_tokens = payload
                .rename_tokens
                .as_deref()
                .map(|rename| strings.encode_rename(&payload.conservative_tokens, rename))
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
                let conservative_tokens: Arc<[ConservativeCloneTokenV1]> =
                    decode_tokens(&body.conservative_tokens, &strings)?.into();
                let rename_tokens = match body.rename_tokens {
                    None => None,
                    Some(PersistedRenameTokensV1::Conservative) => {
                        Some(Arc::clone(&conservative_tokens))
                    }
                    Some(PersistedRenameTokensV1::Renamed(renamed)) => {
                        let mut tokens = conservative_tokens.to_vec();
                        for [position, text] in renamed {
                            let Some(ConservativeCloneTokenV1::Syntax { text: slot, .. }) =
                                usize::try_from(position)
                                    .ok()
                                    .and_then(|position| tokens.get_mut(position))
                            else {
                                return Err(contract(
                                    "sealed clone rename renames a position that is not a syntax token",
                                ));
                            };
                            *slot = grammar_str(string(&strings, text)?);
                        }
                        Some(tokens.into())
                    }
                    Some(PersistedRenameTokensV1::Tokens(codes)) => {
                        Some(decode_tokens(&codes, &strings)?.into())
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
        tokens: &'a [ConservativeCloneTokenV1],
    ) -> Result<Vec<u32>, CodeIndexProductionErrorV1> {
        let mut codes = Vec::with_capacity(tokens.len());
        let mut open = Vec::new();
        for token in tokens {
            match token {
                ConservativeCloneTokenV1::StructureStart { syntax_kind } => {
                    open.push(syntax_kind.as_ref());
                    codes.push(self.code(syntax_kind, TAG_START)?);
                }
                ConservativeCloneTokenV1::StructureEnd { syntax_kind } => {
                    if open.last() == Some(&syntax_kind.as_ref()) {
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
        conservative: &'a [ConservativeCloneTokenV1],
        rename: &'a [ConservativeCloneTokenV1],
    ) -> Result<PersistedRenameTokensV1, CodeIndexProductionErrorV1> {
        if rename == conservative {
            return Ok(PersistedRenameTokensV1::Conservative);
        }
        if rename.len() != conservative.len() {
            return self.encode(rename).map(PersistedRenameTokensV1::Tokens);
        }
        let mut renamed = Vec::new();
        for (position, (left, right)) in conservative.iter().zip(rename).enumerate() {
            match (left, right) {
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

/// Kinds, and texts that name a grammar node, borrow the grammar's static
/// names: a decoded generation holds tens of millions of tokens, and owning
/// both strings per token made clone streams most of its resident bytes.
fn decode_tokens(
    codes: &[u32],
    strings: &[String],
) -> Result<Vec<ConservativeCloneTokenV1>, CodeIndexProductionErrorV1> {
    let mut tokens = Vec::with_capacity(codes.len());
    let mut open = Vec::new();
    let mut codes = codes.iter().copied();
    while let Some(code) = codes.next() {
        let Some(value) = code.checked_sub(1) else {
            let kind = open.pop().ok_or_else(|| {
                contract("sealed clone token stream closes a structure it never opened")
            })?;
            tokens.push(ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: grammar_str(kind),
            });
            continue;
        };
        let kind = string(strings, value >> 2)?;
        tokens.push(match value & 3 {
            TAG_START => {
                open.push(kind);
                ConservativeCloneTokenV1::StructureStart {
                    syntax_kind: grammar_str(kind),
                }
            }
            TAG_END => ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: grammar_str(kind),
            },
            TAG_SYNTAX_KIND_TEXT => {
                let syntax_kind = grammar_str(kind);
                ConservativeCloneTokenV1::Syntax {
                    text: syntax_kind.clone(),
                    syntax_kind,
                }
            }
            _ => {
                let text = codes
                    .next()
                    .ok_or_else(|| contract("sealed clone syntax token is missing its text"))?;
                ConservativeCloneTokenV1::Syntax {
                    syntax_kind: grammar_str(kind),
                    text: grammar_str(string(strings, text)?),
                }
            }
        });
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn start(kind: &'static str) -> ConservativeCloneTokenV1 {
        ConservativeCloneTokenV1::StructureStart {
            syntax_kind: Cow::Borrowed(kind),
        }
    }

    fn end(kind: &'static str) -> ConservativeCloneTokenV1 {
        ConservativeCloneTokenV1::StructureEnd {
            syntax_kind: Cow::Borrowed(kind),
        }
    }

    fn syntax(kind: &'static str, text: &str) -> ConservativeCloneTokenV1 {
        ConservativeCloneTokenV1::Syntax {
            syntax_kind: Cow::Borrowed(kind),
            text: Cow::Owned(text.to_owned()),
        }
    }

    fn round_trip(tokens: &[ConservativeCloneTokenV1]) -> (Vec<u32>, Vec<String>) {
        let mut table = StringTableV1::default();
        let codes = table.encode(tokens).expect("encode");
        let strings = table
            .strings
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            decode_tokens(&codes, &strings).expect("decode"),
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
    fn decoded_grammar_names_borrow_the_grammar_instead_of_owning_a_copy() {
        let tokens = vec![
            start("block"),
            syntax("identifier", "value"),
            syntax("(", "("),
            syntax("identifier", "fixture_only_identifier"),
            end("block"),
        ];
        let (codes, strings) = round_trip(&tokens);
        let first = decode_tokens(&codes, &strings).expect("decode");
        let second = decode_tokens(&codes, &strings).expect("decode");
        let kind = |token: &ConservativeCloneTokenV1| match token {
            ConservativeCloneTokenV1::StructureStart { syntax_kind }
            | ConservativeCloneTokenV1::StructureEnd { syntax_kind }
            | ConservativeCloneTokenV1::Syntax { syntax_kind, .. } => syntax_kind.clone(),
        };
        for (left, right) in first.iter().zip(&second) {
            let (Cow::Borrowed(left), Cow::Borrowed(right)) = (kind(left), kind(right)) else {
                panic!("a grammar kind must decode as the grammar's static name");
            };
            assert!(std::ptr::eq(left, right), "both decodes share one name");
        }
        let texts = first
            .iter()
            .filter_map(|token| match token {
                ConservativeCloneTokenV1::Syntax { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            matches!(texts[1], Cow::Borrowed("(")),
            "punctuation borrows"
        );
        assert!(
            matches!(texts[2], Cow::Owned(text) if text == "fixture_only_identifier"),
            "a name no grammar declares stays owned"
        );
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
