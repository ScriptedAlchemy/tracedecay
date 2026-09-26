//! Binary clone records.
//!
//! A payload stores its canonical fields without their digests (a pure
//! function of those fields, re-derived on decode and checked against the
//! row's content address), each syntax kind once in a per-payload table,
//! and its rename stream as its difference from the conservative stream,
//! which it aligns with token for token. The whole record is deflated.
//! An occurrence stores only content: the columns carry its symbol, path,
//! span, and payload, the blob its eligibility, and the opening route
//! supplies project, repository, worktree, generation, and snapshot.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use tracedecay_code_index::clones::{
    CloneBodyEligibilityV1, CloneBodyOccurrenceV1, CloneBodyPayloadPartsV1, CloneBodyPayloadV1,
    CloneBodyRenameIssueV1, CloneBodyRenameStatusV1, CloneBodyTokenizationIssueV1,
    CloneBodyTokenizationStatusV1, CodeIndexCloneBodyV1, ConservativeCloneTokenV1,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SourceSpan, SymbolOccurrenceId,
    WorktreeId,
};

use super::format::{contract_number, deflate_bytes, encode_varint, inflate_bytes, take_varint};
use super::row_codec::symbol_id_from_key;
use super::{CodeLexicalArtifactErrorV1, sqlite_error};

const CLONE_PAYLOAD_DEFLATE: u8 = 2;
/// Bound on one stored clone payload once inflated; a body's token streams
/// stay far below it.
const CLONE_PAYLOAD_MAX_INFLATED_BYTES: usize = 64 * 1024 * 1024;

const TOKEN_STRUCTURE_START: u64 = 0;
const TOKEN_SYNTAX: u64 = 1;
const TOKEN_STRUCTURE_END: u64 = 2;

const RENAME_ABSENT: u8 = 0;
const RENAME_ALIGNED: u8 = 1;
const RENAME_STREAM: u8 = 2;
const RENAME_TOKEN_SAME: u8 = 0;
const RENAME_TOKEN_TEXT: u8 = 1;

/// The route identity every clone occurrence an opened artifact serves
/// carries: the opener's, never the building worktree's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CloneOccurrenceRouteV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub source_generation: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
}

impl CloneOccurrenceRouteV1 {
    /// Whether `occurrence` names this route's project, repository, and
    /// worktree; generation and snapshot are the route's to assign.
    pub(super) fn owns(&self, occurrence: &CloneBodyOccurrenceV1) -> bool {
        occurrence.project_id == self.project_id
            && occurrence.repository_id == self.repository_id
            && occurrence.worktree_id == self.worktree_id
    }

    /// Rebuild one stored occurrence and its payload under this route.
    pub(super) fn clone_body(
        &self,
        stored: StoredCloneBodyRowV1,
    ) -> Result<CodeIndexCloneBodyV1, CodeLexicalArtifactErrorV1> {
        let (occurrence, payload) = self.occurrence_and_payload(stored)?;
        Ok(CodeIndexCloneBodyV1 {
            payload: Arc::new(payload),
            occurrence,
        })
    }

    pub(super) fn occurrence_and_payload(
        &self,
        (stored, payload): StoredCloneBodyRowV1,
    ) -> Result<(CloneBodyOccurrenceV1, CloneBodyPayloadV1), CodeLexicalArtifactErrorV1> {
        let payload = payload.ok_or_else(|| corrupt("occurrence is missing its payload"))?;
        let occurrence = self.occurrence(stored)?;
        let payload = decode_clone_payload(&payload, occurrence.payload_digest.as_str())?;
        Ok((occurrence, payload))
    }

    /// Rebuild one stored occurrence under this route.
    pub(super) fn occurrence(
        &self,
        stored: StoredCloneOccurrenceV1,
    ) -> Result<CloneBodyOccurrenceV1, CodeLexicalArtifactErrorV1> {
        let payload_digest = stored
            .payload_digest
            .ok_or_else(|| corrupt("occurrence is missing its payload"))?;
        let corrupt = |error: tracedecay_domain::DomainError| {
            CodeLexicalArtifactErrorV1::Corrupt(error.to_string())
        };
        let body_span = SourceSpan {
            start_byte: u64::try_from(stored.body_start).map_err(contract_number)?,
            end_byte: u64::try_from(stored.body_end).map_err(contract_number)?,
        };
        body_span.validate().map_err(corrupt)?;
        Ok(CloneBodyOccurrenceV1 {
            project_id: self.project_id.clone(),
            repository_id: self.repository_id.clone(),
            worktree_id: self.worktree_id.clone(),
            source_generation: self.source_generation.clone(),
            snapshot_digest: self.snapshot_digest.clone(),
            symbol_occurrence_id: SymbolOccurrenceId::new(symbol_id_from_key(
                (&stored.symbol_key).into(),
            )?)
            .map_err(corrupt)?,
            path: stored.path,
            body_span,
            payload_digest: digest_from_key(&payload_digest)?,
            eligibility: decode_clone_eligibility(&stored.eligibility)?,
        })
    }
}

