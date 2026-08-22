//! Independent exact-literal lane contracts.
//!
//! The exact tier is non-demotable and consumes only whole exact technical
//! terms plus a central `ExactAdmissionProof`.
//!
//! The exact lane is a true independent lane, separate from the fielded
//! lexical/BM25 lane. An approximate, graph-only, or later semantic candidate
//! cannot precede an eligible exact result.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, CursorPayloadDigest, EphemeralSanitizedQueryViewV1,
    ExactAdmissionProof, ExactAdmissionRuleRevision, ExactAdmissionValidator, ExactFieldV1,
    FixedPointScore, RetrievalBudget, RetrievalError, RetrievalFailure, RetrievalRequest,
    Retriever, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome,
};

use super::ports::{
    CodeCandidateBindingV1, CompactCandidateLane, ExactTermPostingReadPort, LaneBoundEvidence,
    LaneEvidenceRejections, RetrievalPortError, candidate_checkpoint_prefix, checkpoint_digest,
    contract_error, lane_bound_evidence, lane_candidate_cap,
};

/// Wording the exact lane uses when a port-emitted batch fails the shared
/// candidate/evidence binding checks.
const EXACT_REJECTIONS: LaneEvidenceRejections = LaneEvidenceRejections {
    foreign_candidate: "the exact lane cannot emit non-exact candidates",
    missing_evidence: "exact lane evidence is missing for a returned occurrence",
    unaddressed_binding: "exact lane binding does not address its candidate",
};

/// Typed exact-lane request.
///
/// Exact technical literals are parsed under a versioned exact-admission
/// specification before any lane executes.
#[derive(Debug, PartialEq, Eq)]
pub struct ExactLaneRequest<'a> {
    pub base: RetrievalRequest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub generation: CodeGenerationId,
    /// Candidate literals with their typed fields, pre-parsed by the central
    /// admission validator. The lane never re-derives exact status.
    pub literals: Vec<ExactLiteralV1>,
    pub budget: RetrievalBudget,
}

