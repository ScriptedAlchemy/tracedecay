use std::borrow::Cow;
use std::collections::BTreeMap;

use tracedecay_domain::{
    CalibrationProfileId, ComponentRevision, DiversityPolicy, ExactAdmissionRuleRevision,
    ExactClass, FusionProfile, RankedCandidate, RetrievalSourceRoleV1, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, ScoreDomainCalibrationV1,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageInput,
};
use tracedecay_query::retrieval::graph::GraphLaneEvidence;
use tracedecay_query::retrieval::lexical::{
    LexicalAliasV1, LexicalLane, LexicalLaneRequest, LexicalRouteOutcomeV1, LexicalRoutePlanV1,
    LexicalRoutingV1, merge_lexical_routes,
};
use tracedecay_query::retrieval::{
    QUERY_EXACT_RULE_REVISION_V1, QUERY_EXACT_SCORE_DOMAIN_V1, QUERY_GRAPH_SCORE_DOMAIN_V1,
    QUERY_LEXICAL_CALIBRATION_CEILING_MICROS_V1, QUERY_LEXICAL_PROFILE_REVISION_V1,
    QUERY_LEXICAL_SCORE_DOMAIN_V1,
};

use crate::candidate_producers::{
    FixtureRetrievalExecutionControl, SealedArtifactFixture, base_request, budget, id, query_view,
    real_lexical_source_fixture_from_sources, rust_artifact, sealed_artifact,
};

/// The fixture files that each define an unrelated symbol named `callers`.
const CALLERS_FILES: [&str; 6] = [
    "file.fixture.1",
    "file.fixture.2",
    "file.fixture.3",
    "file.fixture.4",
    "file.fixture.5",
    "file.fixture.6",
];

/// One `format_relative_time` definition, six unrelated symbols named
/// `callers`, and filler functions that name neither.
fn callers_sources() -> Vec<String> {
    with_filler(vec![
        "pub fn format_relative_time(seconds: u64) -> String {\n    format!(\"{seconds}s ago\")\n}\n".to_owned(),
        "/// Every node that calls this one; callers are deduplicated.\npub struct CallGraph {\n    /// The callers of this node.\n    pub callers: Vec<u32>,\n}\n".to_owned(),
        "pub struct Index;\nimpl Index {\n    /// Counts the callers of a symbol.\n    pub fn callers(&self) -> usize {\n        0\n    }\n}\n".to_owned(),
        "/// Lists callers.\npub fn callers() -> Vec<String> {\n    Vec::new()\n}\n".to_owned(),
        "pub struct Report {\n    /// How many callers reached the symbol.\n    pub callers: usize,\n    pub callees: usize,\n}\n".to_owned(),
        "pub trait Graph {\n    /// The callers of a node.\n    fn callers(&self) -> Vec<u64>;\n}\n".to_owned(),
        "pub struct Summary {\n    /// Callers, by name.\n    pub callers: Vec<String>,\n}\n".to_owned(),
    ])
}

/// Filler functions that name nothing a test queries. They make the corpus
/// large enough that every lexical match saturates the lexical calibration,
/// as it does in a real repository.
fn with_filler(mut sources: Vec<String>) -> Vec<String> {
    for file in 0..3 {
        sources.push(
            (0..150)
                .map(|item| {
                    format!(
                        "pub fn filler_{file}_{item}(value: u64) -> u64 {{\n    value + {item}\n}}\n"
                    )
                })
                .collect(),
        );
    }
    sources
}

