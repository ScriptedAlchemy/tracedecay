//! Symbol lineage resolution across generations (Plan 25, "Identity and
//! lineage").
//!
//! Lineage is resolved only from evidence the extraction pipeline attests:
//! the exact symbol identity tuple, the content identity digest, and the
//! qualified structure (file identity, qualified name, kind). Tree-sitter
//! object reuse, path, line, qualified-name similarity, or embedding
//! similarity never proves lineage, so a current symbol with no exact
//! evidence emits no candidate at all — lineage is never fabricated.
//!
//! Ambiguity abstains explicitly: when more than one prior symbol could be
//! the ancestor, the resolver emits a candidate with
//! [`LineageMethodV1::DeclaredAbstention`], confidence
//! [`LineageConfidenceKindV1::Abstained`], every alternative occurrence, and
//! a typed abstention reason instead of silently merging unrelated symbols.
//! Digest-only evidence cannot prove a split or a merge; a candidate-count
//! mismatch inside a qualified-structure group abstains with
//! [`ABSTAIN_CANDIDATE_COUNT_MISMATCH`] rather than guessing one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, FileIdentityDigest, LineageAbstentionV1,
    LineageConfidenceKindV1, LineageEvidenceV1, LineageKindV1, LineageMethodV1,
    SymbolIdentityDigest, SymbolLineageCandidateV1, SymbolOccurrenceId, canonical_sha256,
};

/// Domain separator for canonical lineage evidence digests.
pub const LINEAGE_EVIDENCE_SEPARATOR: &str = "tracedecay.code-lineage-evidence.v1";

/// Abstention reason: several prior symbols share the current symbol's
/// content digest and no unique qualified-name match disambiguates them.
pub const ABSTAIN_AMBIGUOUS_CONTENT_MATCH: &str =
    "multiple content-identical prior candidates without a unique qualified-name match";

/// Abstention reason: a byte-identical body has no matching qualified name or
/// file identity, so it cannot prove continuity on its own.
pub const ABSTAIN_CONTENT_ONLY_MATCH: &str =
    "content-identical prior candidate lacks structural continuity";

/// Abstention reason: several prior symbols occupy the same qualified
/// structure group (file identity, qualified name, kind) with matching group
/// sizes, so no unique ancestor is identifiable.
pub const ABSTAIN_AMBIGUOUS_NAME_GROUP: &str =
    "multiple prior candidates in the qualified-structure group";

/// Abstention reason: the prior and current qualified-structure group sizes
/// differ (a possible split or merge); digest-only evidence cannot prove
/// which, so the resolver abstains instead of fabricating split/merge
/// lineage.
pub const ABSTAIN_CANDIDATE_COUNT_MISMATCH: &str =
    "prior and current qualified-structure group sizes differ; possible split or merge";

/// The lineage evidence one symbol occurrence carries within one generation.
/// Every field is extraction-attested: the occurrence and logical identity
/// digests, the qualified structure, and the content identity digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LineageSymbolRecordV1 {
    pub occurrence: SymbolOccurrenceId,
    /// The logical identity tuple (file identity, qualified name, kind,
    /// same-name occurrence index); stable while every declared input is.
    pub identity: SymbolIdentityDigest,
    pub qualified_name: String,
    pub simple_name: String,
    pub kind: String,
    pub visibility: String,
    pub branches: u32,
    pub loops: u32,
    pub max_nesting: u32,
    pub line_span: u32,
    pub start_line: u32,
    pub signature: Option<String>,
    pub skip_test_coverage: bool,
    pub file_identity: FileIdentityDigest,
    pub content_digest: ContentDigest,
}

impl LineageSymbolRecordV1 {
    /// The qualified-structure group key: identity minus the same-name
    /// occurrence index.
    fn group_key(&self) -> (&str, &str, &str) {
        (
            self.file_identity.as_str(),
            self.qualified_name.as_str(),
            self.kind.as_str(),
        )
    }
}

