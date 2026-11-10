//! Receiver evidence reaches the public callers surface without binding a
//! same-named class from the caller's module or an unproven receiver.

use super::{call_production_tool, graph_query_fixture_with_sources, shutdown_graph_fixture};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use tracedecay_contracts::retrieval::SymbolGraphScope;
use tracedecay_contracts::{
    CallableCodeSurfaceMeta, CodeCallersSurfaceRequest, ResultProjection, RetrievalOrder,
};

#[tokio::test]
async fn typescript_receiver_callers_follow_the_declared_type_and_disclose_unknown_receivers() {
    let fixture = graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/worker.ts"),
            r#"export class Worker {
  execute(): number { return 1; }
  fromLocal(): number { return 2; }
  fromAnnotation(): number { return 3; }
  guarded(): number { return 4; }
}
export async function createWorker(): Promise<Worker> {
  return new Worker();
}
"#,
        )
        .unwrap();
        fs::write(
            project.join("src/decoy.ts"),
            r#"export class Worker {
  execute(): number { return 99; }
  fromLocal(): number { return 99; }
  fromAnnotation(): number { return 99; }
  guarded(): number { return 99; }
}
"#,
        )
        .unwrap();
        fs::write(
            project.join("src/main.ts"),
            r#"import { createWorker as acquire, Worker as ActualWorker } from './worker';
import { Worker } from './decoy';

export async function runFactory(): Promise<number> {
  const worker = await acquire();
  return worker.execute();
}
export function runLocal(): number {
  const worker = new ActualWorker();
  return worker.fromLocal();
}
export function runParameter(worker: ActualWorker): number {
  return worker.fromAnnotation();
}
export function runTypedLocal(value: ActualWorker): number {
  const worker: ActualWorker = value;
  return worker.fromAnnotation();
}
export function shadowed(worker: ActualWorker, unknownValue: any): void {
  {
    const worker = unknownValue;
    worker.guarded();
  }
}
export function reassigned(unknownValue: any): void {
  let worker: ActualWorker = new ActualWorker();
  worker = unknownValue;
  worker.guarded();
}
"#,
        )
        .unwrap();
    })
    .await;

    for (qualified_name, expected_callers, expected_completeness) in [
        (
            "src/worker.ts::Worker::execute",
            vec!["runFactory"],
            "complete",
        ),
        (
            "src/worker.ts::Worker::fromLocal",
            vec!["runLocal"],
            "complete",
        ),
        (
            "src/worker.ts::Worker::fromAnnotation",
            vec!["runParameter", "runTypedLocal"],
            "complete",
        ),
        // The imported factory's return type is resolved in its defining
        // module, where Worker is not the decoy imported by main.ts.
        ("src/decoy.ts::Worker::execute", Vec::new(), "complete"),
        ("src/decoy.ts::Worker::fromLocal", Vec::new(), "complete"),
        (
            "src/decoy.ts::Worker::fromAnnotation",
            Vec::new(),
            "complete",
        ),
        // A shadowing binding or reassignment invalidates the receiver
        // proof. No fabricated call edge may make an empty answer complete.
        ("src/worker.ts::Worker::guarded", Vec::new(), "partial"),
        ("src/decoy.ts::Worker::guarded", Vec::new(), "partial"),
    ] {
        let target = call_production_tool(
            &fixture,
            "tracedecay_by_qualified_name",
            json!({"qualified_name": qualified_name, "format": "json"}),
            None,
            None,
        )
        .await
        .expect("exact receiver method");
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
        let result = call_production_tool(&fixture, "tracedecay_callers", arguments, None, None)
            .await
            .expect("receiver callers");
        let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        let evidence = &payload["outcome"]["value"];
        let mut names = evidence["payload"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("caller evidence missing: {payload:#}"))
            .iter()
            .map(|item| item["symbol"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names, expected_callers, "{qualified_name}: {evidence:#}");
        assert_eq!(
            evidence["coverage"]["completeness"], expected_completeness,
            "{qualified_name}: {evidence:#}"
        );
        let unsupported = evidence["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omission| omission["reason"] == "unsupported");
        assert_eq!(
            unsupported,
            expected_completeness == "partial",
            "{qualified_name}: {evidence:#}"
        );
    }

    shutdown_graph_fixture(fixture).await;
}
