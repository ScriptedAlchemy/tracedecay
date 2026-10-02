//! A sealed graph binds every call site it can and discloses the rest, so a
//! relation answer is complete only when no call site went unbound.

use std::sync::Arc;

use tracedecay_code_index::graph_projection::CodeGraphInteractiveReader;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::NeverCancelled;

use crate::cross_file_import_calls::publish_fixture_tree;
use crate::typescript_module_resolution::reader;

const EVENTS_TS: &str = r#"export class Logger {
  handle(s: string): number {
    return s.length;
  }
}
export function formatEvent(e: string): string {
  return `[${e}]`;
}
export function processAll(items: string[]): string[] {
  return items.map((item) => formatEvent(item));
}
"#;

const MAIN_TS: &str = r#"import { Logger, formatEvent, processAll } from "./events";
export function runPipeline(): number {
  const logger = new Logger();
  const counts = processAll(["a", "b"]);
  const callback = (s: string) => logger.handle(s);
  return callback("x") + counts.length + formatEvent("y").length;
}
"#;

const SCORING_PY: &str = "class Scorer:\n    def score(self, value):\n        return value * 2\n\n\ndef rank_all(values):\n    return sorted(values)\n";

const APP_PY: &str = "from pkg.scoring import Scorer, rank_all\ndef main_entry():\n    s = Scorer()\n    return s.score(5) + len(rank_all([3, 1, 2]))\n";

const LIB_RS: &str = r#"pub struct Payload {
    material: String,
}

impl Payload {
    fn canonicalize_material(raw: &str) -> String {
        raw.trim().to_owned()
    }

    pub fn new(raw: &str) -> Self {
        let material = Self::canonicalize_material(raw);
        Self { material }
    }

    pub fn material(&self) -> &str {
        &self.material
    }
}

pub fn describe(all: &[Payload]) -> usize {
    all.iter().map(|payload| payload.material().len()).sum()
}

pub fn square_area(side: f64) -> f64 {
    side * side
}

pub fn areas(sides: &[f64]) -> Vec<f64> {
    sides.iter().map(|side| square_area(*side)).collect()
}
"#;

struct SealedGraph {
    generation: Arc<CodeIndexPublishedGenerationV1>,
    reader: CodeGraphInteractiveReader,
}

impl SealedGraph {
    fn symbol(&self, qualified_name: &str) -> SymbolOccurrenceId {
        self.generation
            .symbols()
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified_name)
            .unwrap_or_else(|| panic!("missing symbol {qualified_name}"))
            .occurrence
            .clone()
    }

    fn callers(&self, qualified_name: &str) -> Vec<String> {
        let target = self.symbol(qualified_name);
        let mut callers = self
            .generation
            .edges()
            .iter()
            .filter(|edge| edge.kind == RelationEdgeKindV1::Calls && edge.to_occurrence == target)
            .filter_map(|edge| {
                self.generation
                    .symbols()
                    .symbols
                    .iter()
                    .find(|symbol| symbol.occurrence == edge.from_occurrence)
                    .map(|symbol| symbol.qualified_name.clone())
            })
            .collect::<Vec<_>>();
        callers.sort();
        callers
    }

    /// Whether `callers(qualified_name)` must disclose call sites it could
    /// not bind.
    fn callers_partial(&self, qualified_name: &str) -> bool {
        !self
            .reader
            .unresolved_caller_gaps(
                &[self.symbol(qualified_name)],
                None,
                Arc::new(NeverCancelled),
            )
            .expect("unresolved caller probe")
            .is_empty()
    }

    /// Whether `callees(qualified_name)` must disclose call sites it could
    /// not bind.
    fn callees_partial(&self, qualified_name: &str) -> bool {
        !self
            .reader
            .unresolved_callee_gaps(&[self.symbol(qualified_name)], Arc::new(NeverCancelled))
            .expect("unresolved callee probe")
            .is_empty()
    }
}

fn sealed_graph() -> SealedGraph {
    let root = tempfile::tempdir().expect("fixture root");
    for (path, source) in [
        ("ts/src/events.ts", EVENTS_TS),
        ("ts/src/main.ts", MAIN_TS),
        ("py/pkg/__init__.py", ""),
        ("py/pkg/scoring.py", SCORING_PY),
        ("py/pkg/app.py", APP_PY),
        ("rs/src/lib.rs", LIB_RS),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture dir");
        std::fs::write(path, source).expect("fixture source");
    }
    let generation = publish_fixture_tree(root.path(), "relation-coverage");
    let reader = reader(&generation);
    SealedGraph { generation, reader }
}

#[test]
fn rust_self_path_calls_bind_the_enclosing_impl_function() {
    let graph = sealed_graph();
    assert_eq!(
        graph.callers("rs/src/lib.rs::Payload::canonicalize_material"),
        ["rs/src/lib.rs::Payload::new"]
    );
    assert!(!graph.callers_partial("rs/src/lib.rs::Payload::canonicalize_material"));
}

#[test]
fn unbound_receiver_calls_make_callers_and_callees_partial() {
    let graph = sealed_graph();

    // `logger.handle(s)` on a local binds no edge.
    assert_eq!(
        graph.callers("ts/src/events.ts::Logger::handle"),
        Vec::<String>::new()
    );
    assert!(graph.callers_partial("ts/src/events.ts::Logger::handle"));
    assert_eq!(
        graph.callers("ts/src/events.ts::formatEvent"),
        [
            "ts/src/events.ts::processAll",
            "ts/src/main.ts::runPipeline"
        ]
    );
    assert!(!graph.callers_partial("ts/src/events.ts::formatEvent"));

    // `payload.material()` on an untyped closure parameter binds no edge.
    assert!(graph.callers_partial("rs/src/lib.rs::Payload::material"));
    assert_eq!(
        graph.callers("rs/src/lib.rs::square_area"),
        ["rs/src/lib.rs::areas"]
    );
    assert!(!graph.callers_partial("rs/src/lib.rs::square_area"));

    // `s.score(5)` leaves `main_entry` with a callee the graph cannot list.
    assert!(graph.callees_partial("py/pkg/app.py::main_entry"));
    assert!(!graph.callees_partial("py/pkg/scoring.py::Scorer::score"));
}