impl ExactLaneRequest<'_> {
    pub fn validate(&self) -> Result<(), RetrievalPortError> {
        self.base.budget.validate().map_err(contract_error)?;
        self.budget.validate().map_err(contract_error)?;
        self.generation.validate().map_err(contract_error)?;
        let mut seen = BTreeSet::new();
        for literal in &self.literals {
            literal.validate()?;
            if !seen.insert((
                literal.field,
                literal.original_bytes.clone(),
                literal.canonical_bytes.clone(),
            )) {
                return Err(RetrievalPortError::Contract(
                    "exact lane literals must be unique".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// One pre-parsed exact literal candidate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLiteralV1 {
    pub field: ExactFieldV1,
    pub original_bytes: Vec<u8>,
    pub canonical_bytes: Vec<u8>,
}

impl ExactLiteralV1 {
    pub fn validate(&self) -> Result<(), RetrievalPortError> {
        if self.original_bytes.is_empty() || self.canonical_bytes.is_empty() {
            return Err(RetrievalPortError::Contract(
                "exact literals require non-empty original and canonical bytes".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Per-occurrence exact-lane evidence.
///
/// Each returned `source_occurrence_id` has exactly one typed evidence value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub matched_literals: Vec<ExactLiteralV1>,
    /// The validated admission proof minted centrally; the lane attaches it,
    /// it never constructs it.
    pub admission_proof: ExactAdmissionProof,
}

impl LaneBoundEvidence for ExactLaneEvidence {
    fn binding(&self) -> &CodeCandidateBindingV1 {
        &self.binding
    }
}

impl ExactLaneEvidence {
    pub fn validate(&self, request: &ExactLaneRequest<'_>) -> Result<(), RetrievalPortError> {
        request.validate()?;
        self.validate_against_validated_request(request)
    }

    /// Same rejection set as [`Self::validate`], minus the request
    /// revalidation the caller has already performed.
    ///
    /// The lane validates the request once per retrieval; re-running it for
    /// every candidate in the batch is pure hot-path cost.
    fn validate_against_validated_request(
        &self,
        request: &ExactLaneRequest<'_>,
    ) -> Result<(), RetrievalPortError> {
        if self.binding.occurrence.generation != request.generation {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        if self.matched_literals.is_empty() {
            return Err(RetrievalPortError::Contract(
                "exact lane evidence requires a matched literal".to_owned(),
            ));
        }
        for literal in &self.matched_literals {
            literal.validate()?;
        }
        self.admission_proof
            .validate_for_request(&request.base)
            .map_err(contract_error)?;
        if !self.matched_literals.iter().any(|literal| {
            literal.field == self.admission_proof.field
                && literal.original_bytes == self.admission_proof.original_bytes
                && literal.canonical_bytes == self.admission_proof.canonical_bytes
        }) {
            return Err(RetrievalPortError::Contract(
                "exact admission proof does not match the admitted literal".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The exact-lane retriever contract. Implementations adapt the store-side
/// `ExactTermPostingReadPort` into `CompactCandidate` values for one frozen
/// generation.
pub trait ExactLaneRetriever {
    /// Retrieve the committed exact-tier candidate prefix for `request`.
    fn retrieve_exact(
        &self,
        request: &ExactLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError>;
}

/// The sole exact-admission authority surface.
///
/// Only the central exact-admission validator can mint
/// `ExactAdmissionProof`; retrievers cannot assign an exact tier.
pub trait ExactAdmissionAuthority: ExactAdmissionValidator {
    /// Parse `request.query` into typed literal candidates under the
    /// versioned admission specification, preserving original bytes and
    /// normalization provenance.
    fn parse_literals(
        &self,
        query_view: &EphemeralSanitizedQueryViewV1,
        request: &RetrievalRequest,
    ) -> Vec<ExactLiteralV1>;
}

/// Versioned central exact-admission authority for code queries.
///
/// Query parsing and proof minting intentionally live on the same authority.
/// Posting readers can ask this value to mint a proof, but cannot construct or
/// upgrade an exact class themselves. Unprefixed terms use the frozen
/// technical grammar; typed prefixes cover exact error text and the remaining
/// protocol/task fields without guessing from free text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CentralExactAdmissionAuthorityV1 {
    rule_revision: ExactAdmissionRuleRevision,
}

impl CentralExactAdmissionAuthorityV1 {
    pub fn new(rule_revision: ExactAdmissionRuleRevision) -> Self {
        Self { rule_revision }
    }
}

impl ExactAdmissionValidator for CentralExactAdmissionAuthorityV1 {
    fn admit(
        &self,
        field: ExactFieldV1,
        candidate_bytes: &[u8],
        request: &RetrievalRequest,
    ) -> Result<Option<ExactAdmissionProof>, RetrievalError> {
        let Ok(candidate) = std::str::from_utf8(candidate_bytes) else {
            return Ok(None);
        };
        if !exact_field_accepts(field, candidate) {
            return Ok(None);
        }
        let (canonical_bytes, normalization_steps) = canonicalize_exact(field, candidate);
        let proof = ExactAdmissionProof {
            rule_revision: self.rule_revision.clone(),
            field,
            original_bytes: candidate_bytes.to_vec(),
            canonical_bytes,
            normalization_steps,
            scope_digest: request.scope.compute_digest()?,
            authorization_revision: request.snapshot.authorization_revision.clone(),
            snapshot_digest: request.snapshot.compute_digest()?,
        };
        proof.validate_for_request(request)?;
        Ok(Some(proof))
    }
}

impl ExactAdmissionAuthority for CentralExactAdmissionAuthorityV1 {
    fn parse_literals(
        &self,
        query_view: &EphemeralSanitizedQueryViewV1,
        _request: &RetrievalRequest,
    ) -> Vec<ExactLiteralV1> {
        let mut literals = Vec::new();
        let mut seen = BTreeSet::new();
        let query = query_view.as_str();
        if !query.contains('"')
            && !query
                .split_whitespace()
                .next()
                .is_some_and(|atom| atom.contains(':'))
            && is_contextual_error_text(query)
        {
            let (canonical_bytes, _) =
                canonicalize_exact(ExactFieldV1::CompilerOrRuntimeError, query);
            let literal = ExactLiteralV1 {
                field: ExactFieldV1::CompilerOrRuntimeError,
                original_bytes: query.as_bytes().to_vec(),
                canonical_bytes,
            };
            seen.insert((
                literal.field,
                literal.original_bytes.clone(),
                literal.canonical_bytes.clone(),
            ));
            literals.push(literal);
        }
        for atom in exact_query_atoms(query) {
            let field = atom
                .field
                .or_else(|| classify_unprefixed_exact(atom.text.as_str(), atom.quoted));
            let Some(field) = field else {
                continue;
            };
            if !exact_field_accepts(field, &atom.text) {
                continue;
            }
            let (canonical_bytes, _) = canonicalize_exact(field, &atom.text);
            let literal = ExactLiteralV1 {
                field,
                original_bytes: atom.text.into_bytes(),
                canonical_bytes,
            };
            if seen.insert((
                literal.field,
                literal.original_bytes.clone(),
                literal.canonical_bytes.clone(),
            )) {
                literals.push(literal);
            }
        }
        literals
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ExactQueryAtom {
    field: Option<ExactFieldV1>,
    text: String,
    quoted: bool,
}

fn exact_query_atoms(query: &str) -> Vec<ExactQueryAtom> {
    let mut atoms = Vec::new();
    let mut cursor = 0;
    while cursor < query.len() {
        while let Some(ch) = query[cursor..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            cursor += ch.len_utf8();
        }
        if cursor == query.len() {
            break;
        }
        let start = cursor;
        let mut quote = None;
        while let Some(ch) = query[cursor..].chars().next() {
            if ch == '"' {
                quote = Some(cursor);
                break;
            }
            if ch.is_whitespace() {
                break;
            }
            cursor += ch.len_utf8();
        }
        if let Some(quote_start) = quote {
            let prefix = &query[start..quote_start];
            if !prefix.is_empty() && !prefix.ends_with(':') {
                while let Some(ch) = query[cursor..].chars().next() {
                    if ch.is_whitespace() {
                        break;
                    }
                    cursor += ch.len_utf8();
                }
                continue;
            }
            cursor = quote_start + 1;
            let value_start = cursor;
            while let Some(ch) = query[cursor..].chars().next() {
                if ch == '"' {
                    break;
                }
                cursor += ch.len_utf8();
            }
            if cursor == query.len() {
                break;
            }
            let text = query[value_start..cursor].to_owned();
            cursor += 1;
            atoms.push(ExactQueryAtom {
                field: if prefix.is_empty() {
                    Some(ExactFieldV1::QuotedPhrase)
                } else {
                    exact_field_prefix(prefix.trim_end_matches(':'))
                },
                text,
                quoted: true,
            });
            continue;
        }
        let token = &query[start..cursor];
        let (field, text) = token
            .split_once(':')
            .and_then(|(prefix, text)| exact_field_prefix(prefix).map(|field| (Some(field), text)))
            .unwrap_or((None, token));
        atoms.push(ExactQueryAtom {
            field,
            text: text.to_owned(),
            quoted: false,
        });
    }
    atoms
}

fn is_contextual_error_text(value: &str) -> bool {
    value.split_whitespace().nth(1).is_some()
        && value
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .any(|token| {
                matches!(
                    token.to_ascii_lowercase().as_str(),
                    "cannot"
                        | "denied"
                        | "error"
                        | "failed"
                        | "failure"
                        | "invalid"
                        | "mismatch"
                        | "missing"
                        | "must"
                        | "not"
                        | "unavailable"
                        | "unexpected"
                )
            })
}

fn exact_field_prefix(prefix: &str) -> Option<ExactFieldV1> {
    match prefix.to_ascii_lowercase().as_str() {
        "id" | "identifier" | "symbol" => Some(ExactFieldV1::Identifier),
        "qualified" | "qualified_name" => Some(ExactFieldV1::QualifiedName),
        "path" => Some(ExactFieldV1::Path),
        "phrase" => Some(ExactFieldV1::QuotedPhrase),
        "diagnostic" | "diagnostic_code" => Some(ExactFieldV1::DiagnosticCode),
        "diagnostic_text" => Some(ExactFieldV1::DiagnosticText),
        "error" => Some(ExactFieldV1::CompilerOrRuntimeError),
        "flag" | "cli" => Some(ExactFieldV1::CliFlag),
        "tool" => Some(ExactFieldV1::ToolName),
        "config" | "configuration" => Some(ExactFieldV1::ConfigurationKey),
        "commit" => Some(ExactFieldV1::CommitIdentifier),
        "task" | "session" => Some(ExactFieldV1::TaskOrSessionId),
        "protocol" | "field" => Some(ExactFieldV1::ProtocolField),
        _ => None,
    }
}

fn classify_unprefixed_exact(value: &str, quoted: bool) -> Option<ExactFieldV1> {
    if quoted {
        return Some(ExactFieldV1::QuotedPhrase);
    }
    if is_cli_flag(value) {
        return Some(ExactFieldV1::CliFlag);
    }
    if is_diagnostic_code(value) {
        return Some(ExactFieldV1::DiagnosticCode);
    }
    if is_commit_identifier(value) {
        return Some(ExactFieldV1::CommitIdentifier);
    }
    if is_qualified_name(value) {
        return Some(ExactFieldV1::QualifiedName);
    }
    if is_path(value) {
        return Some(ExactFieldV1::Path);
    }
    if is_configuration_key(value) {
        return Some(ExactFieldV1::ConfigurationKey);
    }
    if is_known_tool(value) {
        return Some(ExactFieldV1::ToolName);
    }
    is_identifier(value).then_some(ExactFieldV1::Identifier)
}

fn exact_field_accepts(field: ExactFieldV1, value: &str) -> bool {
    if value.is_empty()
        || value.len() > 512
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return false;
    }
    match field {
        ExactFieldV1::Identifier => is_identifier(value),
        ExactFieldV1::QualifiedName => is_qualified_name(value),
        ExactFieldV1::Path => is_path(value),
        ExactFieldV1::QuotedPhrase
        | ExactFieldV1::DiagnosticText
        | ExactFieldV1::CompilerOrRuntimeError => true,
        ExactFieldV1::DiagnosticCode => is_diagnostic_code(value),
        ExactFieldV1::CliFlag => is_cli_flag(value),
        ExactFieldV1::ToolName => value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        ExactFieldV1::ConfigurationKey => is_configuration_key(value),
        ExactFieldV1::CommitIdentifier => is_commit_identifier(value),
        ExactFieldV1::TaskOrSessionId | ExactFieldV1::ProtocolField => value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '/')),
    }
}

fn canonicalize_exact(field: ExactFieldV1, value: &str) -> (Vec<u8>, Vec<String>) {
    let canonical = match field {
        ExactFieldV1::CliFlag
        | ExactFieldV1::ToolName
        | ExactFieldV1::ConfigurationKey
        | ExactFieldV1::CommitIdentifier => value.to_ascii_lowercase(),
        ExactFieldV1::DiagnosticCode => value.to_ascii_uppercase(),
        _ => value.to_owned(),
    };
    let steps = if canonical == value {
        Vec::new()
    } else if field == ExactFieldV1::DiagnosticCode {
        vec!["ascii_uppercase".to_owned()]
    } else {
        vec!["ascii_lowercase".to_owned()]
    };
    (canonical.into_bytes(), steps)
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_qualified_name(value: &str) -> bool {
    value.contains("::") && value.split("::").all(is_identifier)
}

fn is_path(value: &str) -> bool {
    value.contains('/')
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
}

fn is_cli_flag(value: &str) -> bool {
    value.starts_with('-')
        && value.len() > 1
        && value[1..]
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn is_diagnostic_code(value: &str) -> bool {
    (value.len() > 1
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch.eq_ignore_ascii_case(&'e'))
        && value[1..].chars().all(|ch| ch.is_ascii_digit()))
        || (value.len() > 4
            && value[..4].eq_ignore_ascii_case("err_")
            && value[4..]
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
        || (value.get(..2).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("ts") || prefix.eq_ignore_ascii_case("cs")
        }) && value.get(2..).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
        }))
}

fn is_commit_identifier(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn is_configuration_key(value: &str) -> bool {
    value.contains('.')
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        })
}

fn is_known_tool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "cargo" | "rustc" | "tracedecay" | "pytest" | "kubectl" | "fastembed" | "ast-grep" | "git"
    )
}

/// Fixed-point score contributed by one admitted matched literal.
///
/// The exact tier is non-demotable and its identity is the validated proof.
/// The lane's raw score is a deterministic count of admitted literals, never a
/// float or a port-supplied ranking signal.
const ADMITTED_LITERAL_SCORE_MICROS: u64 = 1_000_000;

/// The independent exact-literal lane.
///
/// The lane consumes only whole exact technical terms plus the centrally
/// minted `ExactAdmissionProof`.
///
/// The lane composes the central [`ExactAdmissionAuthority`] with the
/// store-side [`ExactTermPostingReadPort`]. It never constructs an admission
/// proof, never re-derives exact status, and never emits a candidate whose
/// proof the central authority would not mint byte-for-byte. A port-emitted
/// candidate without a valid proof, with a forged proof, or from another lane
/// fails closed as a typed contract violation; it is never silently dropped
/// or substituted.
#[derive(Clone, Debug)]
pub struct ExactLane<A, P> {
    authority: A,
    postings: P,
}

impl<A, P> ExactLane<A, P> {
    pub fn new(authority: A, postings: P) -> Self {
        Self {
            authority,
            postings,
        }
    }
}

impl<A, P> ExactLane<A, P>
where
    A: ExactAdmissionAuthority,
    P: ExactTermPostingReadPort,
{
    /// Confirm the request literals are exactly what the central authority
    /// parses from the base query. The lane never re-parses; it rejects any
    /// request whose literals were not validator-produced.
    fn enforce_request_literals(
        &self,
        request: &ExactLaneRequest<'_>,
    ) -> Result<(), RetrievalPortError> {
        let parsed: BTreeSet<(ExactFieldV1, Vec<u8>, Vec<u8>)> = self
            .authority
            .parse_literals(request.query_view, &request.base)
            .into_iter()
            .map(|literal| {
                (
                    literal.field,
                    literal.original_bytes,
                    literal.canonical_bytes,
                )
            })
            .collect();
        let declared: BTreeSet<(ExactFieldV1, Vec<u8>, Vec<u8>)> = request
            .literals
            .iter()
            .map(|literal| {
                (
                    literal.field,
                    literal.original_bytes.clone(),
                    literal.canonical_bytes.clone(),
                )
            })
            .collect();
        if parsed != declared {
            return Err(RetrievalPortError::Contract(
                "exact lane literals were not parsed by the central admission authority".to_owned(),
            ));
        }
        Ok(())
    }

    /// Validate one port-emitted batch against the request and the central
    /// admission authority, then rebuild the committed deterministic prefix:
    /// canonical order, sequential ordinals, deterministic fixed-point
    /// scores, typed coverage, budget cutoff, and a checkpoint digest.
    fn enforce_batch(
        &self,
        request: &ExactLaneRequest<'_>,
        batch: &RetrieverBatch<ExactLaneEvidence>,
    ) -> Result<RetrieverBatch<ExactLaneEvidence>, RetrievalPortError> {
        batch.validate().map_err(contract_error)?;
        let mut admitted: Vec<(CompactCandidate, ExactLaneEvidence)> =
            Vec::with_capacity(batch.candidates.len());
        for candidate in &batch.candidates {
            let evidence = lane_bound_evidence(
                batch,
                candidate,
                RetrieverKind::ExactLiteral,
                &EXACT_REJECTIONS,
            )?;
            evidence.validate_against_validated_request(request)?;
            let proof = candidate.exact_admission_proof.clone().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "exact lane candidate is missing its admission proof".to_owned(),
                )
            })?;
            if proof != evidence.admission_proof {
                return Err(RetrievalPortError::Contract(
                    "exact candidate proof does not match its evidence proof".to_owned(),
                ));
            }
            if evidence
                .matched_literals
                .iter()
                .any(|literal| !request.literals.contains(literal))
            {
                return Err(RetrievalPortError::Contract(
                    "exact lane evidence matches a literal outside the request".to_owned(),
                ));
            }
            // Only the central authority may mint a proof; re-admission binds
            // this lane to proofs it can never construct itself.
            let minted = self
                .authority
                .admit(proof.field, &proof.original_bytes, &request.base)
                .map_err(contract_error)?
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "the central exact admission authority rejected the proof literal"
                            .to_owned(),
                    )
                })?;
            if minted != proof {
                return Err(RetrievalPortError::Contract(
                    "exact admission proof was not minted by the central authority".to_owned(),
                ));
            }
            admitted.push((candidate.clone(), evidence.clone()));
        }
        // Canonical deterministic order: admitted matched-literal count
        // (descending), then stable occurrence identity, then the evidence
        // anchor. Port emission order can never select a different prefix.
        admitted.sort_by(|left, right| {
            right
                .1
                .matched_literals
                .len()
                .cmp(&left.1.matched_literals.len())
                .then_with(|| {
                    left.0
                        .source_occurrence_id
                        .cmp(&right.0.source_occurrence_id)
                })
                .then_with(|| {
                    left.0
                        .retriever_evidence_anchor
                        .cmp(&right.0.retriever_evidence_anchor)
                })
        });
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let examined = batch.coverage.examined.max(batch.candidates.len() as u64);
        let truncated = admitted.len().saturating_sub(cap);
        admitted.truncate(cap);
        // Preserve the port's own truncation accounting: a port that already
        // capped its batch reported every eligible row and its surplus, so a
        // pre-capped search must stay capped and non-exhausted here instead
        // of being reported complete.
        let eligible = batch
            .coverage
            .eligible
            .max((admitted.len() + truncated) as u64);
        let capped = batch.coverage.capped.saturating_add(truncated as u64);
        let exhausted = truncated == 0 && batch.coverage.capped == 0;
        let mut candidates = Vec::with_capacity(admitted.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence)) in admitted.into_iter().enumerate() {
            candidate.ordinal_rank = ordinal as u32;
            candidate.raw_score = FixedPointScore(
                (evidence.matched_literals.len() as u64)
                    .saturating_mul(ADMITTED_LITERAL_SCORE_MICROS),
            );
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        let checkpoint_digest = exact_checkpoint_digest(&request.generation, &candidates)?;
        let rebuilt = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: RetrieverCoverage {
                examined,
                eligible,
                excluded: batch.coverage.excluded,
                capped,
                unknown: batch.coverage.unknown,
            },
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::ExactLiteral,
                checkpoint_digest,
                exhausted,
            }),
        };
        rebuilt.validate().map_err(contract_error)?;
        Ok(rebuilt)
    }
}

