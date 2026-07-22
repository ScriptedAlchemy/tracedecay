//! End-to-end harness execution test: index the committed corpus snapshot,
//! run the frozen development workload through the existing
//! `tracedecay_search` / `tracedecay_grep` tool behavior, and validate the
//! emitted typed evidence batch.
//!
//! Scope note (pr9/13): the two smoke anchors below are fixture/harness
//! sanity checks — they prove the port executes and the corpus contains
//! what the labels say it contains. They are NOT quality thresholds and
//! assert nothing about ranking quality; no metric is computed and no
//! outcome is claimed in this packet.

use crate::evaluation::{EvalQueryId, EvalRunScopeV1, EvidenceBatchId, RetrieverLaneId, RunId};
use crate::fixtures;
use crate::retrieval::{CONTENT_GREP_LANE, HarnessRetriever, SYMBOL_SEARCH_LANE};
use crate::support::setup_corpus_project;

fn list_for<'a>(
    batch: &'a crate::evaluation::EvidenceBatchV1,
    query_id: &str,
    lane: &str,
) -> &'a crate::evaluation::CandidateListV1 {
    let query_id = EvalQueryId::new(query_id).unwrap();
    let lane = RetrieverLaneId::new(lane).unwrap();
    batch
        .candidate_lists
        .iter()
        .find(|list| list.query_id == query_id && list.lane == lane)
        .unwrap_or_else(|| panic!("missing candidate list for {query_id} on lane {lane}"))
}

#[tokio::test]
async fn development_workload_executes_and_emits_valid_evidence() {
    let manifest = fixtures::load_manifest();
    fixtures::verify_corpus_digests(&manifest);
    let workload = fixtures::load_workload();
    let labels = fixtures::load_development_labels();
    fixtures::validate_labels_against_workload(&labels, &workload, &manifest)
        .expect("labels cross-validate");

    let project = setup_corpus_project(&fixtures::corpus_root()).await;
    let retriever = HarnessRetriever::new(project.cg(), &manifest);

    let batch = retriever
        .run_development_workload(
            &workload,
            &RunId::new("run.search-quality.dev-smoke.v1").unwrap(),
            &EvidenceBatchId::new("batch.search-quality.dev-smoke.001").unwrap(),
        )
        .await;

    // Typed evidence: scope rules, workload binding, and self-verifying digest.
    assert_eq!(batch.scope, EvalRunScopeV1::Development);
    batch
        .validate_against_workload(&workload)
        .expect("evidence batch validates against the workload");
    batch
        .verify_digest()
        .expect("evidence batch digest verifies");

    // Coverage: every development query executed on exactly the two harness
    // lanes; no sealed-holdout query was ever executed.
    for query in workload.development_queries() {
        let lists: Vec<_> = batch
            .candidate_lists
            .iter()
            .filter(|list| list.query_id == query.query_id)
            .collect();
        assert_eq!(
            lists.len(),
            2,
            "query {} must produce one candidate list per lane",
            query.query_id
        );
    }
    assert!(
        batch
            .candidate_lists
            .iter()
            .all(|list| list.query_id.as_str().starts_with("q-dev-")),
        "development scope must never execute a sealed-holdout query"
    );

    // Smoke anchor 1 (harness sanity, not a quality gate): the corpus
    // snapshot provably contains `UtcMicros` in doc-research-time, so a
    // working symbol-search port must surface that document for q-dev-001.
    let exact_symbol = list_for(&batch, "q-dev-001", SYMBOL_SEARCH_LANE);
    assert!(
        exact_symbol
            .candidates
            .iter()
            .any(|candidate| candidate.anchor.document_id.as_str() == "doc-research-time"),
        "harness sanity: symbol search for UtcMicros must surface doc-research-time: {exact_symbol:?}"
    );

    // Smoke anchor 2 (mechanical): a fixed-string content scan of the
    // corpus for a nonexistent token must be empty.
    let no_result = list_for(&batch, "q-dev-017", CONTENT_GREP_LANE);
    assert!(
        no_result.candidates.is_empty(),
        "harness sanity: fixed-string grep for a nonexistent token must be empty: {no_result:?}"
    );

    // Authorization canary observation is recorded for the evidence trail
    // (no threshold asserted at this revision).
    let canary_query = workload
        .query(&EvalQueryId::new("q-dev-021").unwrap())
        .unwrap();
    let canary_hits: usize = batch
        .candidate_lists
        .iter()
        .filter(|list| list.query_id == canary_query.query_id)
        .map(|list| {
            list.forbidden_hits(&canary_query.forbidden_document_ids)
                .count()
        })
        .sum();
    println!("authorization canary forbidden-anchor observations: {canary_hits}");
}
