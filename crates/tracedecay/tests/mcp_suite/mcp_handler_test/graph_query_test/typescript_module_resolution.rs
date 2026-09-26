//! `callers` and `file_dependents` over a TypeScript monorepo indexed through
//! the production capture path: workspace package names, tsconfig `paths`,
//! dotted extensionless file names, and barrel re-exports all bind, and an
//! import of project code the seal cannot reach is disclosed as partial
//! coverage instead of a complete empty answer.

use super::{call_production_tool, graph_query_fixture_with_sources, shutdown_graph_fixture};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tracedecay_contracts::retrieval::SymbolGraphScope;
use tracedecay_contracts::{
    CallableCodeSurfaceMeta, CodeCallersSurfaceRequest, ResultProjection, RetrievalOrder,
};

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tracedecay-code-extraction/fixtures/typescript-monorepo"
);

fn copy_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

async fn callers_evidence(
    fixture: &super::GraphQueryFixture,
    qualified_name: &str,
) -> (Vec<String>, Value) {
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
    let evidence = payload["outcome"]["value"].clone();
    let mut names = evidence["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("caller evidence missing: {payload:#}"))
        .iter()
        .map(|item| item["symbol"]["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    (names, evidence)
}

async fn file_dependents_evidence(fixture: &super::GraphQueryFixture, file: &str) -> Value {
    let result = call_production_tool(
        fixture,
        "tracedecay_file_dependents",
        json!({"file": file, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("file dependents");
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    payload["outcome"]["value"].clone()
}

fn unsupported_omissions(evidence: &Value) -> Vec<u64> {
    evidence["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|omission| omission["reason"] == "unsupported")
        .map(|omission| omission["count"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn typescript_monorepo_callers_and_file_dependents_bind_across_packages() {
    let fixture =
        graph_query_fixture_with_sources(|project| copy_fixture(Path::new(FIXTURE_ROOT), project))
            .await;

    for (qualified_name, expected_callers, expected_completeness) in [
        // Workspace package name, `exports["."]` mapped from `dist/` to `src/`,
        // `export * from` in the barrel; `format` is a ubiquitous name that
        // only an explicit import may bind.
        (
            "packages/shared/src/format.ts::format",
            vec!["main"],
            "complete",
        ),
        // `export { sum as add } from` plus the `./math` subpath export.
        ("packages/shared/src/math.ts::sum", vec!["main"], "complete"),
        // `./report.helpers` and `../src/report.helpers` name a dotted file.
        (
            "apps/web/src/report.helpers.ts::buildReport",
            vec!["joins rows", "main"],
            "complete",
        ),
        // `./lib` is `lib/index.ts`, which forwards `reexported` from `./x`.
        (
            "apps/web/src/lib/x.ts::reexported",
            vec!["main"],
            "complete",
        ),
        // tsconfig `paths` from the nearest config with `extends`.
        (
            "apps/web/src/widgets/widget.ts::widget",
            vec!["main"],
            "complete",
        ),
        // `gaps.ts` imports these names from project modules the seal cannot
        // reach: no invented edge, and the empty answer is disclosed.
        ("apps/web/src/decoys.ts::missing", Vec::new(), "partial"),
        ("apps/web/src/decoys.ts::gone", Vec::new(), "partial"),
        // An external dependency (`react`, also as `React.useState`) is not a
        // coverage gap.
        ("apps/web/src/decoys.ts::useState", Vec::new(), "complete"),
        // Default imports: `export default function` (also re-defaulted by
        // `relay.ts`), `export default <name>`, `export { impl as default }`,
        // and `export { default as welcome } from` reached by name and as a
        // namespace member.
        (
            "apps/web/src/defaults/greet.ts::greet",
            vec!["consumeDefaults"],
            "complete",
        ),
        (
            "apps/web/src/defaults/farewell.ts::farewell",
            vec!["consumeDefaults"],
            "complete",
        ),
        (
            "apps/web/src/defaults/aliased.ts::aliasedImpl",
            vec!["consumeDefaults"],
            "complete",
        ),
        (
            "apps/web/src/defaults/welcome.ts::welcome",
            vec!["consumeDefaults"],
            "complete",
        ),
        // Namespace member calls: `import * as`, a nested `export * as`
        // behind the workspace package, and a named import of it.
        (
            "apps/web/src/tools.ts::sharpen",
            vec!["consumeNamespaces"],
            "complete",
        ),
        (
            "packages/shared/src/strings.ts::upper",
            vec!["consumeNamespaces"],
            "complete",
        ),
        (
            "packages/shared/src/strings.ts::lower",
            vec!["consumeNamespaces"],
            "complete",
        ),
        // `export *` never forwards `default`; a namespace without the member
        // binds nothing. Both are disclosed.
        (
            "packages/shared/src/defaulted.ts::defaulted",
            Vec::new(),
            "partial",
        ),
        (
            "apps/web/src/decoys.ts::absentMember",
            Vec::new(),
            "partial",
        ),
    ] {
        let (names, evidence) = callers_evidence(&fixture, qualified_name).await;
        assert_eq!(names, expected_callers, "{qualified_name}: {evidence:#}");
        assert_eq!(
            evidence["coverage"]["completeness"], expected_completeness,
            "{qualified_name}: {evidence:#}"
        );
        let expected_omissions: Vec<u64> = if expected_completeness == "partial" {
            vec![1]
        } else {
            Vec::new()
        };
        assert_eq!(
            unsupported_omissions(&evidence),
            expected_omissions,
            "{qualified_name}"
        );
    }

    let evidence = file_dependents_evidence(&fixture, "apps/web/src/report.helpers.ts").await;
    assert_eq!(
        evidence["payload"]["dependent_files"],
        json!([
            "apps/web/src/main.ts",
            "apps/web/test/report.helpers.test.ts"
        ]),
        "{evidence:#}"
    );
    assert_eq!(
        evidence["coverage"]["completeness"], "complete",
        "{evidence:#}"
    );

    let evidence = file_dependents_evidence(&fixture, "packages/shared/src/format.ts").await;
    assert_eq!(
        evidence["payload"]["dependent_files"],
        json!(["apps/web/src/main.ts"]),
        "{evidence:#}"
    );

    for file in [
        "apps/web/src/defaults/welcome.ts",
        "packages/shared/src/strings.ts",
    ] {
        let evidence = file_dependents_evidence(&fixture, file).await;
        assert_eq!(
            evidence["payload"]["dependent_files"],
            json!(["apps/web/src/consumers.ts"]),
            "{file}: {evidence:#}"
        );
        assert_eq!(
            evidence["coverage"]["completeness"], "complete",
            "{file}: {evidence:#}"
        );
    }

    let evidence = file_dependents_evidence(&fixture, "apps/web/src/decoys.ts").await;
    assert_eq!(
        evidence["payload"]["dependent_files"],
        json!([]),
        "{evidence:#}"
    );
    assert_eq!(
        evidence["coverage"]["completeness"], "partial",
        "{evidence:#}"
    );
    assert_eq!(unsupported_omissions(&evidence), vec![1], "{evidence:#}");

    shutdown_graph_fixture(fixture).await;
}

#[tokio::test]
async fn typescript_same_module_exports_and_test_shadowing_bind_callers() {
    let fixture =
        graph_query_fixture_with_sources(|project| copy_fixture(Path::new(FIXTURE_ROOT), project))
            .await;

    for (qualified_name, expected_callers) in [
        // `export { hopped }`; `hopped()` inside `describe("hopped", …)`
        // calls the import, not the describe block.
        (
            "apps/web/src/hops.ts::hopped",
            vec!["calls the import", "consumeHops"],
        ),
        // `export { inner as renamed }` beside an unexported local `renamed`.
        ("apps/web/src/hops.ts::inner", vec!["consumeHops"]),
        ("apps/web/src/hops.ts::renamed", Vec::new()),
        // `export { relayTarget as relayed }` forwards a local import; the
        // helper named `relayed` shadows it only inside its describe block.
        (
            "apps/web/src/hop-target.ts::relayTarget",
            vec!["calls the imported relay", "consumeHops"],
        ),
        (
            "apps/web/test/shadowing.test.ts::relayed::relayed",
            vec!["calls the local helper"],
        ),
        ("apps/web/test/shadowing.test.ts::hopped", Vec::new()),
        ("apps/web/test/shadowing.test.ts::relayed", Vec::new()),
    ] {
        let (names, evidence) = callers_evidence(&fixture, qualified_name).await;
        assert_eq!(names, expected_callers, "{qualified_name}: {evidence:#}");
        assert_eq!(
            evidence["coverage"]["completeness"], "complete",
            "{qualified_name}: {evidence:#}"
        );
    }

    let evidence = file_dependents_evidence(&fixture, "apps/web/src/hop-target.ts").await;
    assert_eq!(
        evidence["payload"]["dependent_files"],
        json!([
            "apps/web/src/hop-consumer.ts",
            "apps/web/test/shadowing.test.ts"
        ]),
        "{evidence:#}"
    );

    shutdown_graph_fixture(fixture).await;
}
