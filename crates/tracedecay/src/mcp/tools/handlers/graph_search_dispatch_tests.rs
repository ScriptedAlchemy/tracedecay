//! Graph search journeys that mount `TraceDecay` stay in the composition root.

use std::collections::HashMap;
use std::future::Future;

use serde_json::{Value, json};
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader;
use tracedecay_domain::ExactClass;
use tracedecay_mcp::ToolResult;
use tracedecay_query::retrieval::lexical::LexicalRoutingV1;

use crate::project::TraceDecay;

fn completed_sparse_search() -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    completed_sparse_search_for_generation("generation.mcp-verified-graph-fixture.1")
}

fn completed_sparse_search_for_generation(
    generation: &str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    let candidate = tracedecay_domain::RankedCandidate {
        candidate: tracedecay_domain::FusedCandidate {
            anchor_id: tracedecay_domain::RetrievalAnchorId::new(
                "code-symbol:sparse-lexical-widget",
            )
            .expect("sparse lexical candidate anchor"),
            logical_evidence_id: tracedecay_domain::LogicalEvidenceId::new(
                "logical.sparse-lexical-widget",
            )
            .expect("sparse lexical candidate logical evidence"),
            occurrences: Vec::new(),
            exact_class: ExactClass::Approximate,
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: Vec::new(),
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    };
    let fallback_coverage = tracedecay_domain::RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .map(|lane| (lane, tracedecay_domain::PublicRetrieverStatus::Complete))
        .collect();
    let query_fallback = tracedecay_domain::QueryFallbackSubpayload::new(
        tracedecay_domain::FusionProfileId::new("profile.sparse-search")
            .expect("sparse search profile"),
        vec![candidate.clone()],
        fallback_coverage,
        Vec::new(),
        None,
    )
    .expect("canonical sparse lexical fallback payload");
    let anchor = candidate.candidate.anchor_id.clone();
    let semantic = tracedecay_query::code_search::CodeIndexSemanticStatusV1::Unavailable {
        reason: "semantic_generation_warming",
    };
    tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(
        tracedecay_query::code_search::CodeIndexSearchCompletedV1 {
            code_generation: generation.to_owned(),
            ordered_candidates: vec![candidate],
            query_fallback: std::sync::Arc::new(query_fallback),
            display_by_anchor: HashMap::from([(
                anchor,
                tracedecay_query::code_search::CodeIndexSearchDisplayV1 {
                    name: "SparseLexicalWidget".to_owned(),
                    qualified_name: "crate::SparseLexicalWidget".to_owned(),
                    kind: "function".to_owned(),
                    path: "src/lib.rs".to_owned(),
                },
            )]),
            coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::fused(&semantic),
            semantic,
            next_cursor: None,
            lexical_routes: tracedecay_query::retrieval::lexical::LexicalRouteReceiptV1 {
                routes: vec![tracedecay_query::retrieval::lexical::LexicalRouteKindV1::Query],
                matches_by_anchor: std::collections::BTreeMap::new(),
            },
        },
    )
}

fn unavailable_search() -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
        tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
            code_generation: Some("generation.mcp-verified-graph-fixture.1".to_owned()),
            reason:
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
            semantic: tracedecay_query::code_search::CodeIndexSemanticStatusV1::Unavailable {
                reason: "search_attempt_repeated",
            },
            coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::unavailable(
                "search_attempt_repeated",
            ),
        },
    )
}

fn search_test_options<'a>(
    cg: &TraceDecay,
    executor: tracedecay_query::code_search::CodeIndexSearchExecutor,
) -> crate::mcp::tools::handlers::ToolCallRegistryOptions<'a> {
    crate::mcp::tools::handlers::dispatch_test_support::verified_graph_options(
        cg,
        crate::mcp::tools::handlers::ToolCallRegistryOptions {
            code_index_search_executor: Some(executor),
            code_index_search_authority: Some(
                tracedecay_query::code_search::CodeIndexSearchAuthorityV1 {
                    principal: tracedecay_domain::PrincipalId::new("principal.search-attempt-test")
                        .expect("search attempt principal"),
                    authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                        "authorization.search-attempt-test",
                    )
                    .expect("search attempt authorization revision"),
                },
            ),
            ..crate::mcp::tools::handlers::ToolCallRegistryOptions::default()
        },
    )
}

