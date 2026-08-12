use super::*;

const GENERATION_ADVANCED_REASON: &str =
    "application.symbol-graph.ignored-dependency-generation-advanced";

enum DependencyImportStyle {
    Named,
    Default,
}

async fn lazy_dependency_fixture(
    symbol: &str,
    import_style: DependencyImportStyle,
) -> ProductionCompositionFixture {
    let (declaration, import) = match import_style {
        DependencyImportStyle::Named => (
            format!("export interface {symbol} {{ value: string }}\n"),
            format!("import type {{ {symbol} }} from \"pkg\";\n"),
        ),
        DependencyImportStyle::Default => (
            format!("export default interface {symbol} {{ value: string }}\n"),
            format!("import type {symbol} from \"pkg\";\n"),
        ),
    };
    production_composition_fixture_with_sources(move |project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(project, &declaration);
        fs::write(
            project.join("src/app.ts"),
            "export function GenerationAnchor() { return 1; }\n",
        )
        .unwrap();
        fs::write(project.join("src/dependency-types.ts"), import).unwrap();
    })
    .await
}

fn assert_generation_advanced_retry(response: &Value) {
    assert!(
        response["result"].is_null(),
        "the generation-advancing call must not return a same-call symbol payload: {response}"
    );
    assert_eq!(
        response["error"]["data"]["reason_code"].as_str(),
        Some(GENERATION_ADVANCED_REASON),
        "lazy admission must expose the canonical usecase retry reason: {response}"
    );
    assert_eq!(response["error"]["data"]["retryable"], true);
}

fn code_generation(payload: &Value) -> &str {
    payload["code_generation"]
        .as_str()
        .expect("search response code generation")
}

async fn exact_symbol_payload(server: &McpServer, arguments: Value) -> Value {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_find_exact_symbol", arguments).await;
    assert!(
        response["error"].is_null(),
        "exact-symbol read must succeed: {response}"
    );
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("exact-symbol response JSON")
}

#[tokio::test]
async fn exact_symbol_explicit_lazy_admission_advances_generation_then_retry_finds_symbol_once() {
    let fixture =
        lazy_dependency_fixture("ExactOnlyDependency", DependencyImportStyle::Named).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let zero =
        exact_symbol_payload(&server, json!({"name": "ExactOnlyDependency", "limit": 5})).await;
    assert_eq!(
        zero["count"], 0,
        "the ignored dependency must be absent before explicit admission: {zero}"
    );
    let after_zero =
        search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_eq!(code_generation(&after_zero), code_generation(&before));

    let arguments = json!({
        "name": "ExactOnlyDependency",
        "limit": 5,
        "lazy_index_ignored_dependencies": true
    });
    let first = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_find_exact_symbol",
        arguments.clone(),
    )
    .await;
    assert_generation_advanced_retry(&first);

    let after = search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_ne!(
        code_generation(&after),
        code_generation(&before),
        "the scheduler must publish a new serving generation before requesting a retry"
    );

    let payload = exact_symbol_payload(&server, arguments).await;
    assert_eq!(payload["count"], 1, "retry must find one symbol: {payload}");
    let matches = payload["matches"]
        .as_array()
        .expect("exact-symbol retry matches");
    assert_eq!(matches.len(), 1, "retry must find one symbol: {payload}");
    assert_eq!(
        matches[0]["name"], "ExactOnlyDependency",
        "retry must return the requested dependency symbol: {payload}"
    );
    assert_eq!(
        matches[0]["file"], "node_modules/pkg/index.d.ts",
        "retry must return the ignored dependency declaration: {payload}"
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn search_explicit_lazy_admission_advances_generation_then_retry_finds_dependency_chunk_once()
{
    let fixture =
        lazy_dependency_fixture("SearchOnlyDependency", DependencyImportStyle::Default).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let zero = search_payload(&server, json!({"query": "default", "limit": 5})).await;
    assert_eq!(
        zero["results"],
        json!([]),
        "the ignored dependency must be absent before explicit admission: {zero}"
    );
    let after_zero =
        search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_eq!(code_generation(&after_zero), code_generation(&before));

    let arguments = json!({
        "query": "default",
        "limit": 5,
        "lazy_index_ignored_dependencies": true
    });
    let first =
        handle_real_server_tool_call_raw(&server, "tracedecay_search", arguments.clone()).await;
    assert_generation_advanced_retry(&first);

    let retry = search_payload(&server, arguments.clone()).await;
    assert_ne!(
        code_generation(&retry),
        code_generation(&before),
        "the retry must bind the scheduler-published generation"
    );
    let results = retry["results"].as_array().expect("search retry results");
    assert_eq!(
        results.len(),
        1,
        "retry must find one dependency chunk: {retry}"
    );
    let result = &results[0];
    let anchor = result["candidate"]["anchor_id"]
        .as_str()
        .expect("search retry candidate anchor");
    assert!(
        anchor.starts_with("code-chunk:"),
        "the default keyword must bind the admitted dependency chunk: {retry}"
    );
    let chunk_id = anchor
        .strip_prefix("code-chunk:")
        .expect("search retry chunk anchor");
    let expected_source = format!("code-chunk:{}:{chunk_id}", code_generation(&retry));
    assert!(
        result["candidate"]["occurrences"]
            .as_array()
            .is_some_and(|occurrences| occurrences.iter().any(|occurrence| {
                occurrence["source_occurrence_id"].as_str() == Some(expected_source.as_str())
                    && occurrence["file_occurrence_id"]
                        .as_str()
                        .is_some_and(|file| !file.is_empty())
            })),
        "the returned chunk must retain its exact generation, chunk, and file occurrence: {retry}"
    );
    let stable = search_payload(&server, arguments).await;
    assert_eq!(
        code_generation(&stable),
        code_generation(&retry),
        "a positive retry must not schedule another generation: {stable}"
    );
    assert_eq!(
        stable["results"].as_array().map(Vec::len),
        Some(1),
        "a positive retry must continue returning exactly one result: {stable}"
    );
    fixture.harness.shutdown().await;
}
