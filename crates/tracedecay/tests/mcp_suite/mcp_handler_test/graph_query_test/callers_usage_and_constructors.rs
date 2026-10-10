//! Callers must return import/type/`new` sites, and must not claim
//! `complete` with an empty page when the graph already held those rows.
//!
//! Issue #3301: rspack `Compiler` (type-only imports) and
//! webpack-external-import `new URLImportPlugin(` both answered
//! `items=[]` / `completeness=complete` while disk and adjacency disagreed.

use super::{call_production_tool, graph_query_fixture_with_sources, shutdown_graph_fixture};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use tracedecay_contracts::retrieval::SymbolGraphScope;
use tracedecay_contracts::{
    CallableCodeSurfaceMeta, CodeCallersSurfaceRequest, ResultProjection, RetrievalOrder,
};

async fn callers_of(fixture: &super::GraphQueryFixture, qualified_name: &str) -> Value {
    let target = call_production_tool(
        fixture,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": qualified_name, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("exact symbol lookup");
    let target: Value = serde_json::from_str(extract_text(&target.value)).unwrap();
    assert_eq!(
        target.as_array().unwrap().len(),
        1,
        "{qualified_name}: {target:#}"
    );
    let mut arguments = serde_json::to_value(CodeCallersSurfaceRequest {
        node_id: target[0]["node_id"].as_str().unwrap().to_owned(),
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
    let result = call_production_tool(fixture, "tracedecay_callers", arguments, None, None)
        .await
        .expect("callers");
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    payload["outcome"]["value"].clone()
}

fn caller_files(evidence: &Value) -> Vec<String> {
    let mut files = evidence["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("caller page has no items: {evidence}"))
        .iter()
        .map(|item| item["symbol"]["file"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

fn caller_names(evidence: &Value) -> Vec<String> {
    let mut names = evidence["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("caller page has no items: {evidence}"))
        .iter()
        .map(|item| item["symbol"]["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// A TypeScript class imported only as a type still has callers: the methods
/// that name it. An empty complete page here is the rspack `Compiler` bug.
#[tokio::test]
async fn callers_return_type_import_usage_sites_instead_of_empty_complete() {
    let fixture = graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/Compiler.ts"),
            "export class Compiler {\n  run() {}\n}\n",
        )
        .unwrap();
        fs::write(
            project.join("src/JsonpTemplatePlugin.ts"),
            "import type { Compiler } from './Compiler';\n\
             export default class JsonpTemplatePlugin {\n\
               apply(compiler: Compiler) {\n\
                 compiler.run();\n\
               }\n\
             }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/WebWorkerTemplatePlugin.ts"),
            "import type { Compiler } from './Compiler';\n\
             export default class WebWorkerTemplatePlugin {\n\
               apply(compiler: Compiler) {\n\
                 compiler.run();\n\
               }\n\
             }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/ContainerPlugin.ts"),
            "import type { Compiler } from './Compiler';\n\
             export default class ContainerPlugin {\n\
               apply(compiler: Compiler) {\n\
                 compiler.run();\n\
               }\n\
             }\n",
        )
        .unwrap();
    })
    .await;

    let evidence = callers_of(&fixture, "src/Compiler.ts::Compiler").await;
    let files = caller_files(&evidence);
    assert!(
        files.contains(&"src/JsonpTemplatePlugin.ts".to_owned())
            && files.contains(&"src/WebWorkerTemplatePlugin.ts".to_owned())
            && files.contains(&"src/ContainerPlugin.ts".to_owned()),
        "type-imported Compiler must list the three apply sites: {evidence:#}"
    );
    assert_eq!(
        evidence["coverage"]["completeness"], "complete",
        "returned usage sites are a complete answer, not an empty one: {evidence:#}"
    );
    assert_ne!(
        evidence["payload"]["items"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        0,
        "adjacency that hydrates to usage sites must not become items=[]: {evidence:#}"
    );
    assert_ne!(
        evidence["coverage"]["eligible"],
        json!(0),
        "eligible must not be 0 when callers were returned: {evidence:#}"
    );

    shutdown_graph_fixture(fixture).await;
}

/// `const Plugin = require("./plugin"); new Plugin()` is a real constructor
/// call site. Callers of the exported class must name that factory.
#[tokio::test]
async fn callers_return_commonjs_constructor_sites() {
    let fixture = graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src/webpack")).unwrap();
        fs::create_dir_all(project.join("manual/webpack")).unwrap();
        fs::write(
            project.join("src/webpack/index.js"),
            "class URLImportPlugin {\n  constructor(opts) {\n    this.opts = opts;\n  }\n}\n\
             module.exports = URLImportPlugin;\n",
        )
        .unwrap();
        fs::write(
            project.join("manual/webpack/webpackConfigFactory.js"),
            "const URLImportPlugin = require(\"../../src/webpack\");\n\
             function build(siteId) {\n\
               return new URLImportPlugin({ manifestName: `website-${siteId}` });\n\
             }\n\
             module.exports = build;\n",
        )
        .unwrap();
    })
    .await;

    let evidence = callers_of(&fixture, "src/webpack/index.js::URLImportPlugin").await;
    assert!(
        caller_names(&evidence).iter().any(|name| name == "build"),
        "new URLImportPlugin in the factory must appear: {evidence:#}"
    );
    assert!(
        caller_files(&evidence)
            .iter()
            .any(|file| file == "manual/webpack/webpackConfigFactory.js"),
        "the constructor file must be a caller: {evidence:#}"
    );
    assert_eq!(
        evidence["coverage"]["completeness"], "complete",
        "{evidence:#}"
    );

    shutdown_graph_fixture(fixture).await;
}