fn run_with_locked_user_data_dir(test: impl Future<Output = ()>) {
    let _env_lock = crate::config::lock_user_data_dir_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("locked user data test runtime")
        .block_on(test);
}

#[test]
fn completed_primary_search_is_not_retried_after_graph_admission() {
    run_with_locked_user_data_dir(
        completed_primary_search_is_not_retried_after_graph_admission_case(),
    );
}

async fn completed_primary_search_is_not_retried_after_graph_admission_case() {
    let dir = tempfile::TempDir::new().expect("single search attempt isolation");
    let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
    let project = dir.path().join("single-search-attempt");
    std::fs::create_dir_all(project.join("src")).expect("create search attempt sources");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn SparseLexicalWidget() {}\n",
    )
    .expect("write search attempt fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.single-search-attempt",
    )
    .await
    .expect("registered search attempt fixture");

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = std::sync::Arc::clone(&calls);
    let executor: tracedecay_query::code_search::CodeIndexSearchExecutor =
        std::sync::Arc::new(move |_| {
            let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                if attempt == 0 {
                    completed_sparse_search()
                } else {
                    unavailable_search()
                }
            })
        });
    let options = search_test_options(&cg, executor);

    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "SparseLexicalWidget",
            "limit": 5,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("first complete search outcome must remain authoritative");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("single-attempt search JSON text"),
    )
    .expect("single-attempt search JSON payload");

    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        payload["results"][0]["display"]["name"],
        "SparseLexicalWidget"
    );
    assert!(payload["status"].is_null());
    cg.close();
}

/// A strict-semantic request the runtime cannot honour is refused: the
/// payload stays typed (`status: "unavailable"`, the reason, the semantic
/// lane's own status) and the call is flagged as a tool-level error — not
/// a JSON-RPC failure, and not an empty page passed off as success. The
/// same outcome under `fallback_allowed` is a degraded answer, not a
/// refusal.
#[test]
fn strict_semantic_unavailability_is_a_typed_refusal() {
    run_with_locked_user_data_dir(strict_semantic_unavailability_is_a_typed_refusal_case());
}

async fn strict_semantic_unavailability_is_a_typed_refusal_case() {
    let dir = tempfile::TempDir::new().expect("strict refusal isolation");
    let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
    let project = dir.path().join("strict-semantic-refusal");
    std::fs::create_dir_all(project.join("src")).expect("create strict refusal sources");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn SparseLexicalWidget() {}\n",
    )
    .expect("write strict refusal fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.strict-semantic-refusal",
    )
    .await
    .expect("registered strict refusal fixture");

    let executor: tracedecay_query::code_search::CodeIndexSearchExecutor = std::sync::Arc::new(
        move |_| {
            Box::pin(async {
                tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
                    tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(
                            "generation.mcp-verified-graph-fixture.1".to_owned(),
                        ),
                        reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::SemanticUnavailable,
                        semantic: tracedecay_query::code_search::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "calibration_unavailable",
                        },
                        coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::unavailable(
                            "calibration_unavailable",
                        ),
                    },
                )
            })
        },
    );

    for (semantic_mode, refused) in [("strict_semantic", true), ("fallback_allowed", false)] {
        let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_search",
            json!({
                "query": "SparseLexicalWidget",
                "limit": 5,
                "format": "json",
                "semantic_mode": semantic_mode,
            }),
            None,
            None,
            search_test_options(&cg, std::sync::Arc::clone(&executor)),
        )
        .await
        .expect("an unavailable search answers with a typed result, not a hard error");
        let payload: Value = serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("unavailable search JSON text"),
        )
        .expect("unavailable search JSON payload");

        assert_eq!(
            result.semantic_error() == Some(true),
            refused,
            "{semantic_mode}: refusal flag mismatch for {payload}"
        );
        assert_eq!(payload["status"], "unavailable", "{semantic_mode}");
        assert_eq!(payload["reason"], "semantic_unavailable", "{semantic_mode}");
        assert_eq!(payload["semantic"]["mode"], semantic_mode);
        assert_eq!(
            payload["semantic"]["status"], "unavailable",
            "{semantic_mode}"
        );
        assert_eq!(
            payload["semantic"]["reason"], "calibration_unavailable",
            "{semantic_mode}"
        );
        assert_eq!(payload["results"], json!([]), "{semantic_mode}");
        assert_eq!(
            payload["query_fallback_digest"],
            Value::Null,
            "{semantic_mode}"
        );
        assert_eq!(
            result.failure_message(),
            Some("code-index search unavailable: semantic_unavailable"),
            "{semantic_mode}"
        );
    }
    cg.close();
}