/// The canonically ordered symbol records of one sealed generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSymbolIndexV1 {
    pub generation_id: CodeGenerationId,
    /// Canonically ordered by occurrence identity; duplicates are rejected.
    pub symbols: Vec<LineageSymbolRecordV1>,
}

impl GenerationSymbolIndexV1 {
    /// Build the index, sorting records into canonical occurrence order and
    /// rejecting duplicate occurrences.
    pub fn new(
        generation_id: CodeGenerationId,
        mut symbols: Vec<LineageSymbolRecordV1>,
    ) -> Result<Self, LineageResolutionErrorV1> {
        generation_id
            .validate()
            .map_err(|error| LineageResolutionErrorV1::Contract(error.to_string()))?;
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        if symbols
            .windows(2)
            .any(|pair| pair[0].occurrence == pair[1].occurrence)
        {
            return Err(LineageResolutionErrorV1::DuplicateOccurrence);
        }
        let mut identities = std::collections::BTreeSet::new();
        if symbols
            .iter()
            .any(|symbol| !identities.insert(symbol.identity.clone()))
        {
            return Err(LineageResolutionErrorV1::DuplicateIdentity);
        }
        Ok(Self {
            generation_id,
            symbols,
        })
    }
}

/// Lineage-resolution failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LineageResolutionErrorV1 {
    #[error("lineage requires two distinct generations")]
    SameGeneration,
    #[error("a symbol occurrence appears twice in one generation index")]
    DuplicateOccurrence,
    #[error("a logical symbol identity appears twice in one generation index")]
    DuplicateIdentity,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// The deterministic symbol lineage resolver. Stateless: identical prior and
/// current indexes always produce identical candidates.
pub struct SymbolLineageResolver;

impl SymbolLineageResolver {
    pub fn new() -> Self {
        Self
    }

    /// Resolve lineage candidates for every current symbol against the prior
    /// generation. Candidates are returned in canonical current-occurrence
    /// order; each prior symbol is consumed at most once.
    pub fn resolve(
        &self,
        prior: &GenerationSymbolIndexV1,
        current: &GenerationSymbolIndexV1,
    ) -> Result<Vec<SymbolLineageCandidateV1>, LineageResolutionErrorV1> {
        if prior.generation_id == current.generation_id {
            return Err(LineageResolutionErrorV1::SameGeneration);
        }

        let mut by_identity: BTreeMap<&str, usize> = BTreeMap::new();
        let mut by_content: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        let mut by_group: BTreeMap<(&str, &str, &str), Vec<usize>> = BTreeMap::new();
        for (index, symbol) in prior.symbols.iter().enumerate() {
            by_identity.insert(symbol.identity.as_str(), index);
            by_content
                .entry(symbol.content_digest.as_str())
                .or_default()
                .push(index);
            by_group.entry(symbol.group_key()).or_default().push(index);
        }
        let mut current_group_sizes: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
        for symbol in &current.symbols {
            *current_group_sizes.entry(symbol.group_key()).or_insert(0) += 1;
        }

        let mut consumed = vec![false; prior.symbols.len()];
        let mut candidates = Vec::new();
        for symbol in &current.symbols {
            let resolution = self.resolve_one(
                prior,
                current,
                symbol,
                &by_identity,
                &by_content,
                &by_group,
                &current_group_sizes,
                &mut consumed,
            )?;
            if let Some(candidate) = resolution {
                candidates.push(candidate);
            }
        }
        Ok(candidates)
    }

