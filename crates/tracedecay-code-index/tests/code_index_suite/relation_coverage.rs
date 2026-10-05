//! A sealed graph binds every call site it can and discloses the rest, so a
//! relation answer is complete only when no call site went unbound.

use std::sync::Arc;

use tracedecay_code_index::graph_projection::{CodeGraphInteractiveReader, CodeGraphSymbolRefV1};
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
            .unresolved_callee_gaps(
                &[
                    CodeGraphSymbolRefV1::for_occurrence(&self.symbol(qualified_name))
                        .expect("symbol graph identity"),
                ],
                Arc::new(NeverCancelled),
            )
            .expect("unresolved callee probe")
            .is_empty()
    }
}

fn sealed_graph() -> SealedGraph {
    sealed_graph_of(&[
        ("ts/src/events.ts", EVENTS_TS),
        ("ts/src/main.ts", MAIN_TS),
        ("py/pkg/__init__.py", ""),
        ("py/pkg/scoring.py", SCORING_PY),
        ("py/pkg/app.py", APP_PY),
        ("rs/src/lib.rs", LIB_RS),
    ])
}

fn sealed_graph_of(files: &[(&str, &str)]) -> SealedGraph {
    let root = tempfile::tempdir().expect("fixture root");
    for (path, source) in files {
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

const NAMESPACES_TS: &str = r#"export function formatEvent(e: string): string {
  return e.trim();
}

namespace Plain {
  export function plainHelper(): string {
    return formatEvent("plain");
  }
}

export namespace Outer {
  export namespace Inner {
    export function deepHelper(x: number): string {
      return formatEvent(String(x));
    }
  }
}

module Legacy {
  export function legacyHelper(): string {
    return formatEvent("legacy");
  }
}

namespace Dotted.Path {
  export function dottedHelper(): string {
    return formatEvent("dotted");
  }
}
"#;

#[test]
fn typescript_namespace_members_are_symbols_and_bound_callers() {
    let graph = sealed_graph_of(&[("ts/src/events.ts", NAMESPACES_TS)]);

    assert_eq!(
        graph.callers("ts/src/events.ts::formatEvent"),
        [
            "ts/src/events.ts::Dotted::Path::dottedHelper",
            "ts/src/events.ts::Legacy::legacyHelper",
            "ts/src/events.ts::Outer::Inner::deepHelper",
            "ts/src/events.ts::Plain::plainHelper",
        ]
    );
    assert!(!graph.callers_partial("ts/src/events.ts::formatEvent"));
}

const COMPUTED_RECEIVERS_TS: &str = r#"export class Box {
  map(n: number): number {
    return n;
  }
  handle(): void {}
  save(): void {}
}

export function makeBox(): Box {
  return new Box();
}

export function run(rows: Box[]): number[] {
  makeBox().handle();
  rows[0].save();
  return [1, 2].map((n) => n + 1);
}
"#;

#[test]
fn typescript_calls_on_computed_receivers_make_callers_partial() {
    let graph = sealed_graph_of(&[("ts/src/box.ts", COMPUTED_RECEIVERS_TS)]);

    for method in ["map", "handle", "save"] {
        let target = format!("ts/src/box.ts::Box::{method}");
        assert_eq!(graph.callers(&target), Vec::<String>::new(), "{target}");
        assert!(graph.callers_partial(&target), "{target} callers partial");
    }
    assert_eq!(
        graph.callers("ts/src/box.ts::makeBox"),
        ["ts/src/box.ts::run"]
    );
    assert!(graph.callees_partial("ts/src/box.ts::run"));

    let mut gaps = graph
        .generation
        .unresolved_import_calls()
        .into_iter()
        .map(|gap| {
            let span = gap.evidence_span.start_byte as usize..gap.evidence_span.end_byte as usize;
            (gap.reference_name, &COMPUTED_RECEIVERS_TS[span])
        })
        .collect::<Vec<_>>();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            ("<computed>.handle".to_owned(), "handle"),
            ("<computed>.map".to_owned(), "map"),
            ("<computed>.save".to_owned(), "save"),
        ]
    );
}

