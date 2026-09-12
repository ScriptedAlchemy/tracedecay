use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, CodeSearchChunkV1, ComponentRevision, ExactFieldV1,
    ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId, LanguageDescriptorRevision,
    RepositoryId, RetrievalAnchorId, ScoreDomainId, SourceFreshness, exact_search_canonical,
    technical_tokens, validate_code_logical_path,
};

use super::{LexicalFieldV1, LexicalLaneRequest};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneRequest};
use crate::retrieval::ports::{RetrievalPortError, contract_error};

mod artifact;
#[cfg(feature = "search-eval")]
mod in_memory;

pub use artifact::{
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeExactLexicalArtifactReaderV1,
    CodeLexicalArtifactBatchLimitV1, CodeLexicalArtifactBuildProgressV1,
    CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1,
    CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactReaderV1,
    CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactWriterRevisionV1,
    CodeLexicalImportMembershipWitnessV1, PreparedCodeLexicalArtifactBatchV1,
    PreparedCodeLexicalArtifactPageV1, VerifiedCodeLexicalArtifactV1,
};
#[cfg(feature = "search-eval")]
pub use in_memory::{
    CodeExactProjectionAdapterV1, CodeLexicalProjectionAdapterV1, CodeLexicalProjectionBuildStepV1,
    CodeLexicalProjectionBuildV1, LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1,
    lexical_projection_build_deadline_micros,
};

const BM25_K1_MILLIS: u64 = 1_200;
const BM25_B_MILLIS: u64 = 750;
const FUZZY_SCORE_MILLIS: u64 = 500;
const PHRASE_SCORE_MILLIS: u64 = 2_000;
const ECHO_SCORE_MILLIS: u64 = 750;

/// Generation and source metadata bound to one immutable lexical projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalProjectionMetadataV1 {
    pub generation: CodeGenerationId,
    pub repository_id: Option<RepositoryId>,
    pub logical_paths: BTreeMap<FileOccurrenceId, String>,
    pub freshness: SourceFreshness,
    pub exact_retriever_revision: ComponentRevision,
    pub lexical_retriever_revision: ComponentRevision,
    pub exact_score_domain: ScoreDomainId,
}

impl CodeLexicalProjectionMetadataV1 {
    fn validate(&self) -> Result<(), RetrievalPortError> {
        self.generation.validate().map_err(contract_error)?;
        if let Some(repository_id) = &self.repository_id {
            repository_id.validate().map_err(contract_error)?;
        }
        for (file, path) in &self.logical_paths {
            file.validate().map_err(contract_error)?;
            validate_code_logical_path(path).map_err(contract_error)?;
        }
        self.freshness
            .source_namespace
            .validate()
            .map_err(contract_error)?;
        self.freshness
            .source_instance
            .validate()
            .map_err(contract_error)?;
        self.freshness
            .policy_revision
            .validate()
            .map_err(contract_error)?;
        self.exact_retriever_revision
            .validate()
            .map_err(contract_error)?;
        self.lexical_retriever_revision
            .validate()
            .map_err(contract_error)?;
        self.exact_score_domain.validate().map_err(contract_error)
    }
}

#[derive(Clone, Debug)]
struct ProjectedChunkV1 {
    id: CodeSearchChunkId,
    anchor: CodeSearchChunkAnchorV1,
    language_descriptor_revision: LanguageDescriptorRevision,
    exact_terms: Vec<ExactTechnicalTermV1>,
    sanitized_text: BoundedSanitizedText,
    logical_path: String,
    symbol_simple_name: Option<String>,
    symbol_qualified_name: Option<String>,
    symbol_kind: Option<String>,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
    normalized_text: String,
}