    /// Resolve one current symbol. Evidence strength decides the method:
    /// exact identity tuple first, then content digest, then qualified
    /// structure. Anything ambiguous abstains; no evidence emits nothing.
    #[allow(clippy::too_many_arguments)]
    fn resolve_one(
        &self,
        prior: &GenerationSymbolIndexV1,
        current: &GenerationSymbolIndexV1,
        symbol: &LineageSymbolRecordV1,
        by_identity: &BTreeMap<&str, usize>,
        by_content: &BTreeMap<&str, Vec<usize>>,
        by_group: &BTreeMap<(&str, &str, &str), Vec<usize>>,
        current_group_sizes: &BTreeMap<(&str, &str, &str), usize>,
        consumed: &mut [bool],
    ) -> Result<Option<SymbolLineageCandidateV1>, LineageResolutionErrorV1> {
        // 1. Exact identity tuple: the declared repository, language,
        //    qualified-structure, and source-evidence tuple is unchanged.
        if let Some(index) = by_identity.get(symbol.identity.as_str())
            && !consumed[*index]
        {
            consumed[*index] = true;
            let ancestor = &prior.symbols[*index];
            let kind = if ancestor.content_digest == symbol.content_digest {
                LineageKindV1::Unchanged
            } else {
                LineageKindV1::StructuralContinuity
            };
            return self.candidate(
                prior,
                current,
                symbol,
                ancestor,
                kind,
                LineageMethodV1::ExactIdentityTuple,
                LineageConfidenceKindV1::Exact,
                vec![],
                None,
            );
        }

        // 2. Content digest plus structural continuity: byte-identical bodies
        //    must also retain either an exact qualified name (move) or exact
        //    file identity (rename), with the same symbol kind. Content alone
        //    never proves lineage.
        let content_candidates: Vec<usize> = by_content
            .get(symbol.content_digest.as_str())
            .map(|indices| {
                indices
                    .iter()
                    .copied()
                    .filter(|index| !consumed[*index])
                    .collect()
            })
            .unwrap_or_default();
        match content_candidates.len() {
            0 => {}
            count => {
                let same_name: Vec<usize> = content_candidates
                    .iter()
                    .copied()
                    .filter(|index| {
                        let ancestor = &prior.symbols[*index];
                        ancestor.kind == symbol.kind
                            && ancestor.qualified_name == symbol.qualified_name
                    })
                    .collect();
                let same_file: Vec<usize> = content_candidates
                    .iter()
                    .copied()
                    .filter(|index| {
                        let ancestor = &prior.symbols[*index];
                        ancestor.kind == symbol.kind
                            && ancestor.file_identity == symbol.file_identity
                    })
                    .collect();
                let selected = if same_name.len() == 1 {
                    Some((same_name[0], LineageKindV1::Moved))
                } else if same_name.is_empty() && same_file.len() == 1 {
                    Some((same_file[0], LineageKindV1::Renamed))
                } else {
                    None
                };
                if let Some((index, kind)) = selected {
                    let confidence = if count == 1 {
                        LineageConfidenceKindV1::Exact
                    } else {
                        LineageConfidenceKindV1::Structural
                    };
                    let alternatives: Vec<SymbolOccurrenceId> = content_candidates
                        .iter()
                        .copied()
                        .filter(|other| *other != index)
                        .map(|other| prior.symbols[other].occurrence.clone())
                        .collect();
                    consumed[index] = true;
                    let ancestor = &prior.symbols[index];
                    return self.candidate(
                        prior,
                        current,
                        symbol,
                        ancestor,
                        kind,
                        LineageMethodV1::ContentDigestMatch,
                        confidence,
                        alternatives,
                        None,
                    );
                }
                let has_structural_continuity =
                    same_name.iter().chain(same_file.iter()).next().is_some();
                return self.abstain(
                    prior,
                    current,
                    symbol,
                    &content_candidates,
                    if has_structural_continuity {
                        ABSTAIN_AMBIGUOUS_CONTENT_MATCH
                    } else {
                        ABSTAIN_CONTENT_ONLY_MATCH
                    },
                    count,
                );
            }
        }

        // 3. Qualified structure: same file identity, qualified name, and
        //    kind, but a shifted identity tuple (a same-name sibling changed
        //    the occurrence index). Never name similarity alone.
        let all_group_candidates: Vec<usize> = by_group
            .get(&symbol.group_key())
            .cloned()
            .unwrap_or_default();
        let current_group_size = current_group_sizes
            .get(&symbol.group_key())
            .copied()
            .unwrap_or(0);
        if !all_group_candidates.is_empty() && all_group_candidates.len() != current_group_size {
            return self.abstain(
                prior,
                current,
                symbol,
                &all_group_candidates,
                ABSTAIN_CANDIDATE_COUNT_MISMATCH,
                all_group_candidates.len().max(current_group_size),
            );
        }

        let group_candidates: Vec<usize> = all_group_candidates
            .into_iter()
            .filter(|index| !consumed[*index])
            .collect();
        match group_candidates.len() {
            0 => Ok(None),
            1 => {
                let index = group_candidates[0];
                consumed[index] = true;
                let ancestor = &prior.symbols[index];
                self.candidate(
                    prior,
                    current,
                    symbol,
                    ancestor,
                    LineageKindV1::StructuralContinuity,
                    LineageMethodV1::QualifiedStructureMatch,
                    LineageConfidenceKindV1::Structural,
                    vec![],
                    None,
                )
            }
            count => self.abstain(
                prior,
                current,
                symbol,
                &group_candidates,
                ABSTAIN_AMBIGUOUS_NAME_GROUP,
                count,
            ),
        }
    }