#[test]
fn generation_mismatch_retry_cannot_erase_a_complete_sparse_search() {
    run_with_locked_user_data_dir(
        generation_mismatch_retry_cannot_erase_a_complete_sparse_search_case(),
    );
}

async fn generation_mismatch_retry_cannot_erase_a_complete_sparse_search_case() {
    let dir = tempfile::TempDir::new().expect("generation mismatch isolation");
    let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
    let project = dir.path().join("generation-mismatch-search");
    std::fs::create_dir_all(project.join("src")).expect("create mismatch search sources");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn SparseLexicalWidget() {}\n",
    )
    .expect("write mismatch search fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.generation-mismatch-search",
    )
    .await
    .expect("registered mismatch search fixture");

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = std::sync::Arc::clone(&calls);
    let executor: tracedecay_query::code_search::CodeIndexSearchExecutor =
        std::sync::Arc::new(move |_| {
            let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                if attempt == 0 {
                    completed_sparse_search_for_generation("generation.search-before-graph")
                } else {
                    unavailable_search()
                }
            })
        });
    let options = search_test_options(&cg, executor);

    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "SparseLexicalWidget",
            "limit": 5,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("failed refresh must preserve the first complete search");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("generation mismatch JSON text"),
    )
    .expect("generation mismatch JSON payload");

    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        payload["results"][0]["display"]["name"],
        "SparseLexicalWidget"
    );
    assert_eq!(
        payload["verified_graph_evidence"]["reason_code"],
        "verified-code-graph-generation-mismatch"
    );
    cg.close();
}

fn freshness_reader(
    latest_generation_id: Option<&str>,
    staleness_state: &str,
    rebuild_in_flight: bool,
) -> CodeIndexFreshnessReader {
    let latest_generation_id = latest_generation_id.map(str::to_owned);
    let staleness_state = staleness_state.to_owned();
    std::sync::Arc::new(move |worktree_root: std::path::PathBuf| {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: worktree_root.display().to_string(),
            latest_generation_id: latest_generation_id.clone(),
            staleness_state: Some(staleness_state.clone()),
            rebuild_in_flight,
            hook_hint_count: Some(0),
            coverage: "complete".to_owned(),
            ..Default::default()
        };
        Box::pin(async move { Some(freshness) })
    })
}

/// A completed sparse search whose lexical lane ran the query route plus
/// one anchor route that ranked the single result.
fn completed_sparse_search_with_anchor_route(
    anchor: &str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    let tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(mut complete) =
        completed_sparse_search()
    else {
        panic!("sparse search fixture is complete");
    };
    let routing = LexicalRoutingV1::new(vec![anchor.to_owned()], false).expect("anchor");
    let route = tracedecay_query::retrieval::lexical::LexicalRouteKindV1::Anchor {
        anchor: routing.anchors[0].clone(),
    };
    let candidate_anchor = complete.ordered_candidates[0].candidate.anchor_id.clone();
    complete.lexical_routes = tracedecay_query::retrieval::lexical::LexicalRouteReceiptV1 {
        routes: vec![
            tracedecay_query::retrieval::lexical::LexicalRouteKindV1::Query,
            route.clone(),
        ],
        matches_by_anchor: std::collections::BTreeMap::from([(
            candidate_anchor,
            vec![tracedecay_query::retrieval::lexical::LexicalRouteMatchV1 {
                route,
                score_micros: 900_000,
                matched_terms: vec![anchor.to_owned()],
            }],
        )]),
    };
    tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete)
}