impl ProjectedChunkV1 {
    /// Clone only the fields the projection retains. Sealed pages lend chunks,
    /// so this avoids `admitted.clone().into_chunk()` copying subtokens and
    /// other dropped payloads on the artifact append path.
    fn from_ref(
        chunk: &CodeSearchChunkV1,
        logical_path: String,
        display: Option<&tracedecay_code_index::production::VerifiedSealedLexicalSymbolDisplayV1>,
    ) -> (Self, BTreeMap<LexicalFieldV1, Vec<String>>) {
        let fields =
            Self::projected_fields(chunk, &logical_path, display.map(|d| d.qualified_name()));
        let normalized_text = normalize_lexical(chunk.sanitized_text.as_str());
        Self::from_parts(
            chunk.id.clone(),
            chunk.anchor.clone(),
            chunk.language_descriptor_revision.clone(),
            chunk.exact_terms.clone(),
            chunk.sanitized_text.clone(),
            logical_path,
            display.map(|display| display.simple_name().to_owned()),
            display.map(|display| display.qualified_name().to_owned()),
            display.map(|display| display.kind().to_owned()),
            normalized_text,
            fields,
        )
    }

    /// `qualified_name` is the parser-attested extracted qualified name of the
    /// symbol this chunk anchors. Rust (and every language whose declarations
    /// name the type and the member separately) never spells `Type::member` in
    /// source, so the whole-term postings derived from chunk text and the
    /// simple-name whole-symbol term cannot answer a qualified-name query. The
    /// extractor is the only authority for that complete name, so the
    /// projection indexes it directly instead of re-deriving one.
    fn projected_fields(
        chunk: &CodeSearchChunkV1,
        logical_path: &str,
        qualified_name: Option<&str>,
    ) -> BTreeMap<LexicalFieldV1, Vec<String>> {
        let mut fields: BTreeMap<LexicalFieldV1, Vec<String>> = BTreeMap::new();
        if let Some(qualified_name) = qualified_name.filter(|name| !name.is_empty()) {
            let qualified_name_postings = fields.entry(LexicalFieldV1::QualifiedName).or_default();
            qualified_name_postings.push(normalize_lexical(qualified_name));
            // The extractor attests the qualified name with the declaring
            // logical path ahead of the symbol path
            // (`src/watermark.rs::VectorWatermark::merge_max`), but an operator
            // names the symbol path alone (`VectorWatermark::merge_max`).
            // Postings are whole terms and cannot match a suffix, so index the
            // attested symbol path as its own key. Only the logical path this
            // chunk already carries is removed - nothing is re-derived from
            // source text, so a wrong qualifier still misses. A path-only
            // qualification leaves a bare simple name, which `SymbolName`
            // already answers from the whole-symbol exact term.
            if let Some(symbol_path) = qualified_name
                .strip_prefix(logical_path)
                .and_then(|suffix| suffix.strip_prefix("::"))
                .filter(|symbol_path| symbol_path.contains("::"))
            {
                qualified_name_postings.push(normalize_lexical(symbol_path));
            }
        }
        let text_field = if chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
            LexicalFieldV1::PreambleText
        } else {
            LexicalFieldV1::BodyText
        };
        fields.insert(text_field, lexical_tokens(chunk.sanitized_text.as_str()));
        fields.insert(LexicalFieldV1::Path, vec![normalize_lexical(logical_path)]);
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
        fields
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        id: CodeSearchChunkId,
        anchor: CodeSearchChunkAnchorV1,
        language_descriptor_revision: LanguageDescriptorRevision,
        exact_terms: Vec<ExactTechnicalTermV1>,
        sanitized_text: BoundedSanitizedText,
        logical_path: String,
        symbol_simple_name: Option<String>,
        symbol_qualified_name: Option<String>,
        symbol_kind: Option<String>,
        normalized_text: String,
        fields: BTreeMap<LexicalFieldV1, Vec<String>>,
    ) -> (Self, BTreeMap<LexicalFieldV1, Vec<String>>) {
        let field_lengths = fields
            .iter()
            .map(|(field, terms)| (*field, terms.len()))
            .collect();
        (
            Self {
                id,
                anchor,
                language_descriptor_revision,
                exact_terms,
                sanitized_text,
                logical_path,
                symbol_simple_name,
                symbol_qualified_name,
                symbol_kind,
                field_lengths,
                normalized_text,
            },
            fields,
        )
    }
}