#[test]
fn a_crlf_cargo_manifest_names_its_crate_for_cross_crate_calls() {
    let graph = sealed_graph_of(&[
        (
            "lib/Cargo.toml",
            "[package]\r\nname = \"crlf-lib\"\r\nversion = \"0.1.0\"\r\n",
        ),
        ("lib/src/lib.rs", "pub fn helper() -> u32 {\n    7\n}\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        ),
        (
            "app/src/main.rs",
            "fn main() {\n    let _ = crlf_lib::helper();\n}\n",
        ),
    ]);

    assert_eq!(
        graph.callers("lib/src/lib.rs::helper"),
        ["app/src/main.rs::main"]
    );
    assert!(!graph.callers_partial("lib/src/lib.rs::helper"));
}

#[test]
fn a_type_path_call_with_two_inherent_candidates_makes_both_callers_partial() {
    let graph = sealed_graph_of(&[
        (
            "rs/src/lib.rs",
            "mod unix;\nmod windows;\n\npub struct Clock;\n\npub fn now() -> u64 {\n    Clock::tick()\n}\n",
        ),
        (
            "rs/src/unix.rs",
            "use crate::Clock;\n\n#[cfg(unix)]\nimpl Clock {\n    pub fn tick() -> u64 {\n        1\n    }\n}\n",
        ),
        (
            "rs/src/windows.rs",
            "use crate::Clock;\n\n#[cfg(windows)]\nimpl Clock {\n    pub fn tick() -> u64 {\n        2\n    }\n}\n",
        ),
    ]);

    for target in [
        "rs/src/unix.rs::Clock::tick",
        "rs/src/windows.rs::Clock::tick",
    ] {
        assert_eq!(graph.callers(target), Vec::<String>::new(), "{target}");
        assert!(graph.callers_partial(target), "{target} callers partial");
    }
    assert!(graph.callees_partial("rs/src/lib.rs::now"));
}

#[test]
fn a_same_file_call_with_two_candidates_makes_both_callers_partial() {
    let graph = sealed_graph_of(&[(
        "rb/units.rb",
        "def scale(x)\n  3\nend\n\ndef scale(x)\n  1\nend\n\ndef total\n  scale(1)\nend\n",
    )]);

    let scales = graph
        .generation
        .symbols()
        .symbols
        .iter()
        .filter(|symbol| symbol.qualified_name == "rb/units.rb::scale")
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    assert_eq!(scales.len(), 2);
    let caller_gaps = graph
        .reader
        .unresolved_caller_gaps(&scales, None, Arc::new(NeverCancelled))
        .expect("unresolved caller probe");
    assert!(!caller_gaps.is_empty());
    assert!(graph.callees_partial("rb/units.rb::total"));
}

#[test]
fn a_same_file_python_call_with_two_candidates_makes_its_callees_partial() {
    let graph = sealed_graph_of(&[(
        "py/units.py",
        "def scale(x):\n    return x\n\ndef scale(x):\n    return x + 1\n\ndef total():\n    return scale(1)\n",
    )]);

    assert!(graph.callees_partial("py/units.py::total"));
}

#[test]
fn locally_ambiguous_definitions_shadow_an_imported_name() {
    let graph = sealed_graph_of(&[
        ("py/pkg/__init__.py", ""),
        ("py/pkg/lib.py", "def scale(x):\n    return x * 2\n"),
        (
            "py/pkg/app.py",
            "from pkg.lib import scale\n\ndef scale(x):\n    return x\n\ndef scale(x):\n    return x + 1\n\ndef total():\n    return scale(1)\n",
        ),
    ]);

    assert_eq!(graph.callers("py/pkg/lib.py::scale"), Vec::<String>::new());
    assert!(graph.callees_partial("py/pkg/app.py::total"));
}

#[test]
fn go_ambiguous_local_calls_disclose_gaps_beside_bound_calls() {
    let graph = sealed_graph_of(&[(
        "go/units.go",
        "package units\n\nfunc scale(x int) int { return x }\nfunc scale(x int) int { return x + 1 }\nfunc known(x int) int { return x * 2 }\nfunc total() int { return scale(1) + known(1) }\n",
    )]);

    assert_eq!(graph.callers("go/units.go::known"), ["go/units.go::total"]);
    assert!(graph.callers_partial("go/units.go::scale"));
    assert!(graph.callees_partial("go/units.go::total"));
}

#[test]
fn typescript_ambiguous_local_calls_do_not_poison_imported_call_resolution() {
    let shadowed = "describe('scope', () => {\n  function scale(x: number) { return x; }\n  function scale(x: number) { return x + 1; }\n  it('shadowed', () => { scale(1); });\n});\n";
    let clear = "export function clear(): number { return scale(1); }\n";
    for functions in [format!("{shadowed}{clear}"), format!("{clear}{shadowed}")] {
        let app = format!("import {{ scale }} from './lib';\n{functions}");
        let graph = sealed_graph_of(&[
            (
                "ts/lib.ts",
                "export function scale(x: number) { return x * 2; }\n",
            ),
            ("ts/app.ts", &app),
        ]);

        assert_eq!(graph.callers("ts/lib.ts::scale"), ["ts/app.ts::clear"]);
        assert!(graph.callees_partial("ts/app.ts::scope::shadowed"));
        assert!(!graph.callees_partial("ts/app.ts::clear"));
    }
}
