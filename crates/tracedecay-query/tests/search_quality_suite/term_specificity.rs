use std::borrow::Cow;
use std::collections::BTreeMap;

use tracedecay_domain::{
    CalibrationProfileId, ComponentRevision, DiversityPolicy, ExactAdmissionRuleRevision,
    ExactClass, FusionProfile, RankedCandidate, RetrieverBatch, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, ScoreDomainCalibrationV1,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageInput,
};
use tracedecay_query::retrieval::graph::GraphLaneEvidence;
use tracedecay_query::retrieval::lexical::{
    LexicalLane, LexicalLaneRequest, LexicalRouteOutcomeV1, LexicalRoutePlanV1, LexicalRoutingV1,
    merge_lexical_routes,
};
use tracedecay_query::retrieval::{
    QUERY_EXACT_RULE_REVISION_V1, QUERY_EXACT_SCORE_DOMAIN_V1, QUERY_GRAPH_SCORE_DOMAIN_V1,
    QUERY_LEXICAL_CALIBRATION_CEILING_MICROS_V1, QUERY_LEXICAL_PROFILE_REVISION_V1,
    QUERY_LEXICAL_SCORE_DOMAIN_V1,
};

use crate::candidate_producers::{
    FixtureRetrievalExecutionControl, base_request, budget, id, query_view, rust_artifact,
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
    let plan =
        LexicalRoutePlanV1::plan(view.as_str(), &LexicalRoutingV1::default()).expect("route plan");
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
fn a_named_identifier_outranks_exact_matches_of_a_common_query_word() {
    let ranked = search(&callers_sources(), "format_relative_time callers");
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