/// The checked-in core query policy's lane weights and calibrations.
fn core_profile() -> FusionProfile {
    let lanes = [
        (
            RetrieverKind::ExactLiteral,
            QUERY_EXACT_SCORE_DOMAIN_V1,
            1_000_000,
            1_000_000,
        ),
        (
            RetrieverKind::Lexical,
            QUERY_LEXICAL_SCORE_DOMAIN_V1,
            1_000_000,
            QUERY_LEXICAL_CALIBRATION_CEILING_MICROS_V1,
        ),
        (
            RetrieverKind::Graph,
            QUERY_GRAPH_SCORE_DOMAIN_V1,
            250_000,
            1_000_000,
        ),
    ];
    let mut calibrations = BTreeMap::new();
    let mut score_domain_calibrations = BTreeMap::new();
    let mut weights_micros = BTreeMap::new();
    for (lane, score_domain, weight_micros, raw_max_micros) in lanes {
        let calibration: CalibrationProfileId =
            id(&format!("calibration.{}.fixture", lane.as_str()));
        let score_domain: tracedecay_domain::ScoreDomainId = id(score_domain);
        calibrations.insert(lane, calibration.clone());
        score_domain_calibrations.insert(
            score_domain.clone(),
            ScoreDomainCalibrationV1 {
                calibration_profile_id: calibration,
                score_domain,
                raw_min_micros: 0,
                raw_max_micros,
            },
        );
        weights_micros.insert(lane, weight_micros);
    }
    FusionProfile {
        profile_id: id("profile.fixture.v1"),
        evaluation_result_anchor: id("evaluation.fixture"),
        calibrations,
        score_domain_calibrations,
        minimum_calibrated_feature_micros: BTreeMap::new(),
        weights_micros,
        diversity_policy_id: id("diversity.fixture.v1"),
        retrieval_budget: budget(32),
    }
}

fn core_diversity() -> DiversityPolicy {
    DiversityPolicy {
        policy_id: id("diversity.fixture.v1"),
        evaluation_result_anchor: Some(id("evaluation.fixture")),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: Some(2),
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    }
}

/// Runs `query` through the exact lane, every planned lexical route, and
/// composition, the way the daemon's search does.
fn search(sources: &[String], query: &str) -> Vec<RankedCandidate> {
    let artifact = rust_artifact(&sources.iter().map(String::as_str).collect::<Vec<_>>());
    search_artifact(&artifact, query)
}

fn search_artifact(artifact: &SealedArtifactFixture, query: &str) -> Vec<RankedCandidate> {
    search_artifact_with_routing(artifact, query, &LexicalRoutingV1::default())
}

fn search_artifact_with_routing(
    artifact: &SealedArtifactFixture,
    query: &str,
    routing: &LexicalRoutingV1,
) -> Vec<RankedCandidate> {
    let generation = artifact.metadata.generation.clone();
    let request = base_request(query, 32);
    let view = query_view(query);
    let authority = CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>(
        QUERY_EXACT_RULE_REVISION_V1,
    ));
    let exact = ExactLane::new(
        authority.clone(),
        artifact.reader.exact_adapter(authority.clone()),
    )
    .retrieve_exact(&ExactLaneRequest {
        control: &FixtureRetrievalExecutionControl,
        literals: authority.parse_literals(&view, &request),
        path_prefix: None,
        base: request.clone(),
        query_view: &view,
        generation: generation.clone(),
        budget: request.budget,
    })
    .expect("exact lane completes");

    let lexical_lane = LexicalLane::new(artifact.reader.clone());
    let plan = LexicalRoutePlanV1::plan(view.as_str(), routing).expect("route plan");
    let routes = plan
        .routes()
        .iter()
        .map(|route| LexicalRouteOutcomeV1 {
            kind: route.kind.clone(),
            outcome: lexical_lane
                .retrieve_lexical(&LexicalLaneRequest {
                    base: request.clone(),
                    query_view: &view,
                    generation: generation.clone(),
                    whole_terms: Cow::Borrowed(route.parts.whole_terms.as_slice()),
                    explicit_terms: Cow::Borrowed(route.parts.explicit_terms.as_slice()),
                    subtokens: Cow::Borrowed(route.parts.subtokens.as_slice()),
                    phrases: Cow::Borrowed(route.parts.phrases.as_slice()),
                    proximities: Cow::Borrowed(route.proximities.as_slice()),
                    field_filters: Cow::Borrowed(route.field_filters.as_slice()),
                    path_prefix: None,
                    fuzzy_budget: 8,
                    lexical_profile_revision: id(QUERY_LEXICAL_PROFILE_REVISION_V1),
                    score_domain: id(QUERY_LEXICAL_SCORE_DOMAIN_V1),
                    budget: request.budget,
                    control: &FixtureRetrievalExecutionControl,
                })
                .expect("lexical route completes"),
        })
        .collect();
    let (lexical, _) = merge_lexical_routes(&generation, &request.budget, &request.budget, routes)
        .expect("lexical routes merge");
    let graph = RetrieverOutcome::Complete(RetrieverBatch::<GraphLaneEvidence> {
        candidates: Vec::new(),
        evidence_by_occurrence: BTreeMap::new(),
        coverage: RetrieverCoverage::default(),
        continuation: None,
    });

    CompositionKernel::new(id::<ComponentRevision>("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: core_profile(),
                lanes: vec![
                    CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact)
                        .expect("exact lane is admitted"),
                    CompositionLaneInput::new(RetrieverKind::Lexical, lexical)
                        .expect("lexical lane is admitted"),
                    CompositionLaneInput::new(RetrieverKind::Graph, graph)
                        .expect("graph lane is admitted"),
                ],
            },
            &core_diversity(),
        )
        .expect("composition succeeds")
        .ranked_candidates
}