#[derive(Default)]
struct FuzzyExpansionsV1 {
    by_query: BTreeMap<String, BTreeSet<String>>,
}

struct FuzzyQueryGroupV1 {
    first_ordinal: usize,
    normalized_query: String,
    queries: BTreeSet<String>,
    bound: usize,
    seen: BTreeSet<String>,
}

struct LexicalRowScoreV1 {
    field_scores: Vec<(LexicalFieldV1, u64)>,
    matched_whole_terms: Vec<String>,
    matched_subtokens: Vec<String>,
    matched_phrases: Vec<String>,
    matched_kinds: Vec<ExactTechnicalTermKindV1>,
    typo_recovery_applied: bool,
    echo_penalty_applied: bool,
}

/// The borrowed subset of a projected chunk that exact-literal matching
/// reads, so artifact rows never deep-clone chunk text per visited document.
struct ExactMatchRowViewV1<'a> {
    sanitized_text: &'a str,
    logical_path: &'a str,
    exact_terms: &'a [ExactTechnicalTermV1],
}

/// Match one row against every request literal, returning matched literal
/// ordinals into `request.literals`. Ordinals defer the literal clones to
/// the cap-bounded winners instead of paying them per visited document.
fn exact_matches(
    row: ExactMatchRowViewV1<'_>,
    request: &ExactLaneRequest,
) -> (Vec<usize>, Vec<ExactTechnicalTermKindV1>) {
    let mut matched_literals = Vec::new();
    let mut matched_kinds = BTreeSet::new();
    for (ordinal, literal) in request.literals.iter().enumerate() {
        let mut matched = false;
        if matches!(
            literal.field,
            ExactFieldV1::QuotedPhrase
                | ExactFieldV1::DiagnosticText
                | ExactFieldV1::CompilerOrRuntimeError
        ) {
            matched = contains_bytes(row.sanitized_text.as_bytes(), &literal.original_bytes);
        }
        if literal.field == ExactFieldV1::Path
            && row.logical_path.as_bytes() == literal.canonical_bytes.as_slice()
        {
            matched = true;
            matched_kinds.insert(ExactTechnicalTermKindV1::Path);
        }
        for term in row.exact_terms {
            if exact_field_for_kind(term.kind()) == literal.field
                && canonical_projected_exact_term(term).as_ref()
                    == literal.canonical_bytes.as_slice()
            {
                matched = true;
                matched_kinds.insert(term.kind());
            }
        }
        if matched {
            matched_literals.push(ordinal);
        }
    }
    (matched_literals, matched_kinds.into_iter().collect())
}

/// Per-request lazily admitted proofs, one slot per request literal.
///
/// An admission proof depends only on the literal and the request — never on
/// the matched document — while one `admit` costs four canonical-JSON SHA-256
/// digests. Both posting adapters previously re-admitted per matching
/// document, which dominated exact retrieval for high-cardinality literals.
/// Laziness preserves the original failure surface: a literal no document
/// matches is never admitted at all.
struct LiteralProofCacheV1 {
    slots: Vec<Option<Option<tracedecay_domain::ExactAdmissionProof>>>,
}

impl LiteralProofCacheV1 {
    fn new(literal_count: usize) -> Self {
        Self {
            slots: vec![None; literal_count],
        }
    }

