use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    AuthorizationRevision, ExactClass, FusedCandidate, FusionProfileId, LogicalEvidenceId,
    PrincipalId, PublicRetrieverStatus, QueryFallbackSubpayload, RankedCandidate,
    RetrievalAnchorId, RetrieverKind,
};

use super::dispatch_test_support::{SelectorEnv, verified_graph_options};
use super::*;
use crate::config::lock_user_data_dir_test_env;

struct PendingVerifiedGraphQueryPort;

impl tracedecay_graph_query::VerifiedGraphQueryPort for PendingVerifiedGraphQueryPort {
    fn open<'a>(
        &'a self,
        _request: tracedecay_graph_query::VerifiedGraphQueryRequest<'a>,
    ) -> tracedecay_graph_query::VerifiedGraphQueryFuture<'a> {
        Box::pin(std::future::pending())
    }
}

fn lexical_candidate() -> RankedCandidate {
    RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: RetrievalAnchorId::new("code-symbol:lexical-widget")
                .expect("lexical candidate anchor"),
            logical_evidence_id: LogicalEvidenceId::new("logical.lexical-widget")
                .expect("lexical candidate logical evidence"),
            occurrences: Vec::new(),
            exact_class: ExactClass::Approximate,
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: Vec::new(),
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    }
}

fn completed_lexical_search() -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
    let candidate = lexical_candidate();
    let fallback_coverage = RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .map(|lane| (lane, PublicRetrieverStatus::Complete))
        .collect::<BTreeMap<_, _>>();
    let query_fallback = QueryFallbackSubpayload::new(
        FusionProfileId::new("profile.search-graph-independence").expect("search profile identity"),
        vec![candidate.clone()],
        fallback_coverage,
        Vec::new(),
        None,
    )
    .expect("canonical lexical fallback payload");
    let anchor = candidate.candidate.anchor_id.clone();
    let semantic = crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
        reason: "semantic_generation_warming",
    };
    crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(
        crate::mcp::server::CodeIndexSearchCompletedV1 {
            code_generation: "generation.search-degradation.1".to_owned(),
            ordered_candidates: vec![candidate],
            query_fallback: Arc::new(query_fallback),
            display_by_anchor: HashMap::from([(
                anchor,
                crate::mcp::server::CodeIndexSearchDisplayV1 {
                    name: "LexicalWidget".to_owned(),
                    qualified_name: "crate::LexicalWidget".to_owned(),
                    kind: "function".to_owned(),
                    path: "src/lib.rs".to_owned(),
                },
            )]),
            coverage: crate::mcp::server::CodeIndexSearchCoverageV1::fused(&semantic),
            semantic,
            next_cursor: None,
            lexical_routes: tracedecay_query::retrieval::lexical::LexicalRouteReceiptV1 {
                routes: vec![tracedecay_query::retrieval::lexical::LexicalRouteKindV1::Query],
                matches_by_anchor: BTreeMap::new(),
            },
        },
    )
}

fn lexical_search_options(cg: &TraceDecay) -> ToolCallRegistryOptions<'_> {
    let executor: crate::mcp::server::CodeIndexSearchExecutor =
        Arc::new(|_| Box::pin(async { completed_lexical_search() }));
    verified_graph_options(
        cg,
        ToolCallRegistryOptions {
            code_index_search_executor: Some(executor),
            code_index_search_authority: Some(crate::mcp::server::CodeIndexSearchAuthorityV1 {
                principal: PrincipalId::new("principal.search-graph-independence")
                    .expect("search principal"),
                authorization_revision: AuthorizationRevision::new(
                    "authorization.search-graph-independence",
                )
                .expect("search authorization revision"),
            }),
            ..ToolCallRegistryOptions::default()
        },
    )
}

#[tokio::test]
async fn tracedecay_search_preserves_lexical_results_when_graph_admission_is_missing() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("search degradation isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("search-graph-independence");
    fs::create_dir_all(project.join("src")).expect("create fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn LexicalWidget() {}\n")
        .expect("write lexical fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.search-graph-independence",
    )
    .await
    .expect("registered search fixture");

    let mut options = lexical_search_options(&cg);
    options.verified_graph_query_port = None;

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "LexicalWidget",
            "limit": 5,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("missing graph admission must not erase lexical search");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("search JSON text"),
    )
    .expect("search JSON payload");

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["results"][0]["display"]["name"], "LexicalWidget");
    assert_eq!(payload["results"][0]["display"]["path"], "src/lib.rs");
    assert!(payload["results"][0]["node_id"].is_null());
    assert_eq!(
        payload["code_generation"],
        "generation.search-degradation.1"
    );
    assert_eq!(payload["coverage"]["exact"], "complete");
    assert_eq!(payload["coverage"]["lexical"], "complete");
    assert_eq!(
        payload["verified_graph_evidence"],
        json!({
            "status": "unavailable",
            "reason_code": "verified-code-graph-read-unavailable",
            "retryable": false,
            "detail": "the exact project verified graph query is not mounted",
        })
    );
    assert_eq!(
        payload["external_import_hint"],
        payload["verified_graph_evidence"]
    );
    cg.close();
}

