use super::{call_production_tool, graph_query_fixture_with_sources, shutdown_graph_fixture};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use tracedecay_contracts::retrieval::SymbolGraphScope;
use tracedecay_contracts::{
    CallableCodeSurfaceMeta, CodeCallersSurfaceRequest, ResultProjection, RetrievalOrder,
};

#[tokio::test]
async fn callers_report_unsupported_temporary_receivers_without_inventing_edges() {
    let fixture = graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/walk.rs"),
            "pub struct WalkBuilder;\nimpl WalkBuilder {\n\
             pub fn build(&self) {}\n\
             pub fn finish(&self) {}\n\
             pub fn spaced(&self) {}\n\
             pub fn commented(&self) {}\n\
             pub fn receiver_only(&self) {}\n\
             pub fn bound(&self) {}\n\
             pub fn read(&self) {}\n\
             pub fn shadowed(&self) {}\n\
             pub fn repeat(&self) -> WalkBuilder { WalkBuilder }\n\
             pub fn unused(&self) {}\n}\n",
        )
        .unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "mod walk;\nuse walk::WalkBuilder;\n\
             pub struct Args;\nimpl Args {\n\
             pub fn walk_builder(&self) -> Result<WalkBuilder, ()> { Ok(WalkBuilder) }\n}\n\
             pub fn first(args: &Args) -> Result<(), ()> { args.walk_builder()?.build(); Ok(()) }\n\
             pub fn second(args: &Args) -> Result<(), ()> { args.walk_builder()?.build(); Ok(()) }\n\
             pub fn third(args: &Args) -> Result<(), ()> { args.walk_builder()?.build(); Ok(()) }\n\
             pub fn unbound(args: &Args) -> Result<(), ()> { args.walk_builder()?.finish(); Ok(()) }\n\
             pub fn spaced(args: &Args) -> Result<(), ()> { args.walk_builder()?.\n spaced(); Ok(()) }\n\
             pub fn commented(args: &Args) -> Result<(), ()> { args.walk_builder()?. /* comment */ commented(); Ok(()) }\n\
             pub fn collision(receiver_only: &Args) -> Result<(), ()> { receiver_only.walk_builder()?. /* decoy.unused */ spaced(); Ok(()) }\n\
             pub fn nested(builder: &WalkBuilder) { builder.repeat().repeat(); }\n\
             pub fn shadowed() {}\n\
             pub fn unproven(args: &Args) -> Result<(), ()> { args.walk_builder()?.shadowed(); Ok(()) }\n\
             pub fn known(builder: &WalkBuilder) { builder.build(); builder.bound(); builder.read(); }\n",
        )
        .unwrap();
    })
    .await;

    for (method, expected_completeness, expected_names) in [
        ("unused", "complete", Vec::new()),
        ("receiver_only", "complete", Vec::new()),
        ("spaced", "partial", Vec::new()),
        ("commented", "partial", Vec::new()),
        ("read", "complete", vec!["known"]),
        ("bound", "complete", vec!["known"]),
        ("repeat", "partial", vec!["nested"]),
        ("shadowed", "partial", Vec::new()),
        ("finish", "partial", Vec::new()),
        ("build", "partial", vec!["known"]),
    ] {
        let target = call_production_tool(
            &fixture,
            "tracedecay_by_qualified_name",
            json!({"qualified_name": format!("src/walk.rs::WalkBuilder::{method}"), "format": "json"}),
            None,
            None,
        ).await.expect("exact builder method");
        let target: Value = serde_json::from_str(extract_text(&target.value)).unwrap();
        assert_eq!(target.as_array().unwrap().len(), 1, "{target:#}");
        let node = target[0]["node_id"].as_str().unwrap().to_owned();
        let mut arguments = serde_json::to_value(CodeCallersSurfaceRequest {
            node_id: node,
            maximum_depth: 1,
            scope: SymbolGraphScope::default(),
            meta: CallableCodeSurfaceMeta {
                projection: ResultProjection::Evidence,
                order: RetrievalOrder::SourcePosition,
                cursor: None,
            },
        })
        .unwrap();
        arguments["format"] = json!("json");
        let result = call_production_tool(&fixture, "tracedecay_callers", arguments, None, None)
            .await
            .expect("public callers");
        let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        let evidence = &payload["outcome"]["value"];
        let names = evidence["payload"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("caller evidence missing: {payload:#}"))
            .iter()
            .map(|item| item["symbol"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names, expected_names,
            "unsupported sites must not become edges"
        );
        assert_eq!(
            evidence["coverage"]["completeness"], expected_completeness,
            "caller coverage must account for unsupported receiver sites: {payload:#}"
        );
        let omissions = evidence["omissions"].as_array().unwrap();
        if expected_completeness == "partial" {
            // Three build sites still describe one unsupported capability,
            // not a claim about the number of missing caller symbols.
            assert_eq!(
                omissions
                    .iter()
                    .filter(|omission| omission["reason"] == "unsupported")
                    .map(|omission| omission["count"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                vec![1],
                "unsupported resolution capability must be disclosed once: {payload:#}"
            );
        } else {
            assert!(
                omissions.is_empty(),
                "unrelated methods remain complete: {payload:#}"
            );
        }
    }
    shutdown_graph_fixture(fixture).await;
}