/// One `clone_occurrences` row's content columns with its payload's
/// digest, selected in the order `symbol_key, payload.payload_digest, path,
/// body_start, body_end, eligibility`.
pub(super) struct StoredCloneOccurrenceV1 {
    pub symbol_key: rusqlite::types::Value,
    /// `None` when the left-joined payload row is absent.
    pub payload_digest: Option<Vec<u8>>,
    pub path: String,
    pub body_start: i64,
    pub body_end: i64,
    pub eligibility: Vec<u8>,
}

impl StoredCloneOccurrenceV1 {
    pub(super) fn read(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            symbol_key: row.get(first)?,
            payload_digest: row.get(first + 1)?,
            path: row.get(first + 2)?,
            body_start: row.get(first + 3)?,
            body_end: row.get(first + 4)?,
            eligibility: row.get(first + 5)?,
        })
    }
}

/// The occurrence columns at `first` of a row that left-joins them, `None`
/// when the join found no occurrence.
pub(super) fn stored_clone_occurrence(
    row: &rusqlite::Row<'_>,
    first: usize,
) -> Result<Option<StoredCloneOccurrenceV1>, CodeLexicalArtifactErrorV1> {
    if row.get_ref(first).map_err(sqlite_error)? == rusqlite::types::ValueRef::Null {
        return Ok(None);
    }
    StoredCloneOccurrenceV1::read(row, first)
        .map(Some)
        .map_err(sqlite_error)
}

/// An occurrence's columns followed by its left-joined payload.
pub(super) type StoredCloneBodyRowV1 = (StoredCloneOccurrenceV1, Option<Vec<u8>>);

pub(super) fn routed_clone_body_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredCloneBodyRowV1> {
    Ok((StoredCloneOccurrenceV1::read(row, 0)?, row.get(6)?))
}

/// The 32 bytes a `sha256:` manifest digest stores as.
pub(super) fn digest_key(digest: &ManifestDigest) -> Result<[u8; 32], CodeLexicalArtifactErrorV1> {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .and_then(|hex_digest| hex::decode(hex_digest).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "clone digest {} is not a SHA-256 manifest digest",
                digest.as_str()
            ))
        })
}

/// Inverse of [`digest_key`].
pub(super) fn digest_from_key(key: &[u8]) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    if key.len() != 32 {
        return Err(corrupt("digest is not 32 bytes"));
    }
    ManifestDigest::from_sha256_bytes(key)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
}

pub(super) fn encode_clone_eligibility(eligibility: CloneBodyEligibilityV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(1);
    match eligibility {
        CloneBodyEligibilityV1::Eligible => encoded.push(0),
        CloneBodyEligibilityV1::ExcludedIncompleteTokenization => encoded.push(1),
        CloneBodyEligibilityV1::ExcludedTooSmall { minimum_tokens } => {
            encoded.push(2);
            encode_varint(u64::from(minimum_tokens), &mut encoded);
        }
        CloneBodyEligibilityV1::ExcludedTooLarge {
            maximum_tokens,
            maximum_bytes,
        } => {
            encoded.push(3);
            encode_varint(u64::from(maximum_tokens), &mut encoded);
            encode_varint(maximum_bytes, &mut encoded);
        }
    }
    encoded
}

pub(super) fn decode_clone_eligibility(
    mut bytes: &[u8],
) -> Result<CloneBodyEligibilityV1, CodeLexicalArtifactErrorV1> {
    let eligibility = match take_u8(&mut bytes)? {
        0 => CloneBodyEligibilityV1::Eligible,
        1 => CloneBodyEligibilityV1::ExcludedIncompleteTokenization,
        2 => CloneBodyEligibilityV1::ExcludedTooSmall {
            minimum_tokens: take_u32(&mut bytes)?,
        },
        3 => CloneBodyEligibilityV1::ExcludedTooLarge {
            maximum_tokens: take_u32(&mut bytes)?,
            maximum_bytes: take_varint(&mut bytes)?,
        },
        _ => return Err(corrupt("eligibility has an unknown tag")),
    };
    if !bytes.is_empty() {
        return Err(corrupt("eligibility has trailing bytes"));
    }
    Ok(eligibility)
}