fn file(ranked: &RankedCandidate) -> &str {
    ranked.candidate.occurrences[0]
        .file_occurrence_id
        .as_ref()
        .expect("code candidate names its file")
        .as_str()
}

#[test]
fn a_named_identifier_outranks_an_explicit_exact_common_query_word() {
    let ranked = search(&callers_sources(), "format_relative_time id:callers");
    let summary = ranked
        .iter()
        .map(|ranked| (file(ranked), ranked.candidate.utility_micros))
        .collect::<Vec<_>>();
    assert_eq!(file(&ranked[0]), "file.fixture.0", "{summary:?}");
    let best = ranked[0].candidate.utility_micros;
    let callers = ranked
        .iter()
        .filter(|ranked| {
            ranked.candidate.exact_class != ExactClass::Approximate
                && CALLERS_FILES.contains(&file(ranked))
        })
        .collect::<Vec<_>>();
    assert!(callers.len() >= CALLERS_FILES.len(), "{summary:?}");
    for ranked in callers {
        assert!(
            ranked.candidate.utility_micros * 2 <= best,
            "an exact `callers` row must score at most half the definition's utility so the ranked-tail \
             trim can cut it: {summary:?}"
        );
    }
}

/// One `store_response_handle` definition and unrelated symbols named
/// exactly `store`, `response`, and `handle`.
fn split_word_sources() -> Vec<String> {
    with_filler(vec![
        "pub fn store_response_handle(body: &str) -> String {\n    body.to_owned()\n}\n".to_owned(),
        "pub fn store() -> Vec<u8> {\n    Vec::new()\n}\n".to_owned(),
        "pub struct Cache;\nimpl Cache {\n    pub fn store(&self, key: u64) -> u64 {\n        key\n    }\n}\n".to_owned(),
        "pub fn response() -> u16 {\n    200\n}\n".to_owned(),
        "pub struct Reply {\n    pub response: String,\n}\n".to_owned(),
        "pub fn handle() -> u32 {\n    0\n}\n".to_owned(),
        "pub trait Resource {\n    fn handle(&self) -> u32;\n}\n".to_owned(),
    ])
}

#[test]
fn split_words_rank_the_identifier_they_spell_first() {
    let ranked = search(&split_word_sources(), "store response handle");
    let summary = ranked
        .iter()
        .map(|ranked| (file(ranked), ranked.candidate.utility_micros))
        .collect::<Vec<_>>();
    assert_eq!(file(&ranked[0]), "file.fixture.0", "{summary:?}");
}

