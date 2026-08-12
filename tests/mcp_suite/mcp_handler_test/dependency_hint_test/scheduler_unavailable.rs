use super::*;

#[tokio::test]
async fn search_explicit_lazy_opt_in_fails_closed_without_scheduler_authority() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(
            project,
            "export default interface SchedulerOnlyDependency { value: string }\n",
        );
        fs::write(
            project.join("src/app.ts"),
            "export function GenerationAnchor() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/dependency-types.ts"),
            "import type SchedulerOnlyDependency from \"pkg\";\n",
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let zero = search_payload(&server, json!({"query": "default", "limit": 5})).await;
    assert_eq!(
        zero["results"].as_array().map(Vec::len),
        Some(0),
        "ordinary exact and lexical search must genuinely miss before opt-in: {zero}"
    );
    assert_eq!(zero["code_generation"], before["code_generation"]);

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_search",
        json!({
            "query": "default",
            "limit": 5,
            "lazy_index_ignored_dependencies": true,
        }),
    )
    .await;
    assert!(
        response["result"].is_null(),
        "an unavailable scheduler must not fabricate a same-call result: {response}"
    );
    assert_eq!(
        response["error"]["data"]["reason_code"].as_str(),
        Some("ignored_dependency_index_scheduler_unavailable"),
        "explicit opt-in must fail closed at the typed scheduler boundary: {response}"
    );
    assert_eq!(response["error"]["data"]["retryable"], true);

    let after = search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_eq!(
        after["code_generation"], before["code_generation"],
        "the fail-closed P0 bridge must not mutate the serving generation"
    );
    fixture.harness.shutdown().await;
}