/// Encode `payload` and return the stored bytes with their inflated length.
pub(super) fn encode_clone_payload(
    payload: &CloneBodyPayloadV1,
) -> Result<(Vec<u8>, usize), CodeLexicalArtifactErrorV1> {
    let mut encoded = Vec::with_capacity(payload.conservative_tokens.len() * 4 + 64);
    put_str(&mut encoded, &payload.language);
    put_str(&mut encoded, &payload.symbol_kind);
    encode_varint(u64::from(payload.token_count), &mut encoded);
    encode_varint(
        u64::from(payload.conservative_normalization_revision),
        &mut encoded,
    );
    encoded.push(match payload.tokenization_status {
        CloneBodyTokenizationStatusV1::Complete => 0,
        CloneBodyTokenizationStatusV1::Partial => 1,
    });
    put_len(&mut encoded, payload.tokenization_issues.len())?;
    for issue in &payload.tokenization_issues {
        encoded.push(match issue {
            CloneBodyTokenizationIssueV1::BodyBoundaryUnavailable => 0,
            CloneBodyTokenizationIssueV1::BodyExceedsSizeBound => 1,
            CloneBodyTokenizationIssueV1::InvalidSourceRange => 2,
            CloneBodyTokenizationIssueV1::ParseError => 3,
        });
    }
    encode_varint(
        payload
            .rename_normalization_revision
            .map_or(0, |revision| u64::from(revision) + 1),
        &mut encoded,
    );
    encoded.push(match payload.rename_coverage {
        CloneBodyRenameStatusV1::Complete => 0,
        CloneBodyRenameStatusV1::Partial => 1,
        CloneBodyRenameStatusV1::UnsupportedLanguage => 2,
    });
    put_len(&mut encoded, payload.rename_issues.len())?;
    for issue in &payload.rename_issues {
        encoded.push(match issue {
            CloneBodyRenameIssueV1::DynamicBinding => 0,
            CloneBodyRenameIssueV1::UnsupportedBindingSyntax => 1,
        });
    }

    let rename = payload.rename_tokens.as_deref();
    let mut kinds = SyntaxKindTableV1::default();
    for token in payload
        .conservative_tokens
        .iter()
        .chain(rename.into_iter().flatten())
    {
        kinds.intern(syntax_kind(token));
    }
    put_len(&mut encoded, kinds.names.len())?;
    for name in &kinds.names {
        put_str(&mut encoded, name);
    }
    put_tokens(&mut encoded, &payload.conservative_tokens, &kinds)?;
    match rename {
        None => encoded.push(RENAME_ABSENT),
        Some(rename) if aligned(&payload.conservative_tokens, rename) => {
            encoded.push(RENAME_ALIGNED);
            for (conservative, renamed) in payload.conservative_tokens.iter().zip(rename) {
                match (conservative, renamed) {
                    (
                        ConservativeCloneTokenV1::Syntax { text: original, .. },
                        ConservativeCloneTokenV1::Syntax { text, .. },
                    ) if original != text => {
                        encoded.push(RENAME_TOKEN_TEXT);
                        put_str(&mut encoded, text);
                    }
                    _ => encoded.push(RENAME_TOKEN_SAME),
                }
            }
        }
        Some(rename) => {
            encoded.push(RENAME_STREAM);
            put_tokens(&mut encoded, rename, &kinds)?;
        }
    }
    Ok((
        deflate_bytes(CLONE_PAYLOAD_DEFLATE, &encoded)?,
        encoded.len(),
    ))
}