    /// Build an evidenced candidate.
    #[allow(clippy::too_many_arguments)]
    fn candidate(
        &self,
        prior: &GenerationSymbolIndexV1,
        current: &GenerationSymbolIndexV1,
        symbol: &LineageSymbolRecordV1,
        ancestor: &LineageSymbolRecordV1,
        kind: LineageKindV1,
        method: LineageMethodV1,
        confidence: LineageConfidenceKindV1,
        mut alternatives: Vec<SymbolOccurrenceId>,
        abstention: Option<LineageAbstentionV1>,
    ) -> Result<Option<SymbolLineageCandidateV1>, LineageResolutionErrorV1> {
        alternatives.sort();
        alternatives.dedup();
        let evidence = evidence(
            &prior.generation_id,
            &current.generation_id,
            symbol,
            Some(ancestor),
            kind,
            method,
        )?;
        Ok(Some(SymbolLineageCandidateV1 {
            prior_occurrence: ancestor.occurrence.clone(),
            current_occurrence: symbol.occurrence.clone(),
            kind,
            method,
            evidence,
            confidence,
            alternatives,
            abstention,
        }))
    }

    /// Build an abstaining candidate. The canonically first alternative
    /// occupies `prior_occurrence` so the record stays total; every other
    /// candidate is listed in `alternatives` and the abstention carries the
    /// typed reason and full candidate count.
    fn abstain(
        &self,
        prior: &GenerationSymbolIndexV1,
        current: &GenerationSymbolIndexV1,
        symbol: &LineageSymbolRecordV1,
        candidates: &[usize],
        reason: &str,
        candidate_count: usize,
    ) -> Result<Option<SymbolLineageCandidateV1>, LineageResolutionErrorV1> {
        let mut ordered: Vec<usize> = candidates.to_vec();
        ordered.sort_by(|left, right| {
            prior.symbols[*left]
                .occurrence
                .cmp(&prior.symbols[*right].occurrence)
        });
        let first = ordered[0];
        let alternatives: Vec<SymbolOccurrenceId> = ordered[1..]
            .iter()
            .map(|index| prior.symbols[*index].occurrence.clone())
            .collect();
        let ancestor = &prior.symbols[first];
        let mut candidate = self
            .candidate(
                prior,
                current,
                symbol,
                ancestor,
                LineageKindV1::StructuralContinuity,
                LineageMethodV1::DeclaredAbstention,
                LineageConfidenceKindV1::Abstained,
                alternatives,
                Some(LineageAbstentionV1 {
                    reason: reason.to_owned(),
                    candidate_count: candidate_count as u32,
                }),
            )?
            .expect("an abstaining candidate is always constructed");
        // An abstention attests no chosen ancestor digest.
        candidate.evidence = evidence(
            &prior.generation_id,
            &current.generation_id,
            symbol,
            None,
            candidate.kind,
            candidate.method,
        )?;
        Ok(Some(candidate))
    }
}