    /// The first matched literal ordinal the central authority admits, with
    /// its proof — the same first-admitting-literal selection the per-document
    /// `find_map` performed, at most one `admit` per literal per request.
    fn first_admitted<A>(
        &mut self,
        matched_ordinals: &[usize],
        request: &ExactLaneRequest<'_>,
        authority: &A,
    ) -> Result<Option<(usize, tracedecay_domain::ExactAdmissionProof)>, RetrievalPortError>
    where
        A: ExactAdmissionAuthority,
    {
        for ordinal in matched_ordinals {
            let slot = self.slots.get_mut(*ordinal).ok_or_else(|| {
                RetrievalPortError::Contract(
                    "exact match ordinal is outside the request literals".to_owned(),
                )
            })?;
            if slot.is_none() {
                let literal = &request.literals[*ordinal];
                *slot = Some(
                    authority
                        .admit(literal.field, &literal.original_bytes, &request.base)
                        .map_err(contract_error)?,
                );
            }
            if let Some(Some(proof)) = slot {
                return Ok(Some((*ordinal, proof.clone())));
            }
        }
        Ok(None)
    }

    /// The already-admitted proof for one literal ordinal. Winner
    /// materialization resolves through this instead of retaining a proof
    /// clone per visited document.
    fn admitted_proof(
        &self,
        ordinal: usize,
    ) -> Result<tracedecay_domain::ExactAdmissionProof, RetrievalPortError> {
        self.slots
            .get(ordinal)
            .and_then(|slot| slot.as_ref())
            .and_then(|admitted| admitted.clone())
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "exact winner names a literal the authority never admitted".to_owned(),
                )
            })
    }
}