/// Decode one stored payload and prove it is the payload `expected_digest`
/// names.
pub(super) fn decode_clone_payload(
    stored: &[u8],
    expected_digest: &str,
) -> Result<CloneBodyPayloadV1, CodeLexicalArtifactErrorV1> {
    let inflated = inflate_bytes(
        CLONE_PAYLOAD_DEFLATE,
        stored,
        CLONE_PAYLOAD_MAX_INFLATED_BYTES,
    )?;
    let mut bytes = inflated.as_slice();
    let language = take_string(&mut bytes)?;
    let symbol_kind = take_string(&mut bytes)?;
    let token_count = take_u32(&mut bytes)?;
    let conservative_normalization_revision = take_u16(&mut bytes)?;
    let tokenization_status = match take_u8(&mut bytes)? {
        0 => CloneBodyTokenizationStatusV1::Complete,
        1 => CloneBodyTokenizationStatusV1::Partial,
        _ => return Err(corrupt("tokenization status has an unknown tag")),
    };
    let tokenization_issues = (0..take_len(&mut bytes)?)
        .map(|_| match take_u8(&mut bytes)? {
            0 => Ok(CloneBodyTokenizationIssueV1::BodyBoundaryUnavailable),
            1 => Ok(CloneBodyTokenizationIssueV1::BodyExceedsSizeBound),
            2 => Ok(CloneBodyTokenizationIssueV1::InvalidSourceRange),
            3 => Ok(CloneBodyTokenizationIssueV1::ParseError),
            _ => Err(corrupt("tokenization issue has an unknown tag")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rename_normalization_revision = match take_varint(&mut bytes)? {
        0 => None,
        revision => Some(
            u16::try_from(revision - 1).map_err(|_| corrupt("rename revision overflows u16"))?,
        ),
    };
    let rename_coverage = match take_u8(&mut bytes)? {
        0 => CloneBodyRenameStatusV1::Complete,
        1 => CloneBodyRenameStatusV1::Partial,
        2 => CloneBodyRenameStatusV1::UnsupportedLanguage,
        _ => return Err(corrupt("rename coverage has an unknown tag")),
    };
    let rename_issues = (0..take_len(&mut bytes)?)
        .map(|_| match take_u8(&mut bytes)? {
            0 => Ok(CloneBodyRenameIssueV1::DynamicBinding),
            1 => Ok(CloneBodyRenameIssueV1::UnsupportedBindingSyntax),
            _ => Err(corrupt("rename issue has an unknown tag")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let kinds = (0..take_len(&mut bytes)?)
        .map(|_| take_string(&mut bytes))
        .collect::<Result<Vec<_>, _>>()?;
    let conservative_tokens: Arc<[ConservativeCloneTokenV1]> =
        take_tokens(&mut bytes, &kinds)?.into();
    let rename_tokens: Option<Arc<[ConservativeCloneTokenV1]>> = match take_u8(&mut bytes)? {
        RENAME_ABSENT => None,
        RENAME_ALIGNED => Some(
            conservative_tokens
                .iter()
                .map(|token| match (take_u8(&mut bytes)?, token) {
                    (RENAME_TOKEN_SAME, _) => Ok(token.clone()),
                    (RENAME_TOKEN_TEXT, ConservativeCloneTokenV1::Syntax { syntax_kind, .. }) => {
                        Ok(ConservativeCloneTokenV1::Syntax {
                            syntax_kind: syntax_kind.clone(),
                            text: Cow::Owned(take_string(&mut bytes)?),
                        })
                    }
                    _ => Err(corrupt("rename token difference is not canonical")),
                })
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        RENAME_STREAM => Some(take_tokens(&mut bytes, &kinds)?.into()),
        _ => return Err(corrupt("rename stream has an unknown tag")),
    };
    if !bytes.is_empty() {
        return Err(corrupt("payload has trailing bytes"));
    }
    let payload = CloneBodyPayloadV1::from_parts(CloneBodyPayloadPartsV1 {
        language,
        symbol_kind,
        token_count,
        conservative_normalization_revision,
        conservative_tokens,
        tokenization_status,
        tokenization_issues,
        rename_normalization_revision,
        rename_tokens,
        rename_coverage,
        rename_issues,
    })
    .map_err(CodeLexicalArtifactErrorV1::Corrupt)?;
    if payload.payload_digest.as_str() != expected_digest {
        return Err(corrupt("payload does not hash to its content address"));
    }
    Ok(payload)
}

#[derive(Default)]
struct SyntaxKindTableV1<'a> {
    names: Vec<&'a str>,
    indexes: HashMap<&'a str, u64>,
}

impl<'a> SyntaxKindTableV1<'a> {
    fn intern(&mut self, name: &'a str) {
        let next = self.names.len() as u64;
        if let std::collections::hash_map::Entry::Vacant(slot) = self.indexes.entry(name) {
            slot.insert(next);
            self.names.push(name);
        }
    }

    fn index(&self, name: &str) -> Result<u64, CodeLexicalArtifactErrorV1> {
        self.indexes.get(name).copied().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract("clone syntax kind was not interned".to_owned())
        })
    }
}

fn syntax_kind(token: &ConservativeCloneTokenV1) -> &str {
    match token {
        ConservativeCloneTokenV1::StructureStart { syntax_kind }
        | ConservativeCloneTokenV1::Syntax { syntax_kind, .. }
        | ConservativeCloneTokenV1::StructureEnd { syntax_kind } => syntax_kind,
    }
}

/// Whether `rename` differs from `conservative` only in syntax-token text.
fn aligned(conservative: &[ConservativeCloneTokenV1], rename: &[ConservativeCloneTokenV1]) -> bool {
    conservative.len() == rename.len()
        && conservative
            .iter()
            .zip(rename)
            .all(|(left, right)| match (left, right) {
                (
                    ConservativeCloneTokenV1::Syntax {
                        syntax_kind: left, ..
                    },
                    ConservativeCloneTokenV1::Syntax {
                        syntax_kind: right, ..
                    },
                ) => left == right,
                _ => left == right,
            })
}

fn put_tokens(
    encoded: &mut Vec<u8>,
    tokens: &[ConservativeCloneTokenV1],
    kinds: &SyntaxKindTableV1<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    put_len(encoded, tokens.len())?;
    for token in tokens {
        let (tag, text) = match token {
            ConservativeCloneTokenV1::StructureStart { .. } => (TOKEN_STRUCTURE_START, None),
            ConservativeCloneTokenV1::Syntax { text, .. } => (TOKEN_SYNTAX, Some(text)),
            ConservativeCloneTokenV1::StructureEnd { .. } => (TOKEN_STRUCTURE_END, None),
        };
        encode_varint(kinds.index(syntax_kind(token))? * 3 + tag, encoded);
        if let Some(text) = text {
            put_str(encoded, text);
        }
    }
    Ok(())
}

fn take_tokens(
    bytes: &mut &[u8],
    kinds: &[String],
) -> Result<Vec<ConservativeCloneTokenV1>, CodeLexicalArtifactErrorV1> {
    let count = take_len(bytes)?;
    // Every token takes at least one byte, so a count above the remaining
    // bytes is corrupt rather than an allocation to honor.
    if count > bytes.len() {
        return Err(corrupt("token count exceeds its record"));
    }
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        let header = take_varint(bytes)?;
        let kind = usize::try_from(header / 3)
            .ok()
            .and_then(|index| kinds.get(index))
            .ok_or_else(|| corrupt("token names an unknown syntax kind"))?;
        let syntax_kind = Cow::Owned(kind.clone());
        tokens.push(match header % 3 {
            TOKEN_STRUCTURE_START => ConservativeCloneTokenV1::StructureStart { syntax_kind },
            TOKEN_SYNTAX => ConservativeCloneTokenV1::Syntax {
                syntax_kind,
                text: Cow::Owned(take_string(bytes)?),
            },
            _ => ConservativeCloneTokenV1::StructureEnd { syntax_kind },
        });
    }
    Ok(tokens)
}

fn put_len(encoded: &mut Vec<u8>, length: usize) -> Result<(), CodeLexicalArtifactErrorV1> {
    encode_varint(u64::try_from(length).map_err(contract_number)?, encoded);
    Ok(())
}

fn put_str(encoded: &mut Vec<u8>, value: &str) {
    encode_varint(value.len() as u64, encoded);
    encoded.extend_from_slice(value.as_bytes());
}

fn take_len(bytes: &mut &[u8]) -> Result<usize, CodeLexicalArtifactErrorV1> {
    usize::try_from(take_varint(bytes)?).map_err(|_| corrupt("length overflows usize"))
}

fn take_u8(bytes: &mut &[u8]) -> Result<u8, CodeLexicalArtifactErrorV1> {
    let (&value, rest) = bytes.split_first().ok_or_else(|| corrupt("is truncated"))?;
    *bytes = rest;
    Ok(value)
}

fn take_u16(bytes: &mut &[u8]) -> Result<u16, CodeLexicalArtifactErrorV1> {
    u16::try_from(take_varint(bytes)?).map_err(|_| corrupt("value overflows u16"))
}

fn take_u32(bytes: &mut &[u8]) -> Result<u32, CodeLexicalArtifactErrorV1> {
    u32::try_from(take_varint(bytes)?).map_err(|_| corrupt("value overflows u32"))
}

fn take_string(bytes: &mut &[u8]) -> Result<String, CodeLexicalArtifactErrorV1> {
    let length = take_len(bytes)?;
    if length > bytes.len() {
        return Err(corrupt("string exceeds its record"));
    }
    let (value, rest) = bytes.split_at(length);
    *bytes = rest;
    String::from_utf8(value.to_vec()).map_err(|_| corrupt("string is not UTF-8"))
}

fn corrupt(detail: &str) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(format!("lexical artifact clone record {detail}"))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use tracedecay_code_index::clones::{
        CloneBodyEligibilityV1, CloneBodyPayloadPartsV1, CloneBodyPayloadV1,
        CloneBodyRenameIssueV1, CloneBodyRenameStatusV1, CloneBodyTokenizationIssueV1,
        CloneBodyTokenizationStatusV1, ConservativeCloneTokenV1,
    };

    use super::{
        decode_clone_eligibility, decode_clone_payload, encode_clone_eligibility,
        encode_clone_payload,
    };
    use crate::retrieval::lexical::CodeLexicalArtifactErrorV1;

    fn token(kind: &'static str, text: Option<&str>) -> ConservativeCloneTokenV1 {
        match text {
            Some(text) => ConservativeCloneTokenV1::Syntax {
                syntax_kind: Cow::Borrowed(kind),
                text: Cow::Owned(text.to_owned()),
            },
            None => ConservativeCloneTokenV1::StructureStart {
                syntax_kind: Cow::Borrowed(kind),
            },
        }
    }

    fn payload(rename: Option<Vec<ConservativeCloneTokenV1>>) -> CloneBodyPayloadV1 {
        let conservative = vec![
            token("block", None),
            token("identifier", Some("alpha")),
            token("+", Some("+")),
            token("identifier", Some("beta")),
            ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: Cow::Borrowed("block"),
            },
        ];
        CloneBodyPayloadV1::from_parts(CloneBodyPayloadPartsV1 {
            language: "rust".to_owned(),
            symbol_kind: "function".to_owned(),
            token_count: 3,
            conservative_normalization_revision: 1,
            conservative_tokens: conservative.into(),
            tokenization_status: CloneBodyTokenizationStatusV1::Partial,
            tokenization_issues: vec![CloneBodyTokenizationIssueV1::ParseError],
            rename_normalization_revision: rename.as_ref().map(|_| 1),
            rename_tokens: rename.map(Arc::from),
            rename_coverage: CloneBodyRenameStatusV1::Partial,
            rename_issues: vec![CloneBodyRenameIssueV1::DynamicBinding],
        })
        .expect("payload")
    }

    #[test]
    fn payloads_round_trip_with_aligned_divergent_and_absent_rename_streams() {
        let aligned = vec![
            token("block", None),
            token("identifier", Some("$0")),
            token("+", Some("+")),
            token("identifier", Some("$1")),
            ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: Cow::Borrowed("block"),
            },
        ];
        let divergent = vec![token("call", None), token("identifier", Some("$0"))];
        for payload in [
            payload(Some(aligned)),
            payload(Some(divergent)),
            payload(None),
        ] {
            let (stored, inflated) = encode_clone_payload(&payload).expect("encode");
            assert!(inflated > 0);
            let decoded =
                decode_clone_payload(&stored, payload.payload_digest.as_str()).expect("decode");
            assert_eq!(decoded, payload);
            assert_eq!(
                encode_clone_payload(&decoded).expect("re-encode").0,
                stored,
                "the encoding is canonical"
            );
        }
    }

    #[test]
    fn payloads_refuse_another_address_and_damaged_bytes() {
        let payload = payload(None);
        let (stored, _) = encode_clone_payload(&payload).expect("encode");
        let other = super::super::format::stored_metadata_digest(b"other").expect("digest");
        assert!(matches!(
            decode_clone_payload(&stored, other.as_str()),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
        let mut damaged = stored.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        assert!(decode_clone_payload(&damaged, payload.payload_digest.as_str()).is_err());
    }

    #[test]
    fn eligibility_round_trips_every_variant() {
        for eligibility in [
            CloneBodyEligibilityV1::Eligible,
            CloneBodyEligibilityV1::ExcludedIncompleteTokenization,
            CloneBodyEligibilityV1::ExcludedTooSmall { minimum_tokens: 30 },
            CloneBodyEligibilityV1::ExcludedTooLarge {
                maximum_tokens: 4096,
                maximum_bytes: 65_536,
            },
        ] {
            assert_eq!(
                decode_clone_eligibility(&encode_clone_eligibility(eligibility)).expect("decode"),
                eligibility
            );
        }
        assert!(decode_clone_eligibility(&[9]).is_err());
        assert!(decode_clone_eligibility(&[0, 0]).is_err());
    }
}