fn response_text(result: &ToolResult) -> String {
    result.value["content"][0]["text"]
        .as_str()
        .expect("tool response text")
        .to_owned()
}

#[test]
fn search_opens_with_a_freshness_verdict_from_typed_state() {
    run_with_locked_user_data_dir(search_opens_with_a_freshness_verdict_from_typed_state_case());
}

async fn search_opens_with_a_freshness_verdict_from_typed_state_case() {
    let dir = tempfile::TempDir::new().expect("freshness verdict isolation");
    let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
    let project = dir.path().join("freshness-verdict-search");
    std::fs::create_dir_all(project.join("src")).expect("create freshness sources");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn SparseLexicalWidget() {}\n",
    )
    .expect("write freshness fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.freshness-verdict-search",
    )
    .await
    .expect("registered freshness fixture");
    let executor: tracedecay_query::code_search::CodeIndexSearchExecutor =
        std::sync::Arc::new(|_| Box::pin(async { completed_sparse_search() }));

    let settled = crate::mcp::tools::handlers::ToolCallRegistryOptions {
        code_index_freshness_reader: Some(freshness_reader(
            Some("generation.mcp-verified-graph-fixture.1"),
            "fresh",
            false,
        )),
        ..search_test_options(&cg, executor.clone())
    };
    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({"query": "SparseLexicalWidget", "limit": 5}),
        None,
        None,
        settled,
    )
    .await
    .expect("settled search renders");
    let text = response_text(&result);
    assert!(
        text.starts_with("freshness: fresh\n## Search Results"),
        "a settled generation opens with the fresh verdict: {text}"
    );
    assert!(!text.contains("indexing:"));

    let rebuilding = crate::mcp::tools::handlers::ToolCallRegistryOptions {
        code_index_freshness_reader: Some(freshness_reader(
            Some("generation.mcp-verified-graph-fixture.2"),
            "refreshing",
            true,
        )),
        ..search_test_options(&cg, executor.clone())
    };
    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({"query": "SparseLexicalWidget", "limit": 5, "format": "json"}),
        None,
        None,
        rebuilding,
    )
    .await
    .expect("rebuilding search renders");
    let payload: Value = serde_json::from_str(&response_text(&result)).expect("search JSON");
    assert_eq!(payload["freshness"]["state"], "possibly_stale");
    assert_eq!(
        payload["freshness"]["indexing"]["summary"],
        "state=refreshing rebuild_in_flight=true served_generation=generation.mcp-verified-graph-fixture.1 latest_generation=generation.mcp-verified-graph-fixture.2"
    );
    assert_eq!(
        payload["freshness"]["indexing"]["latest_generation"],
        "generation.mcp-verified-graph-fixture.2"
    );
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));

    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({"query": "SparseLexicalWidget", "limit": 5}),
        None,
        None,
        crate::mcp::tools::handlers::ToolCallRegistryOptions {
            code_index_freshness_reader: Some(freshness_reader(
                Some("generation.mcp-verified-graph-fixture.1"),
                "stale",
                false,
            )),
            ..search_test_options(&cg, executor)
        },
    )
    .await
    .expect("stalled search renders");
    let text = response_text(&result);
    assert!(
        text.starts_with(
            "freshness: possibly_stale\nindexing: state=stale rebuild_in_flight=false served_generation=generation.mcp-verified-graph-fixture.1 latest_generation=generation.mcp-verified-graph-fixture.1\n## Search Results"
        ),
        "a stale seat opens with the verdict and one indexing line: {text}"
    );
    cg.close();
}