/// Query-derived strings normalized once per retrieval. Row scoring reuses
/// these instead of re-normalizing every query term, subtoken, phrase, and
/// the whole echo query for every visited document.
struct PreparedLexicalQueryV1<'request> {
    /// `(original, normalized)` per request whole term.
    whole_terms: Vec<(&'request str, String)>,
    /// `(original, normalized)` per request subtoken.
    subtokens: Vec<(&'request str, String)>,
    /// `(original, normalized)` per request phrase.
    phrases: Vec<(&'request str, String)>,
    /// The normalized quote-trimmed query for the echo penalty.
    echo_query: String,
}

impl<'request> PreparedLexicalQueryV1<'request> {
    fn new(request: &'request LexicalLaneRequest<'_>) -> Self {
        Self {
            whole_terms: request
                .whole_terms
                .iter()
                .map(|term| (term.as_str(), normalize_lexical(term)))
                .collect(),
            subtokens: request
                .subtokens
                .iter()
                .map(|subtoken| (subtoken.as_str(), normalize_lexical(subtoken)))
                .collect(),
            phrases: request
                .phrases
                .iter()
                .map(|phrase| (phrase.as_str(), normalize_lexical(phrase)))
                .collect(),
            echo_query: normalize_lexical(request.query_view.as_str().trim_matches('"')),
        }
    }
}

fn exact_field_for_kind(kind: ExactTechnicalTermKindV1) -> ExactFieldV1 {
    match kind {
        ExactTechnicalTermKindV1::WholeSymbol => ExactFieldV1::Identifier,
        ExactTechnicalTermKindV1::QualifiedName => ExactFieldV1::QualifiedName,
        ExactTechnicalTermKindV1::Path => ExactFieldV1::Path,
        ExactTechnicalTermKindV1::CompilerErrorCode
        | ExactTechnicalTermKindV1::RuntimeErrorCode => ExactFieldV1::DiagnosticCode,
        ExactTechnicalTermKindV1::CompilerErrorText
        | ExactTechnicalTermKindV1::RuntimeErrorText => ExactFieldV1::CompilerOrRuntimeError,
        ExactTechnicalTermKindV1::CliFlag => ExactFieldV1::CliFlag,
        ExactTechnicalTermKindV1::ToolName => ExactFieldV1::ToolName,
        ExactTechnicalTermKindV1::ConfigurationKey => ExactFieldV1::ConfigurationKey,
        ExactTechnicalTermKindV1::CommitIdentifier => ExactFieldV1::CommitIdentifier,
    }
}

/// Posting-key form of one minted term: the shared per-field search
/// canonicalization over the mint canonical, after stripping the extraction
/// `commit:` prefix so bare-hash query literals address the same key.
fn canonical_projected_exact_term(term: &ExactTechnicalTermV1) -> Cow<'_, [u8]> {
    let bytes = term.canonical_bytes();
    let Ok(value) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let value = if term.kind() == ExactTechnicalTermKindV1::CommitIdentifier {
        value.strip_prefix("commit:").unwrap_or(value)
    } else {
        value
    };
    let canonical = exact_search_canonical(exact_field_for_kind(term.kind()), value);
    if canonical.as_bytes() == bytes {
        Cow::Borrowed(bytes)
    } else {
        Cow::Owned(canonical.into_owned().into_bytes())
    }
}

fn collect_term_kinds(
    exact_terms: &[ExactTechnicalTermV1],
    normalized_term: &str,
    kinds: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    for term in exact_terms {
        // `normalized_term` is already ASCII-lowercased, so the allocation-free
        // case-insensitive comparison is exactly `normalize_lexical(value) ==
        // normalized_term`.
        if std::str::from_utf8(term.canonical_bytes())
            .is_ok_and(|value| value.eq_ignore_ascii_case(normalized_term))
        {
            kinds.insert(term.kind());
        }
    }
}

fn retrieval_anchor(value: String) -> Result<RetrievalAnchorId, RetrievalPortError> {
    RetrievalAnchorId::new(value).map_err(contract_error)
}

fn normalize_lexical(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn lexical_tokens(value: &str) -> Vec<String> {
    technical_tokens(value)
        .map(|(_, token)| normalize_lexical(token))
        .collect()
}

fn substring_count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn add_score(scores: &mut BTreeMap<LexicalFieldV1, u64>, field: LexicalFieldV1, score: u64) {
    scores
        .entry(field)
        .and_modify(|current| *current = current.saturating_add(score))
        .or_insert(score);
}

fn field_weight_millis(field: LexicalFieldV1) -> u64 {
    match field {
        LexicalFieldV1::SymbolName => 4_000,
        LexicalFieldV1::QualifiedName => 3_500,
        LexicalFieldV1::Path => 3_000,
        LexicalFieldV1::ExactTerm => 2_500,
        LexicalFieldV1::Subtoken => 1_500,
        LexicalFieldV1::BodyText | LexicalFieldV1::PreambleText => 1_000,
    }
}

fn bm25_score_micros(
    document_count: usize,
    document_frequency: usize,
    term_frequency: usize,
    document_length: usize,
    average_length: usize,
    field_weight_millis: u64,
) -> u64 {
    if document_count == 0 || document_frequency == 0 || term_frequency == 0 {
        return 0;
    }
    let idf_micros = fixed_ln_ratio_micros(
        (document_count as u64).saturating_mul(2).saturating_add(2),
        (document_frequency as u64)
            .saturating_mul(2)
            .saturating_add(1),
    );
    let length_ratio_millis =
        (document_length as u128).saturating_mul(1_000) / average_length.max(1) as u128;
    let normalization_millis = u128::from(1_000 - BM25_B_MILLIS)
        + u128::from(BM25_B_MILLIS).saturating_mul(length_ratio_millis) / 1_000;
    let denominator_millis = (term_frequency as u128).saturating_mul(1_000)
        + u128::from(BM25_K1_MILLIS).saturating_mul(normalization_millis) / 1_000;
    let tf_micros = (term_frequency as u128)
        .saturating_mul(u128::from(BM25_K1_MILLIS + 1_000))
        .saturating_mul(1_000_000)
        / denominator_millis.max(1);
    let score = u128::from(idf_micros)
        .saturating_mul(tf_micros)
        .saturating_mul(u128::from(field_weight_millis))
        / 1_000_000
        / 1_000;
    score.min(u128::from(u64::MAX)) as u64
}

fn fixed_ln_ratio_micros(numerator: u64, denominator: u64) -> u64 {
    const SCALE: u128 = 1_u128 << 40;
    const LN_2_SCALED: u128 = 762_123_384_786;
    let mut ratio = u128::from(numerator).saturating_mul(SCALE) / u128::from(denominator.max(1));
    let mut powers_of_two = 0_u128;
    while ratio >= SCALE.saturating_mul(2) {
        ratio /= 2;
        powers_of_two += 1;
    }
    let z = ratio.saturating_sub(SCALE).saturating_mul(SCALE) / ratio.saturating_add(SCALE).max(1);
    let z_squared = z.saturating_mul(z) / SCALE;
    let mut term = z;
    let mut sum = term;
    for divisor in [3_u128, 5, 7, 9, 11, 13, 15] {
        term = term.saturating_mul(z_squared) / SCALE;
        sum = sum.saturating_add(term / divisor);
    }
    let scaled = sum
        .saturating_mul(2)
        .saturating_add(powers_of_two.saturating_mul(LN_2_SCALED));
    (scaled.saturating_mul(1_000_000) / SCALE).min(u128::from(u64::MAX)) as u64
}

/// Upper byte-length caps that keep the fst Levenshtein automaton inside its
/// fixed 10_000-state capacity. The DFA size is content-dependent — repeated
/// characters collapse states, so uniform strings are the automaton's best
/// case — and the caps are anchored to the measured worst case on fst 0.4
/// (all-distinct bytes: distance 1 builds up to 416 bytes, distance 2 only up
/// to 49) with headroom below those ceilings. Queries beyond a cap skip fuzzy
/// expansion the same way sub-5-character queries do; the exact and phrase
/// lanes still serve them.
const FUZZY_DISTANCE_ONE_MAX_BYTES: usize = 320;
const FUZZY_DISTANCE_TWO_MAX_BYTES: usize = 32;

fn fuzzy_distance_bound(normalized_query: &str) -> usize {
    let character_count = normalized_query.chars().count();
    let byte_count = normalized_query.len();
    if character_count <= 4 || byte_count > FUZZY_DISTANCE_ONE_MAX_BYTES {
        0
    } else if character_count <= 8 || byte_count > FUZZY_DISTANCE_TWO_MAX_BYTES {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod fuzzy_distance_bound_tests {
    use super::*;

    /// Worst-case automaton input: all-distinct bytes defeat the DFA state
    /// collapsing that repeated characters allow.
    fn distinct_byte_query(length: usize) -> String {
        (0u32..length as u32)
            .map(|i| char::from_u32(33 + (i * 7) % 94).expect("printable ascii"))
            .collect()
    }

    #[test]
    fn caps_stay_inside_the_fst_automaton_capacity() {
        // Anchors the byte caps to the automaton implementation: if fst ever
        // tightens its state limit these constructions fail and the caps must
        // shrink with them.
        fst::automaton::Levenshtein::new(&distinct_byte_query(FUZZY_DISTANCE_ONE_MAX_BYTES), 1)
            .expect("distance-1 automaton must fit at the distance-1 byte cap");
        fst::automaton::Levenshtein::new(&distinct_byte_query(FUZZY_DISTANCE_TWO_MAX_BYTES), 2)
            .expect("distance-2 automaton must fit at the distance-2 byte cap");
    }

    #[test]
    fn long_queries_skip_fuzzy_expansion_like_short_ones() {
        assert_eq!(fuzzy_distance_bound("abcd"), 0);
        assert_eq!(fuzzy_distance_bound("abcdef"), 1);
        assert_eq!(fuzzy_distance_bound("abcdefghi"), 2);
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_TWO_MAX_BYTES)),
            2
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_TWO_MAX_BYTES + 1)),
            1
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES)),
            1
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES + 1)),
            0
        );
        // Multibyte queries are capped by their UTF-8 byte length, not their
        // character count.
        let multibyte = "\u{3042}".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES / 3 + 1);
        assert_eq!(fuzzy_distance_bound(&multibyte), 0);
    }
}
