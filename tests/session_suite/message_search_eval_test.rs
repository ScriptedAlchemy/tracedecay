//! Labeled ranking eval for registered session-message search.
//!
//! This is the retrieval-surface twin of the redundancy scoring eval
//! (`src/redundancy.rs::redundancy_eval_fixture_scores_real_cases` +
//! `tests/fixtures/redundancy_eval_labeled.json`): a JSON fixture of labeled
//! cases, per-case `expect` blocks, and IR metrics (p@1/p@3/MRR) recomputed by
//! the test from the LIVE ranking so the fixture cannot silently disagree with
//! its own corpus.
//!
//! The corpus is seeded into a real per-test global store and every query runs
//! the production `search_session_messages` path (bm25 over
//! `session_messages_fts`), not a reimplementation. Where current master ranks
//! a case worse than the ranking we ultimately want, the case is marked
//! `expected_current_failure: true`: the test asserts the DOCUMENTED current
//! outcome (keeping the suite green today) while printing the aspirational one,
//! forming a known-gap ledger that a later ranking fix must consciously flip.
//!
//! Set `TD_MSG_EVAL_RECORD=1` to print the live ranking of every query and
//! skip assertions — the calibration path used to author/refresh the fixture.

use serde_json::Value;
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_domain::ProjectId;
use tracedecay_sessions::runtime::SessionMessageSearchResult;

use crate::common::{MessageRecordBuilder, global_session as sample_session};

async fn open_isolated_db(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    let profile_root = tmp.path().join("profile");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        ProjectId::new("project.message-search-eval").expect("project id"),
    )
    .await
    .expect("registered project session runtime")
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/message_search_eval_labeled.json"))
        .expect("valid message-search eval fixture")
}