#[tokio::test]
async fn tracedecay_search_refuses_foreign_generation_graph_evidence_without_erasing_results() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("graph generation mismatch isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("search-graph-generation-mismatch");
    fs::create_dir_all(project.join("src")).expect("create mismatch fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn LexicalWidget() {}\n")
        .expect("write mismatch fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.search-graph-generation-mismatch",
    )
    .await
    .expect("registered mismatch fixture");

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_search",
        json!({
            "query": "LexicalWidget",
            "limit": 5,
            "format": "json",
        }),
        None,
        None,
        lexical_search_options(&cg),
    )
    .await
    .expect("foreign graph generation must remain optional for lexical search");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("mismatch search JSON text"),
    )
    .expect("mismatch search JSON payload");

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["results"][0]["display"]["name"], "LexicalWidget");
    assert!(payload["results"][0]["node_id"].is_null());
    assert_eq!(
        payload["verified_graph_evidence"]["reason_code"],
        "verified-code-graph-generation-mismatch"
    );
    assert_eq!(payload["verified_graph_evidence"]["retryable"], true);
    assert_eq!(
        payload["external_import_hint"],
        payload["verified_graph_evidence"]
    );
    cg.close();
}

#[tokio::test]
async fn tracedecay_search_does_not_wait_for_slow_graph_admission() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("slow graph isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("slow-search-graph-admission");
    fs::create_dir_all(project.join("src")).expect("create slow graph fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn LexicalWidget() {}\n")
        .expect("write slow graph fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.slow-search-graph-admission",
    )
    .await
    .expect("registered slow graph fixture");

    let mut options = lexical_search_options(&cg);
    options.verified_graph_query_port = Some(Arc::new(PendingVerifiedGraphQueryPort));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_search",
            json!({
                "query": "LexicalWidget",
                "limit": 1,
                "format": "json",
            }),
            None,
            None,
            options,
        ),
    )
    .await
    .expect("primary search must not wait for pending graph admission")
    .expect("pending graph admission must not erase lexical search");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("slow graph search JSON text"),
    )
    .expect("slow graph search JSON payload");

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["results"][0]["display"]["name"], "LexicalWidget");
    assert_eq!(
        payload["code_generation"],
        "generation.search-degradation.1"
    );
    assert_eq!(payload["coverage"]["exact"], "complete");
    assert_eq!(payload["coverage"]["lexical"], "complete");
    assert_eq!(
        payload["verified_graph_evidence"],
        json!({
            "status": "unavailable",
            "reason_code": "verified-code-graph-read-unavailable",
            "retryable": true,
            "detail": "verified graph evidence did not become available before primary search completed",
        })
    );
    assert!(payload["external_import_hint"].is_null());

    let mut markdown_options = lexical_search_options(&cg);
    markdown_options.verified_graph_query_port = Some(Arc::new(PendingVerifiedGraphQueryPort));
    let markdown = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_search",
            json!({
                "query": "LexicalWidget",
                "limit": 1,
                "format": "markdown",
            }),
            None,
            None,
            markdown_options,
        ),
    )
    .await
    .expect("markdown search must not wait for pending graph admission")
    .expect("markdown search must preserve lexical results");
    let text = markdown.value["content"][0]["text"]
        .as_str()
        .expect("slow graph search markdown");
    assert!(text.contains("LexicalWidget"));
    assert!(text.contains("Verified Graph Evidence"));
    assert!(text.contains("Graph enrichment unavailable"));
    cg.close();
}