impl<A, P> ExactLaneRetriever for ExactLane<A, P>
where
    A: ExactAdmissionAuthority,
    P: ExactTermPostingReadPort,
{
    fn retrieve_exact(
        &self,
        request: &ExactLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        request.validate()?;
        self.enforce_request_literals(request)?;
        let outcome = match self.postings.read_exact_postings(request) {
            Ok(outcome) => outcome,
            // A missing exact authority rejects the request as a typed
            // unavailable outcome, never a substitution.
            Err(RetrievalPortError::AuthorityUnavailable(detail)) => {
                return Ok(RetrieverOutcome::Unavailable(
                    RetrievalFailure::AuthorityUnavailable { detail },
                ));
            }
            Err(error) => return Err(error),
        };
        match outcome {
            RetrieverOutcome::Complete(batch) => Ok(RetrieverOutcome::Complete(
                self.enforce_batch(request, &batch)?,
            )),
            RetrieverOutcome::Partial { value, reason } => Ok(RetrieverOutcome::Partial {
                value: self.enforce_batch(request, &value)?,
                reason,
            }),
            outcome => Ok(outcome),
        }
    }
}

impl<'a, A, P> Retriever<ExactLaneRequest<'a>, ExactLaneEvidence> for ExactLane<A, P>
where
    A: ExactAdmissionAuthority,
    P: ExactTermPostingReadPort,
{
    fn retrieve(
        &self,
        request: &ExactLaneRequest<'a>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalError> {
        self.retrieve_exact(request)
            .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))
    }
}

impl<'a, A, P> CompactCandidateLane<ExactLaneRequest<'a>, ExactLaneEvidence> for ExactLane<A, P>
where
    A: ExactAdmissionAuthority,
    P: ExactTermPostingReadPort,
{
    fn candidates(
        &self,
        request: &ExactLaneRequest<'a>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.retrieve_exact(request)
    }
}

/// Deterministic digest of the exact lane's committed prefix.
///
/// A lane contributes its admitted prefix with a committed checkpoint; cursor
/// replay binds the completed set and never recomputes it.
fn exact_checkpoint_digest(
    generation: &CodeGenerationId,
    candidates: &[CompactCandidate],
) -> Result<CursorPayloadDigest, RetrievalPortError> {
    checkpoint_digest(&(
        "tracedecay.retrieval-lane-checkpoint.v1",
        RetrieverKind::ExactLiteral.as_str(),
        generation.as_str(),
        candidate_checkpoint_prefix(candidates),
    ))
}

#[cfg(test)]
mod tests;