impl Default for SymbolLineageResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// The canonical evidence record for one candidate. An abstention carries no
/// prior digest: no ancestor was chosen.
fn evidence(
    prior_generation: &CodeGenerationId,
    current_generation: &CodeGenerationId,
    symbol: &LineageSymbolRecordV1,
    ancestor: Option<&LineageSymbolRecordV1>,
    kind: LineageKindV1,
    method: LineageMethodV1,
) -> Result<LineageEvidenceV1, LineageResolutionErrorV1> {
    let prior_digest = ancestor.map(|record| record.content_digest.clone());
    let evidence_digest = canonical_sha256(&(
        LINEAGE_EVIDENCE_SEPARATOR,
        prior_generation,
        current_generation,
        &symbol.occurrence,
        ancestor.map(|record| &record.occurrence),
        kind,
        method,
        &prior_digest,
        &symbol.content_digest,
    ))
    .map_err(|error| LineageResolutionErrorV1::Contract(error.to_string()))?;
    Ok(LineageEvidenceV1 {
        prior_generation: prior_generation.clone(),
        current_generation: current_generation.clone(),
        prior_digest,
        current_digest: Some(symbol.content_digest.clone()),
        evidence_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn digest(byte: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid digest")
    }

    fn identity(byte: char) -> SymbolIdentityDigest {
        SymbolIdentityDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn file_identity(byte: char) -> FileIdentityDigest {
        FileIdentityDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn generation(sequence: u64) -> CodeGenerationId {
        CodeGenerationId::new(format!("generation.v1.aaaaaaaa.{sequence:08}"))
            .expect("valid generation id")
    }

    fn record(
        occurrence: &str,
        identity_byte: char,
        name: &str,
        kind: &str,
        file_byte: char,
        content_byte: char,
    ) -> LineageSymbolRecordV1 {
        LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("valid occurrence"),
            identity: identity(identity_byte),
            qualified_name: name.to_owned(),
            simple_name: name.rsplit("::").next().unwrap_or(name).to_owned(),
            kind: kind.to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span: 1,
            start_line: 0,
            signature: None,
            skip_test_coverage: false,
            file_identity: file_identity(file_byte),
            content_digest: digest(content_byte),
        }
    }

    fn index(
        generation: CodeGenerationId,
        symbols: Vec<LineageSymbolRecordV1>,
    ) -> GenerationSymbolIndexV1 {
        GenerationSymbolIndexV1::new(generation, symbols).expect("canonical index")
    }

    fn resolver() -> SymbolLineageResolver {
        SymbolLineageResolver::new()
    }

    #[test]
    fn exact_identity_match_classifies_unchanged_and_structural_continuity() {
        let prior = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::beta", "function", 'f', '1'),
            ],
        );
        let current = index(
            generation(2),
            vec![
                // Identical identity tuple and content: unchanged.
                record("sym.c1", 'a', "crate::alpha", "function", 'f', '0'),
                // Identical identity tuple, evolved content: continuity.
                record("sym.c2", 'b', "crate::beta", "function", 'f', '2'),
            ],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert_eq!(candidates.len(), 2);

        let unchanged = &candidates[0];
        assert_eq!(unchanged.prior_occurrence.as_str(), "sym.p1");
        assert_eq!(unchanged.current_occurrence.as_str(), "sym.c1");
        assert_eq!(unchanged.kind, LineageKindV1::Unchanged);
        assert_eq!(unchanged.method, LineageMethodV1::ExactIdentityTuple);
        assert_eq!(unchanged.confidence, LineageConfidenceKindV1::Exact);
        assert!(unchanged.alternatives.is_empty());
        assert!(unchanged.abstention.is_none());
        assert_eq!(unchanged.evidence.prior_digest.as_ref(), Some(&digest('0')));

        let continuity = &candidates[1];
        assert_eq!(continuity.kind, LineageKindV1::StructuralContinuity);
        assert_eq!(continuity.method, LineageMethodV1::ExactIdentityTuple);
        assert_eq!(continuity.confidence, LineageConfidenceKindV1::Exact);
        assert_eq!(
            continuity.evidence.prior_digest.as_ref(),
            Some(&digest('1'))
        );
        assert_eq!(
            continuity.evidence.current_digest.as_ref(),
            Some(&digest('2'))
        );
        assert_eq!(continuity.evidence.prior_generation, generation(1));
        assert_eq!(continuity.evidence.current_generation, generation(2));
    }

    #[test]
    fn content_match_classifies_moved_and_renamed() {
        let prior = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::old_name", "function", 'f', '1'),
            ],
        );
        let current = index(
            generation(2),
            vec![
                // Same name and body, different file identity: moved.
                record("sym.c1", 'c', "crate::alpha", "function", '9', '0'),
                // Different name, same file and body: renamed.
                record("sym.c2", 'd', "crate::new_name", "function", 'f', '1'),
            ],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert_eq!(candidates.len(), 2);

        let moved = &candidates[0];
        assert_eq!(moved.kind, LineageKindV1::Moved);
        assert_eq!(moved.method, LineageMethodV1::ContentDigestMatch);
        assert_eq!(moved.confidence, LineageConfidenceKindV1::Exact);
        assert_eq!(moved.prior_occurrence.as_str(), "sym.p1");

        let renamed = &candidates[1];
        assert_eq!(renamed.kind, LineageKindV1::Renamed);
        assert_eq!(renamed.method, LineageMethodV1::ContentDigestMatch);
        assert_eq!(renamed.confidence, LineageConfidenceKindV1::Exact);
        assert_eq!(renamed.prior_occurrence.as_str(), "sym.p2");
    }

    #[test]
    fn content_only_match_without_structural_continuity_abstains() {
        let prior = index(
            generation(1),
            vec![record(
                "sym.p1",
                'a',
                "crate::unrelated_prior",
                "function",
                'f',
                '0',
            )],
        );
        let current = index(
            generation(2),
            vec![record(
                "sym.c1",
                'b',
                "crate::unrelated_current",
                "function",
                'e',
                '0',
            )],
        );

        let candidates = resolver().resolve(&prior, &current).expect("resolution");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].method, LineageMethodV1::DeclaredAbstention);
        assert_eq!(candidates[0].confidence, LineageConfidenceKindV1::Abstained);
        assert!(candidates[0].evidence.prior_digest.is_none());
    }

    #[test]
    fn ambiguous_content_match_abstains_with_every_alternative() {
        let prior = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::one", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::two", "function", 'f', '0'),
            ],
        );
        let current = index(
            generation(2),
            vec![record("sym.c1", 'c', "crate::three", "function", 'f', '0')],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert_eq!(candidates.len(), 1);

        let abstained = &candidates[0];
        assert_eq!(abstained.method, LineageMethodV1::DeclaredAbstention);
        assert_eq!(abstained.confidence, LineageConfidenceKindV1::Abstained);
        assert_eq!(abstained.kind, LineageKindV1::StructuralContinuity);
        // The canonically first candidate occupies prior_occurrence; the
        // other is an explicit alternative.
        assert_eq!(abstained.prior_occurrence.as_str(), "sym.p1");
        assert_eq!(abstained.alternatives.len(), 1);
        assert_eq!(abstained.alternatives[0].as_str(), "sym.p2");
        let abstention = abstained.abstention.as_ref().expect("abstention recorded");
        assert_eq!(abstention.reason, ABSTAIN_AMBIGUOUS_CONTENT_MATCH);
        assert_eq!(abstention.candidate_count, 2);
        // No ancestor digest is attested for an abstention.
        assert_eq!(abstained.evidence.prior_digest, None);
        assert_eq!(
            abstained.evidence.current_digest.as_ref(),
            Some(&digest('0'))
        );
    }

    #[test]
    fn shared_name_disambiguates_content_matches_with_alternatives_recorded() {
        let prior = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::same", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::other", "function", 'f', '0'),
            ],
        );
        // Same body, same name as one prior, different file: a moved
        // candidate with the name-losing twin recorded as an alternative.
        let current = index(
            generation(2),
            vec![record("sym.c1", 'c', "crate::same", "function", '9', '0')],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert_eq!(candidates.len(), 1);
        let moved = &candidates[0];
        assert_eq!(moved.kind, LineageKindV1::Moved);
        assert_eq!(moved.method, LineageMethodV1::ContentDigestMatch);
        assert_eq!(moved.confidence, LineageConfidenceKindV1::Structural);
        assert_eq!(moved.prior_occurrence.as_str(), "sym.p1");
        assert_eq!(moved.alternatives.len(), 1);
        assert_eq!(moved.alternatives[0].as_str(), "sym.p2");
        assert!(moved.abstention.is_none());
    }

    #[test]
    fn qualified_structure_match_and_count_mismatch_abstains() {
        // Single candidate in the qualified-structure group: continuity via
        // qualified structure (the identity tuple shifted, content evolved).
        let prior = index(
            generation(1),
            vec![record("sym.p1", 'a', "crate::alpha", "function", 'f', '0')],
        );
        let current = index(
            generation(2),
            vec![record("sym.c1", 'b', "crate::alpha", "function", 'f', '1')],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert_eq!(candidates.len(), 1);
        let continuity = &candidates[0];
        assert_eq!(continuity.kind, LineageKindV1::StructuralContinuity);
        assert_eq!(continuity.method, LineageMethodV1::QualifiedStructureMatch);
        assert_eq!(continuity.confidence, LineageConfidenceKindV1::Structural);

        // Two priors in the group against two currents with equal group
        // sizes and no digest evidence: ambiguous name group abstention.
        let prior_two = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::alpha", "function", 'f', '1'),
            ],
        );
        let current_two = index(
            generation(2),
            vec![
                record("sym.c1", 'c', "crate::alpha", "function", 'f', '2'),
                record("sym.c2", 'd', "crate::alpha", "function", 'f', '4'),
            ],
        );
        let candidates = resolver()
            .resolve(&prior_two, &current_two)
            .expect("resolution");
        assert_eq!(candidates.len(), 2);
        for candidate in &candidates {
            assert_eq!(candidate.method, LineageMethodV1::DeclaredAbstention);
            let abstention = candidate.abstention.as_ref().expect("abstention recorded");
            assert_eq!(abstention.reason, ABSTAIN_AMBIGUOUS_NAME_GROUP);
            assert_eq!(abstention.candidate_count, 2);
        }

        // Group sizes differ (two priors, one current): a possible split or
        // merge. Digest evidence cannot prove which, so it abstains — split
        // and merge kinds are never fabricated.
        let prior_split = index(
            generation(1),
            vec![
                record("sym.p1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.p2", 'b', "crate::alpha", "function", 'f', '1'),
            ],
        );
        let current_split = index(
            generation(2),
            vec![record("sym.c1", 'c', "crate::alpha", "function", 'f', '2')],
        );
        let candidates = resolver()
            .resolve(&prior_split, &current_split)
            .expect("resolution");
        assert_eq!(candidates.len(), 1);
        let abstained = &candidates[0];
        assert_eq!(abstained.method, LineageMethodV1::DeclaredAbstention);
        assert_eq!(abstained.confidence, LineageConfidenceKindV1::Abstained);
        assert_ne!(abstained.kind, LineageKindV1::Split);
        assert_ne!(abstained.kind, LineageKindV1::Merged);
        let abstention = abstained.abstention.as_ref().expect("abstention recorded");
        assert_eq!(abstention.reason, ABSTAIN_CANDIDATE_COUNT_MISMATCH);
        assert_eq!(abstention.candidate_count, 2);
    }

    #[test]
    fn no_evidence_emits_no_candidate() {
        // A name-similar prior with different identity, file, and content is
        // not lineage: qualified-name similarity never proves lineage, so a
        // genuinely new symbol emits no candidate at all.
        let prior = index(
            generation(1),
            vec![record("sym.p1", 'a', "crate::alpha", "function", 'f', '0')],
        );
        let current = index(
            generation(2),
            vec![
                record("sym.c1", 'b', "crate::alpha", "function", '9', '1'),
                record("sym.c2", 'c', "crate::brand_new", "function", 'f', '2'),
            ],
        );
        let candidates = resolver().resolve(&prior, &current).expect("resolution");
        assert!(candidates.is_empty());
    }

    #[test]
    fn resolution_is_deterministic_generation_checked_and_serde_round_trips() {
        let prior = index(
            generation(1),
            vec![record("sym.p1", 'a', "crate::alpha", "function", 'f', '0')],
        );
        let current = index(
            generation(2),
            vec![record("sym.c1", 'a', "crate::alpha", "function", 'f', '0')],
        );
        let first = resolver().resolve(&prior, &current).expect("first");
        let second = resolver().resolve(&prior, &current).expect("second");
        assert_eq!(first, second);

        // The evidence digest recomputes from the attested inputs.
        let candidate = &first[0];
        let expected = canonical_sha256(&(
            LINEAGE_EVIDENCE_SEPARATOR,
            &generation(1),
            &generation(2),
            &candidate.current_occurrence,
            Some(&candidate.prior_occurrence),
            candidate.kind,
            candidate.method,
            &candidate.evidence.prior_digest,
            &digest('0'),
        ))
        .expect("evidence digest recomputes");
        assert_eq!(candidate.evidence.evidence_digest, expected);

        let bytes = serde_json::to_vec(&first).expect("serialize");
        let decoded: Vec<SymbolLineageCandidateV1> =
            serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(first, decoded);

        // Lineage across one generation is meaningless.
        assert_eq!(
            resolver().resolve(&prior, &prior),
            Err(LineageResolutionErrorV1::SameGeneration)
        );

        // Duplicate occurrences in one index are non-canonical.
        let duplicate = GenerationSymbolIndexV1::new(
            generation(3),
            vec![
                record("sym.d1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.d1", 'b', "crate::beta", "function", 'f', '1'),
            ],
        );
        assert_eq!(
            duplicate,
            Err(LineageResolutionErrorV1::DuplicateOccurrence)
        );

        // The same logical identity cannot name two generation-local
        // occurrences. Accepting it would let map insertion order choose an
        // ancestor before the resolver can abstain.
        let duplicate_identity = GenerationSymbolIndexV1::new(
            generation(3),
            vec![
                record("sym.d1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.d2", 'a', "crate::alpha", "function", 'f', '0'),
            ],
        );
        assert_eq!(
            duplicate_identity,
            Err(LineageResolutionErrorV1::DuplicateIdentity)
        );

        // Consumption is one-to-one: two identical current symbols cannot
        // both claim the same prior symbol.
        let twins = index(
            generation(4),
            vec![
                record("sym.c1", 'a', "crate::alpha", "function", 'f', '0'),
                record("sym.c2", 'b', "crate::alpha", "function", 'f', '0'),
            ],
        );
        let single_prior = index(
            generation(1),
            vec![record("sym.p1", 'a', "crate::alpha", "function", 'f', '0')],
        );
        let candidates = resolver()
            .resolve(&single_prior, &twins)
            .expect("resolution");
        // One-to-one consumption binds claims, not declared abstentions: an
        // abstention candidate anchors to the ambiguous prior with typed
        // alternatives (domain `prior_occurrence` is non-optional) without
        // claiming it.
        let claims: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.abstention.is_none())
            .collect();
        let claimed: BTreeSet<&str> = claims
            .iter()
            .map(|candidate| candidate.prior_occurrence.as_str())
            .collect();
        assert_eq!(claimed.len(), claims.len());
        // The second twin cannot claim the consumed prior and must abstain.
        assert!(
            candidates.iter().any(
                |candidate| candidate.current_occurrence.as_str() == "sym.c2"
                    && candidate.abstention.is_some()
            ),
            "the unclaimable twin must produce a typed abstention"
        );
    }
}
