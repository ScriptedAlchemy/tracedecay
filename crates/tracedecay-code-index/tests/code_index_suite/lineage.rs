use std::fmt::Debug;

use tracedecay_code_index::lineage::{
    ABSTAIN_CANDIDATE_COUNT_MISMATCH, GenerationSymbolIndexV1, LineageSymbolRecordV1,
    SymbolLineageResolver,
};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, FileIdentityDigest, LineageConfidenceKindV1, LineageKindV1,
    LineageMethodV1, SymbolIdentityDigest, SymbolOccurrenceId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn generation(sequence: u64) -> CodeGenerationId {
    id(&format!("generation.v1.aaaaaaaa.{sequence:08}"))
}

fn symbol(
    occurrence: &str,
    identity: char,
    qualified_name: &str,
    file_identity: char,
    content: char,
) -> LineageSymbolRecordV1 {
    LineageSymbolRecordV1 {
        occurrence: id::<SymbolOccurrenceId>(occurrence),
        identity: digest::<SymbolIdentityDigest>(identity),
        qualified_name: qualified_name.to_owned(),
        kind: "function".to_owned(),
        file_identity: digest::<FileIdentityDigest>(file_identity),
        content_digest: digest::<ContentDigest>(content),
    }
}

fn index(sequence: u64, symbols: Vec<LineageSymbolRecordV1>) -> GenerationSymbolIndexV1 {
    GenerationSymbolIndexV1::new(generation(sequence), symbols).expect("canonical symbol index")
}

#[test]
fn exact_content_evidence_classifies_moves_and_renames() {
    let prior = index(
        1,
        vec![
            symbol("symbol.prior.move", 'a', "crate::move_me", 'f', '0'),
            symbol("symbol.prior.rename", 'b', "crate::old_name", 'f', '1'),
        ],
    );
    let current = index(
        2,
        vec![
            symbol("symbol.current.move", 'c', "crate::move_me", '9', '0'),
            symbol("symbol.current.rename", 'd', "crate::new_name", 'f', '1'),
        ],
    );

    let candidates = SymbolLineageResolver::new()
        .resolve(&prior, &current)
        .expect("lineage resolves");

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].kind, LineageKindV1::Moved);
    assert_eq!(candidates[0].method, LineageMethodV1::ContentDigestMatch);
    assert_eq!(candidates[1].kind, LineageKindV1::Renamed);
    assert_eq!(candidates[1].method, LineageMethodV1::ContentDigestMatch);
}

#[test]
fn content_digest_without_continuity_abstains() {
    let prior = index(
        1,
        vec![symbol(
            "symbol.prior",
            'a',
            "crate::unrelated_prior",
            'f',
            '0',
        )],
    );
    let current = index(
        2,
        vec![symbol(
            "symbol.current",
            'b',
            "crate::unrelated_current",
            'e',
            '0',
        )],
    );

    let candidates = SymbolLineageResolver::new()
        .resolve(&prior, &current)
        .expect("lineage resolution remains explicit");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].method, LineageMethodV1::DeclaredAbstention);
    assert_eq!(candidates[0].confidence, LineageConfidenceKindV1::Abstained);
    assert!(candidates[0].evidence.prior_digest.is_none());
}

#[test]
fn possible_split_abstains_for_each_current_symbol_instead_of_consuming_one_ancestor() {
    let prior = index(
        1,
        vec![symbol("symbol.prior", 'a', "crate::shared", 'f', '0')],
    );
    let current = index(
        2,
        vec![
            symbol("symbol.current.one", 'b', "crate::shared", 'f', '1'),
            symbol("symbol.current.two", 'c', "crate::shared", 'f', '2'),
        ],
    );

    let candidates = SymbolLineageResolver::new()
        .resolve(&prior, &current)
        .expect("lineage resolves");

    assert_eq!(candidates.len(), 2);
    for candidate in candidates {
        assert_eq!(candidate.method, LineageMethodV1::DeclaredAbstention);
        assert_eq!(candidate.confidence, LineageConfidenceKindV1::Abstained);
        assert_ne!(candidate.kind, LineageKindV1::Split);
        let abstention = candidate.abstention.expect("abstention evidence");
        assert_eq!(abstention.reason, ABSTAIN_CANDIDATE_COUNT_MISMATCH);
        assert_eq!(abstention.candidate_count, 2);
        assert!(candidate.evidence.prior_digest.is_none());
    }
}

#[test]
fn possible_merge_abstains_with_all_prior_alternatives() {
    let prior = index(
        1,
        vec![
            symbol("symbol.prior.one", 'a', "crate::shared", 'f', '0'),
            symbol("symbol.prior.two", 'b', "crate::shared", 'f', '1'),
        ],
    );
    let current = index(
        2,
        vec![symbol("symbol.current", 'c', "crate::shared", 'f', '2')],
    );

    let candidates = SymbolLineageResolver::new()
        .resolve(&prior, &current)
        .expect("lineage resolves");

    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate.method, LineageMethodV1::DeclaredAbstention);
    assert_eq!(candidate.confidence, LineageConfidenceKindV1::Abstained);
    assert_ne!(candidate.kind, LineageKindV1::Merged);
    assert_eq!(candidate.alternatives.len(), 1);
    let abstention = candidate.abstention.as_ref().expect("abstention evidence");
    assert_eq!(abstention.reason, ABSTAIN_CANDIDATE_COUNT_MISMATCH);
    assert_eq!(abstention.candidate_count, 2);
}