/// Seed every corpus session and message into the store, routing through the
/// production upsert path so the FTS index is populated exactly as ingest would
/// populate it.
async fn seed_corpus(db: &HostAdmissionTestRuntimeV1, fixture: &Value) {
    for session in fixture["corpus"].as_array().expect("corpus") {
        let provider = session["provider"].as_str().expect("session provider");
        let session_id = session["session_id"].as_str().expect("session_id");
        let project_key = session["project_key"].as_str().expect("project_key");
        assert!(
            db.upsert_session_for_test(
                HostAdmissionScope::Project,
                &sample_session(provider, session_id, project_key),
            )
            .await
            .expect("seed registered session"),
            "seed session {session_id}"
        );
        for (ordinal, message) in session["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .enumerate()
        {
            let record = MessageRecordBuilder::new(
                provider,
                message["id"].as_str().expect("message id"),
                session_id,
                message["role"].as_str().unwrap_or("assistant"),
                ordinal as i64,
                message["text"].as_str().expect("message text"),
                message["kind"].as_str().unwrap_or("message"),
            )
            .with_timestamp(message["timestamp"].as_i64())
            .with_tool_names(message["tool_names"].as_str())
            .with_source(Some("/tmp/project/transcript.jsonl"), Some(ordinal as i64))
            .build();
            assert!(
                db.upsert_session_message_for_test(HostAdmissionScope::Project, &record)
                    .await
                    .expect("seed registered session message"),
                "seed message {}",
                message["id"].as_str().unwrap_or("?")
            );
        }
    }
}

/// Run one query case against the live search path, returning the ordered
/// message ids (our per-message labels are the message ids).
async fn run_query(db: &HostAdmissionTestRuntimeV1, case: &Value) -> Vec<String> {
    let provider = case["provider"].as_str().expect("case provider");
    let project_key = case["project_key"].as_str();
    let query = case["query"].as_str().expect("case query");
    let limit = case["limit"].as_u64().unwrap_or(10) as usize;
    let results: Vec<SessionMessageSearchResult> = db
        .search_project_session_messages_for_test(provider, project_key, query, limit)
        .await
        .expect("search registered project session messages");
    results
        .into_iter()
        .map(|hit| hit.message.message_id)
        .collect()
}

fn str_vec(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn index_of(ranked: &[String], id: &str) -> Option<usize> {
    ranked.iter().position(|candidate| candidate == id)
}

fn precision_at_k(ranked: &[String], relevant: &[String], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.iter().any(|rel| rel == *id))
        .count();
    hits as f64 / k as f64
}

fn reciprocal_rank(ranked: &[String], relevant: &[String]) -> f64 {
    for (idx, id) in ranked.iter().enumerate() {
        if relevant.iter().any(|rel| rel == id) {
            return 1.0 / (idx + 1) as f64;
        }
    }
    0.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[tokio::test]
async fn message_search_eval_fixture_scores_live_ranking() {
    let fixture = fixture();
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    seed_corpus(&db, &fixture).await;

    let record_mode = std::env::var_os("TD_MSG_EVAL_RECORD").is_some();

    // Aggregate IR metrics recomputed from the live ranking over the positive
    // (non-negative) queries, mirroring the redundancy eval's recomputed
    // metrics: a fixture whose expected.metrics disagree with its own corpus
    // fails loudly here.
    let mut p1_sum = 0.0;
    let mut p3_sum = 0.0;
    let mut rr_sum = 0.0;
    let mut positive_queries = 0usize;

    let mut seen_labels = std::collections::HashSet::new();

    for case in fixture["queries"].as_array().expect("queries") {
        let label = case["label"].as_str().expect("label");
        assert!(seen_labels.insert(label), "duplicate query label {label}");
        let expect = &case["expect"];
        let ranked = run_query(&db, case).await;

        if record_mode {
            println!("QUERY {label}: query={:?} => {ranked:?}", case["query"]);
            continue;
        }

        // Scoping exclusions: provider/project filters must keep these ids out.
        for excluded in str_vec(expect, "excluded") {
            assert!(
                index_of(&ranked, &excluded).is_none(),
                "case {label}: excluded id {excluded} appeared in {ranked:?}"
            );
        }

        if expect["empty"].as_bool().unwrap_or(false) {
            assert!(
                ranked.is_empty(),
                "case {label}: expected no hits, got {ranked:?}"
            );
            continue;
        }
        assert!(!ranked.is_empty(), "case {label}: expected hits, got none");

        // Exact full-result assertion (used for single-hit cases).
        let exact = str_vec(expect, "result_ids_exact");
        if !exact.is_empty() {
            assert_eq!(ranked, exact, "case {label}: exact result ids");
        }

        // Hard top-1 assertion, honoring the known-gap ledger.
        let gap = expect["expected_current_failure"]
            .as_bool()
            .unwrap_or(false);
        if gap {
            let documented = expect["current_top1"]
                .as_str()
                .expect("current_top1 required when expected_current_failure");
            let aspirational = expect["aspirational_top1"].as_str().unwrap_or("<unset>");
            assert_eq!(
                ranked.first().map(String::as_str),
                Some(documented),
                "case {label}: documented current top1"
            );
            println!(
                "KNOWN GAP {label}: current top1={documented}, aspirational top1={aspirational} \
                 (the ranking fix must flip this and update the fixture)"
            );
        } else if let Some(top1) = expect["top1"].as_str() {
            assert_eq!(
                ranked.first().map(String::as_str),
                Some(top1),
                "case {label}: top1 (ranking {ranked:?})"
            );
        }

        // Top-k membership set (order-agnostic, tie-safe).
        if let Some(top_set) = expect.get("top_set") {
            let k = top_set["k"].as_u64().expect("top_set.k") as usize;
            let want: std::collections::HashSet<String> =
                str_vec(top_set, "ids").into_iter().collect();
            let got: std::collections::HashSet<String> = ranked.iter().take(k).cloned().collect();
            assert_eq!(got, want, "case {label}: top-{k} set");
        }

        // Pairwise ordering constraints.
        if let Some(pairs) = expect.get("ranked_above").and_then(Value::as_array) {
            for pair in pairs {
                let above = pair[0].as_str().expect("ranked_above above");
                let below = pair[1].as_str().expect("ranked_above below");
                let above_idx = index_of(&ranked, above)
                    .unwrap_or_else(|| panic!("case {label}: {above} missing"));
                let below_idx = index_of(&ranked, below)
                    .unwrap_or_else(|| panic!("case {label}: {below} missing"));
                assert!(
                    above_idx < below_idx,
                    "case {label}: {above} (rank {above_idx}) must outrank {below} (rank {below_idx})"
                );
            }
        }

        // Accumulate IR metrics from the live ranking over positive queries.
        let relevant = str_vec(expect, "relevant");
        if !relevant.is_empty() {
            positive_queries += 1;
            p1_sum += precision_at_k(&ranked, &relevant, 1);
            p3_sum += precision_at_k(&ranked, &relevant, 3);
            rr_sum += reciprocal_rank(&ranked, &relevant);
        }
    }

    if record_mode {
        return;
    }

    assert!(positive_queries > 0, "fixture must have positive queries");
    let mean_p1 = round2(p1_sum / positive_queries as f64);
    let mean_p3 = round2(p3_sum / positive_queries as f64);
    let mean_mrr = round2(rr_sum / positive_queries as f64);

    let metrics = &fixture["metrics"];
    for (key, actual) in [
        ("mean_p_at_1", mean_p1),
        ("mean_p_at_3", mean_p3),
        ("mean_mrr", mean_mrr),
    ] {
        let expected = metrics[key]
            .as_f64()
            .unwrap_or_else(|| panic!("metrics.{key}"));
        assert!(
            (actual - expected).abs() < 1e-9,
            "{key}: computed {actual}, fixture expects {expected}"
        );
    }
}