#[test]
fn search_forwards_lexical_routing_and_renders_route_evidence() {
    run_with_locked_user_data_dir(
        search_forwards_lexical_routing_and_renders_route_evidence_case(),
    );
}

async fn search_forwards_lexical_routing_and_renders_route_evidence_case() {
    let dir = tempfile::TempDir::new().expect("lexical routing isolation");
    let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
    let project = dir.path().join("lexical-routing-search");
    std::fs::create_dir_all(project.join("src")).expect("create routing sources");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn SparseLexicalWidget() {}\n",
    )
    .expect("write routing fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.lexical-routing-search",
    )
    .await
    .expect("registered routing fixture");

    let observed_routing = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = std::sync::Arc::clone(&observed_routing);
    let executor: tracedecay_query::code_search::CodeIndexSearchExecutor =
        std::sync::Arc::new(move |request| {
            *sink.lock().expect("routing sink") = Some(request.lexical_routing.clone());
            Box::pin(async { completed_sparse_search_with_anchor_route("SparseLexicalWidget") })
        });
    let options = crate::mcp::tools::handlers::ToolCallRegistryOptions {
        code_index_freshness_reader: Some(freshness_reader(
            Some("generation.mcp-verified-graph-fixture.1"),
            "fresh",
            false,
        )),
        ..search_test_options(&cg, executor.clone())
    };
    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "sparse widget",
            "lexical_anchors": ["SparseLexicalWidget"],
            "prefer_symbol": true,
            "limit": 5,
        }),
        None,
        None,
        options,
    )
    .await
    .expect("routed search renders");
    let routing = observed_routing
        .lock()
        .expect("routing sink")
        .clone()
        .expect("the executor received the request");
    assert_eq!(routing.anchors[0].as_str(), "SparseLexicalWidget");
    assert!(routing.prefer_symbol);
    let text = response_text(&result);
    assert!(
        text.contains("**SparseLexicalWidget** (function, approximate) — rank 1 · utility 1 · via anchor:SparseLexicalWidget"),
        "each result names the routes that ranked it: {text}"
    );
    assert!(
        text.contains("Ranked routes fused into this page: query, anchor:SparseLexicalWidget"),
        "{text}"
    );

    let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "sparse widget",
            "lexical_anchors": ["SparseLexicalWidget"],
            "format": "json",
        }),
        None,
        None,
        search_test_options(&cg, executor.clone()),
    )
    .await
    .expect("routed JSON search renders");
    let payload: Value = serde_json::from_str(&response_text(&result)).expect("search JSON");
    assert_eq!(
        payload["lexical_routes"][1],
        json!({"route": "anchor", "anchor": "SparseLexicalWidget", "label": "anchor:SparseLexicalWidget"})
    );
    assert_eq!(
        payload["results"][0]["lexical_routes"],
        json!([{
            "route": "anchor:SparseLexicalWidget",
            "score_micros": 900_000,
            "matched_terms": ["SparseLexicalWidget"],
        }])
    );

    let too_many: Vec<String> = (0..=tracedecay_query::retrieval::lexical::MAX_LEXICAL_ANCHORS_V1)
        .map(|index| format!("anchor_{index}"))
        .collect();
    let error = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({"query": "sparse widget", "lexical_anchors": too_many}),
        None,
        None,
        search_test_options(&cg, executor.clone()),
    )
    .await
    .expect_err("anchor bounds are enforced before any lane runs");
    assert!(error.to_string().contains("at most 8 anchors"), "{error}");
    let error = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({"query": "sparse widget", "lexical_anchors": [""]}),
        None,
        None,
        search_test_options(&cg, executor),
    )
    .await
    .expect_err("empty anchors are rejected");
    assert!(error.to_string().contains("anchor 0 is empty"), "{error}");
    cg.close();
}