#[test]
fn request_limit_question_outranks_unrelated_get_methods() {
    let mut sources = vec![
        "/// Enforces the per-request limit before admitting another operation.\n\
         pub fn enforce_request_limit(remaining: usize) -> bool { remaining > 0 }\n"
            .to_owned(),
    ];
    for index in 0..8 {
        sources.push(format!(
            "pub struct Unrelated{index};\nimpl Unrelated{index} {{\n\
             pub fn get(&self) -> usize {{ {index} }}\n}}\n"
        ));
    }
    let ranked = search(
        &with_filler(sources),
        "how does the per-request limit get enforced",
    );
    assert!(
        !ranked.is_empty(),
        "the documented implementation must be found"
    );
    let summary = ranked
        .iter()
        .map(|ranked| {
            (
                file(ranked),
                ranked.candidate.source_role,
                ranked.candidate.exact_class,
                ranked.candidate.utility_micros,
                ranked
                    .candidate
                    .contributions
                    .iter()
                    .map(|contribution| (contribution.retriever, contribution.raw_score))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(file(&ranked[0]), "file.fixture.0", "{summary:?}");
    assert_eq!(ranked[0].candidate.exact_class, ExactClass::Approximate);
    assert!(
        ranked
            .iter()
            .filter(|ranked| file(ranked) != "file.fixture.0")
            .all(|ranked| ranked.candidate.source_role
                != RetrievalSourceRoleV1::ProductionDefinition),
        "incidental prose words cannot receive protected definition priority: {summary:?}"
    );
}

#[test]
fn request_limit_question_keeps_the_documented_typescript_caller_in_the_page() {
    let mut sources = vec![
        (
            "file.limit".to_owned(),
            "src/limit-cap.ts".to_owned(),
            b"/** Enforces the per-request limit before admitting another operation. */\n\
              export function enforceRequestLimit(remaining: number): boolean {\n\
              \x20   return remaining > 0;\n\
              }\n"
            .to_vec(),
        ),
        (
            "file.caller".to_owned(),
            "src/check.ts".to_owned(),
            b"import { enforceRequestLimit } from './limit-cap';\n\n\
              /** Checks per-request limit enforcement before accepting a request. */\n\
              export function checkRequest(remaining: number): boolean {\n\
              \x20   return enforceRequestLimit(remaining);\n\
              }\n"
            .to_vec(),
        ),
        (
            "file.plan".to_owned(),
            "docs/plan.md".to_owned(),
            b"# Scope\n\nCache storage and expiry.\n\n# Out of Scope\n\nNetwork transports.\n"
                .to_vec(),
        ),
    ];
    for index in 0..8 {
        sources.push((
            format!("file.cache.{index}"),
            format!("scripts/cache_{index}.py"),
            format!("class Cache{index}:\n    def get(self):\n        return {index}\n")
                .into_bytes(),
        ));
        sources.push((
            format!("file.unrelated.{index}"),
            format!("src/unrelated_{index}.ts"),
            format!("export const cap = {index};\n").into_bytes(),
        ));
    }
    sources.sort_by(|left, right| left.1.cmp(&right.1));
    let fixture = real_lexical_source_fixture_from_sources(sources);
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let ranked = search_artifact(&artifact, "how does the per-request limit get enforced");
    let summary = ranked
        .iter()
        .map(|ranked| {
            (
                file(ranked),
                ranked.candidate.source_role,
                ranked.candidate.exact_class,
                ranked.candidate.utility_micros,
                ranked
                    .candidate
                    .contributions
                    .iter()
                    .map(|contribution| (contribution.retriever, contribution.raw_score))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    for required in ["file.limit", "file.caller"] {
        assert!(
            ranked.iter().take(6).any(|ranked| file(ranked) == required),
            "the six-result question page must retain {required}: {summary:?}"
        );
    }
    assert!(
        matches!(ranked.first().map(file), Some("file.limit" | "file.caller")),
        "the documented implementation must outrank unrelated get methods: {summary:?}"
    );

    let named = search_artifact(&artifact, "enforceRequestLimit");
    assert_eq!(file(&named[0]), "file.limit");
    assert_ne!(named[0].candidate.exact_class, ExactClass::Approximate);
    let generic = search_artifact(&artifact, "get");
    assert!(file(&generic[0]).starts_with("file.cache."));
    assert_ne!(generic[0].candidate.exact_class, ExactClass::Approximate);
    assert_eq!(
        generic[0].candidate.source_role,
        RetrievalSourceRoleV1::ProductionDefinition,
        "a direct get query must retain definition admission"
    );
}

#[test]
fn prose_articles_do_not_spend_file_slots_on_rust_lifetimes() {
    let sources = with_filler(vec![
        "/// Enforces capacity by rejecting an operation after all slots are occupied.\n\
         pub fn allow_next(remaining: usize) -> bool { remaining > 0 }\n\n\
         pub struct Borrowed<T>(Vec<T>);\n\
         impl<'a, T> IntoIterator for &'a mut Borrowed<T> {\n\
         \x20   type Item = &'a mut T;\n\
         \x20   type IntoIter = std::slice::IterMut<'a, T>;\n\
         \x20   fn into_iter(self) -> Self::IntoIter { self.0.iter_mut() }\n\
         }\n"
        .to_owned(),
        "pub fn a() -> bool { true }\n".to_owned(),
    ]);
    let artifact = rust_artifact(&sources.iter().map(String::as_str).collect::<Vec<_>>());
    let named = search_artifact(&artifact, "allow_next");
    let answer = &named.first().expect("the named gate is indexed").candidate;
    assert_eq!(file(&named[0]), "file.fixture.0");
    assert_eq!(
        answer.source_role,
        RetrievalSourceRoleV1::ProductionDefinition
    );
    assert_ne!(answer.exact_class, ExactClass::Approximate);

    let lifetime_aliases = ["Item", "IntoIter"]
        .into_iter()
        .flat_map(|name| search_artifact(&artifact, name))
        .map(|ranked| ranked.candidate.anchor_id)
        .collect::<Vec<_>>();
    assert!(
        !lifetime_aliases.is_empty(),
        "the lifetime declarations must be indexed for the isolation check"
    );

    let ranked = search_artifact(&artifact, "how is a request kept within the capacity limit");
    let summary = ranked
        .iter()
        .map(|ranked| {
            (
                &ranked.candidate.anchor_id,
                file(ranked),
                ranked.candidate.source_role,
                &ranked.candidate.contributions,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ranked.first().map(|ranked| &ranked.candidate.anchor_id),
        Some(&answer.anchor_id),
        "the documented gate must retain its file's result slot ahead of lifetime noise: {summary:?}"
    );
    assert!(
        ranked
            .iter()
            .all(|ranked| !lifetime_aliases.contains(&ranked.candidate.anchor_id)),
        "a prose article is not a request for the same-spelled Rust lifetime: {summary:?}"
    );

    let direct = search_artifact(&artifact, "a");
    let explicit = &direct
        .first()
        .expect("the ordinary name is indexed")
        .candidate;
    assert_eq!(file(&direct[0]), "file.fixture.1");
    for query in ["a", "find id:a"] {
        let ranked = search_artifact(&artifact, query);
        let first = &ranked
            .first()
            .expect("explicit intent remains searchable")
            .candidate;
        assert_eq!(first.anchor_id, explicit.anchor_id, "{query}: {ranked:?}");
        assert_ne!(first.exact_class, ExactClass::Approximate, "{query}");
        assert!(
            first.contributions.iter().any(|contribution| {
                contribution.retriever == RetrieverKind::Lexical && contribution.raw_score.0 > 0
            }),
            "explicit terms must remain lexical evidence as well as exact matches: {query}: {ranked:?}"
        );
    }

    let literal_artifact = rust_artifact(&["pub fn token() -> &'static str { \"a\" }\n"]);
    let quoted = search_artifact(&literal_artifact, "where is \"a\" defined");
    assert!(
        quoted.iter().any(|ranked| {
            file(ranked) == "file.fixture.0"
                && ranked.candidate.contributions.iter().any(|contribution| {
                    contribution.retriever == RetrieverKind::Lexical && contribution.raw_score.0 > 0
                })
        }),
        "a quoted article must remain lexical evidence in its source file: {quoted:?}"
    );
}

#[test]
fn a_one_content_word_question_keeps_prose_definition_priority() {
    let artifact = rust_artifact(&[
        "/// Tracks remaining capacity before accepting work.\n\
         pub fn reserve_slot(remaining: usize) -> bool { remaining > 0 }\n",
        "pub fn capacity() -> usize { 0 }\n",
    ]);
    let direct = search_artifact(&artifact, "capacity");
    assert_eq!(file(&direct[0]), "file.fixture.1");
    assert_eq!(
        direct[0].candidate.source_role,
        RetrievalSourceRoleV1::ProductionDefinition,
        "a direct name owns definition priority"
    );

    let prose = search_artifact(&artifact, "what is capacity");
    assert!(
        prose.iter().any(|ranked| file(ranked) == "file.fixture.0"),
        "the question must retain documented evidence: {prose:?}"
    );
    assert!(
        prose.iter().all(|ranked| {
            ranked.candidate.exact_class == ExactClass::Approximate
                && ranked.candidate.source_role != RetrievalSourceRoleV1::ProductionDefinition
        }),
        "removing prose connectives cannot manufacture explicit name intent: {prose:?}"
    );
}

#[test]
fn uppercase_names_remain_lexical_evidence_inside_prose() {
    let artifact = rust_artifact(&[
        "pub const ALL: bool = true;\n",
        "pub fn selected() -> bool { ALL }\n",
    ]);
    let ranked = search_artifact(&artifact, "where is ALL used");
    assert!(
        ranked.iter().any(|ranked| {
            file(ranked) == "file.fixture.1"
                && ranked.candidate.contributions.iter().any(|contribution| {
                    contribution.retriever == RetrieverKind::Lexical && contribution.raw_score.0 > 0
                })
        }),
        "a body-only uppercase name must retain lexical evidence even when its lowercase spelling is a prose connective: {ranked:?}"
    );
}

#[test]
fn alias_routes_preserve_their_own_explicit_literal_intent() {
    let artifact = rust_artifact(&["pub fn token() -> &'static str { \"all\" }\n"]);
    let query = "default scope";
    let strict = search_artifact(&artifact, query);
    assert!(
        strict.is_empty(),
        "the strict query must not supply the alias's literal evidence: {strict:?}"
    );

    let routing = LexicalRoutingV1::default()
        .with_aliases(vec![LexicalAliasV1 {
            strict_query: query.to_owned(),
            alternative: "where is \"all\" defined".to_owned(),
        }])
        .expect("the explicit literal alias is valid");
    let ranked = search_artifact_with_routing(&artifact, query, &routing);
    assert!(
        ranked.iter().any(|ranked| {
            file(ranked) == "file.fixture.0"
                && ranked.candidate.contributions.iter().any(|contribution| {
                    contribution.retriever == RetrieverKind::Lexical && contribution.raw_score.0 > 0
                })
        }),
        "an alias's quoted term must survive independently of the original query's syntax: {ranked:?}"
    );
    assert!(
        ranked
            .iter()
            .all(|ranked| ranked.candidate.exact_class == ExactClass::Approximate),
        "a lexical alias must not mint exact admission: {ranked:?}"
    );

    let prose_routing = LexicalRoutingV1::default()
        .with_aliases(vec![LexicalAliasV1 {
            strict_query: query.to_owned(),
            alternative: "where is all defined".to_owned(),
        }])
        .expect("the prose alias is valid");
    let prose = search_artifact_with_routing(&artifact, query, &prose_routing);
    assert!(
        prose.is_empty(),
        "a generated prose phrase must not grant explicit literal intent: {prose:?}"
    );
}