#[tokio::test]
async fn tracedecay_context_preserves_fallback_results_while_semantic_and_graph_warm() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("context degradation isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("context-graph-independence");
    fs::create_dir_all(project.join("src")).expect("create fixture sources");
    fs::write(project.join("src/lib.rs"), "pub fn LexicalWidget() {}\n")
        .expect("write lexical fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.context-graph-independence",
    )
    .await
    .expect("registered context fixture");

    let mut options = lexical_search_options(&cg);
    options.verified_graph_query_port = None;
    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_context",
        json!({
            "task": "explain LexicalWidget",
            "include_memory": false,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("warming graph and semantic lanes must not erase fallback context");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("context JSON text"),
    )
    .expect("context JSON payload");

    assert_eq!(
        payload["code_generation"],
        "generation.search-degradation.1"
    );
    assert_eq!(payload["search_matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["search_matches"][0]["name"], "LexicalWidget");
    assert_eq!(payload["search_matches"][0]["file"], "src/lib.rs");
    assert_eq!(payload["symbols"].as_array().map(Vec::len), Some(0));
    assert_eq!(payload["coverage"]["exact"], "complete");
    assert_eq!(payload["coverage"]["lexical"], "complete");
    assert_eq!(
        payload["coverage"]["semantic"],
        json!({
            "status": "unavailable",
            "reason": "semantic_generation_warming",
        })
    );
    assert_eq!(
        payload["verified_graph_evidence"]["reason_code"],
        "verified-code-graph-read-unavailable"
    );
    assert_eq!(payload["memory_matches"].as_array().map(Vec::len), Some(0));

    let mut markdown_options = lexical_search_options(&cg);
    markdown_options.verified_graph_query_port = None;
    let markdown = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_context",
        json!({
            "task": "explain LexicalWidget",
            "include_memory": false,
            "format": "markdown",
        }),
        None,
        None,
        markdown_options,
    )
    .await
    .expect("warming context markdown must preserve fallback results");
    let text = markdown.value["content"][0]["text"]
        .as_str()
        .expect("context markdown text");
    assert!(text.contains("LexicalWidget"));
    assert!(text.contains("Semantic results pending"));
    assert!(text.contains("Graph enrichment unavailable"));
    cg.close();
}

#[tokio::test]
async fn tracedecay_context_returns_typed_pending_coverage_when_every_code_lane_warms() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("cold context isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("cold-context");
    fs::create_dir_all(&project).expect("create cold context fixture");
    let (cg, _runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, "project.cold-context")
            .await
            .expect("registered cold context fixture");

    let executor: crate::mcp::server::CodeIndexSearchExecutor = Arc::new(|_| {
        Box::pin(async {
            crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                crate::mcp::server::CodeIndexSearchUnavailableV1 {
                    code_generation: None,
                    reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                    semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                        reason: tracedecay_query::code_search::lane_reason::GENERATION_REBUILDING,
                    },
                    coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                        tracedecay_query::code_search::lane_reason::GENERATION_REBUILDING,
                    ),
                },
            )
        })
    });
    let options = ToolCallRegistryOptions {
        code_index_search_executor: Some(executor),
        verified_graph_query_port: None,
        ..lexical_search_options(&cg)
    };
    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_context",
        json!({
            "task": "explain cold startup",
            "include_memory": false,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("cold code lanes must return typed partial context");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("cold context JSON text"),
    )
    .expect("cold context JSON payload");

    assert!(payload.get("code_generation").is_none());
    assert!(payload.get("search_matches").is_none());
    assert_eq!(payload["symbols"].as_array().map(Vec::len), Some(0));
    for lane in ["exact", "lexical", "graph", "semantic"] {
        assert_eq!(payload["coverage"][lane]["status"], "unavailable");
        assert_eq!(payload["coverage"][lane]["reason"], "generation_rebuilding");
    }
    assert_eq!(payload["memory_matches"].as_array().map(Vec::len), Some(0));
    cg.close();
}

fn stale_lexical_search() -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
    match completed_lexical_search() {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(mut complete) => {
            complete.coverage = crate::mcp::server::CodeIndexSearchCoverageV1::fused_stale(
                &complete.code_generation,
                &complete.semantic,
            );
            crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete)
        }
        other @ crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(_) => other,
    }
}

#[tokio::test]
async fn tracedecay_context_preserves_stale_lane_coverage_markers() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("stale context isolation");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("stale-context-coverage");
    fs::create_dir_all(project.join("src")).expect("create stale context sources");
    fs::write(project.join("src/lib.rs"), "pub fn LexicalWidget() {}\n")
        .expect("write stale context fixture");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.stale-context-coverage",
    )
    .await
    .expect("registered stale context fixture");

    let executor: crate::mcp::server::CodeIndexSearchExecutor =
        Arc::new(|_| Box::pin(async { stale_lexical_search() }));
    let mut options = lexical_search_options(&cg);
    options.code_index_search_executor = Some(executor);
    options.verified_graph_query_port = None;
    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_context",
        json!({
            "task": "explain LexicalWidget",
            "include_memory": false,
            "format": "json",
        }),
        None,
        None,
        options,
    )
    .await
    .expect("serve-stale context must still answer");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("stale context JSON text"),
    )
    .expect("stale context JSON payload");

    assert_eq!(
        payload["code_generation"],
        "generation.search-degradation.1"
    );
    assert_eq!(payload["search_matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["coverage"]["recall"], "partial");
    for lane in ["exact", "lexical", "graph"] {
        assert_eq!(
            payload["coverage"][lane]["status"], "stale",
            "context must keep serve-stale markers on {lane}: {payload}"
        );
        assert_eq!(
            payload["coverage"][lane]["generation"], "generation.search-degradation.1",
            "stale {lane} must name the served generation: {payload}"
        );
    }
    cg.close();
}
