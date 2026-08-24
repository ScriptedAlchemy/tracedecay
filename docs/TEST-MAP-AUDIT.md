# Test-Map Behavior Audit — Integration-Test Coverage Attribution

**Task:** `t_b1feb03c` (Audit current test-map behavior on integration tests)
**Scope:** why `tracedecay_test_risk` reports ~12% mapped function coverage for a repo with a large integration test suite.
**Method:** reproduced the tool output, read the implementation (`src/mcp/tools/handlers/health.rs`, `src/db/coverage.rs`, `is_test_file` in `src/tracedecay.rs`), and verified every claim against the live graph at `.tracedecay/tracedecay.db` with direct SQL + a Python reimplementation of the algorithm.

> Headline: the 12% is **not** "the tests are missing." It is "the static *attribution* of tests to functions is shallow." The real exercised surface is far higher; the gap is in how coverage is computed, not in what is tested.

---

## 1. Reproducing the current signal

```
$ tracedecay tool test_risk --json   →   summary:
    total_functions: 4397
    tested:           542
    skipped:            3
    coverage_pct:    12.0
    top_risk_untested: "run"
```

Independently recomputed from the DB: denominator **4397**, depth-1 tested **542**, **12.3%** (tool rounds to 12.0). Exact match.

Repo reality (counted from the graph): **7,669** function/method nodes total — **4,597** in `src/`, **2,710** in test files, **362** in `dashboard/` Python + scripts. **1,852** `#[test]`-annotated functions (**1,341** in `tests/`, **511** inline `#[cfg(test)]` in `src/`), across **143** indexed test files. The integration suite is large and fully indexed — so the low number is an attribution artifact, not missing tests.

---

## 2. How the mapping works (the algorithm)

`handle_test_risk` (`src/mcp/tools/handlers/health.rs:650`) marks a source function as **tested** iff it is the **direct (depth‑1) `Calls` target** of a caller that is either:

- a node located in a "test file" — `is_test_file()` (`src/tracedecay.rs:3746`) is a substring test for `tests/`, `test/`, `__tests__/`, `spec/`, `e2e/`, `.test.`, `.spec.`, `_test.`, `_spec.`; **or**
- a `#[test]`-annotated function — resolved via `annotates` edges from `annotation_usage` nodes named `test` (`src/db/coverage.rs:12`).

```rust
// health.rs:727 — the entire "tested" computation:
for e in &all_edges {
    if e.kind == EdgeKind::Calls {
        let is_test = node_to_file.get(&e.source).is_some_and(|f| is_test_file(f))
            || test_annotated_callers.contains(&e.source);
        if is_test { tested.insert(e.target.clone()); }
    }
}
```

Two structural properties follow directly:

1. **Depth‑1 only.** There is no transitive closure. A function reached only through `test → public_api → helper` is *not* counted.
2. **Edge-existence bound.** Coverage is capped by what the tree-sitter extractor emits as a resolved `Calls` edge. Calls that cross trait/dynamic-dispatch boundaries, wrapper layers, language boundaries, or process boundaries produce no edge — so the target is invisible even at infinite depth.

Note the asymmetry: the **per-file** `tracedecay_test_map` tool (`handle_test_map`, `health.rs:873`) uses `get_callers(&node.id, 3)` — **depth‑3**. So `test_map` (zoomed-in) and `test_risk` (headline) disagree by construction: a function `test_map` will happily list tests for can still be "untested" in the `test_risk` aggregate.

---

## 3. Quantified root-cause breakdown

Re-running the exact algorithm and then extending the same forward call graph with BFS from the identical seed set (test files + `#[test]` fns):

| Closure depth | tested within denom | coverage |
|---|---|---|
| **1 (current `test_risk`)** | **542** | **12.3%** |
| 2 | 955 | 21.7% |
| **3 (matches `test_map`)** | **1,103** | **25.1%** |
| 5 | 1,192 | 27.1% |
| full transitive (∞) | 1,201 | 27.3% |

**Reading:** transitive closure alone roughly *doubles* the attributed coverage (12.3% → 25.1% at depth 3, +561 functions). That is a pure, low-risk fix — `test_map` already does it.

The ceiling, though, is ~27%. **3,196 of 4,397 source functions (72.7%) are unreachable from any test by *any* static call chain.** Breaking that remainder down (src/ subset, 2,839 unreachable):

| Category | Count | Meaning |
|---|---|---|
| Zero incoming `Calls` edges | 1,776 | Orphans / public entries / dead code — nothing in the graph calls them at all |
| Has callers, chain never reaches a test | 1,063 | Genuinely exercised but the chain breaks at a dispatch/process boundary |
| Of the unreachable, are `method` nodes | 1,788 | Strong trait-impl / dynamic-dispatch signal |

Non-`src/` remainder (357 unreachable): `dashboard/` Python (242 — no cross-language edges), `scripts/` (51), `benchmarks/`/`benches/` (42), `eval/` (16), `build.rs` (5) — code the Rust test suite structurally cannot call.

---

## 4. Representative mapping gaps (file / function / test evidence)

### Gap A — Depth‑1 truncation hides transitively-covered helpers (largest fixable chunk)
These are high-fan-in helpers reached from tests only via 2–3 hops; `test_risk` reports them untested, `test_map` would list tests for them.

| Function | fan_in | first reached at depth |
|---|---|---|
| `src/mcp/tools/definitions.rs:19` `def` | 70 | 3 |
| `src/extraction/bash_extractor.rs:29` `ExtractionState::new` | 47 | 2 |
| `src/memory/store.rs:1573` `db_error` | 29 | 2 |
| `src/dashboard/util.rs:46` `query_rows` | 27 | 2 |
| `src/db/sql.rs:78` `collect_rows` | 17 | 2 |

### Gap B — Binary/CLI subprocess tests (no call edge ever)
`src/main.rs:250` `run` is the tool's **#1 top-risk-untested**. It is invoked by **10+** integration test files (`tests/cli_non_interactive_test.rs`, `tests/agent_test.rs`, `tests/copilot_agent_test.rs`, …) via `Command::new("tracedecay")` subprocess spawns. The graph records **0** test-caller edges to it (only 2 callers total, neither a test) and it is **unreachable at any depth** — spawning a process is not a function call. (Contrast: `dashboard::run`, which *is* called as a function in `tests/dashboard_api_test.rs`, resolves fine.) Every CLI entry point has this shape.

### Gap C — Fixture-driven behavioral coverage of extractor internals
Per-language extractor internals are exercised behaviorally by the `*_extraction_test.rs` fixture suites, but the call graph never reaches them:

- `src/extraction/glsl_extractor.rs:755` `GlslExtractor::find_descendant_by_kind` — reachable=False, callers=0
- `src/extraction/objc_extractor.rs:1436` `ObjcExtractor::find_descendant_by_kind` — reachable=False, callers=0
- `src/extraction/cpp_extractor.rs:75` `ExtractionState::node_text` — **39 internal callers**, but the chain from any test never reaches it
- `src/extraction/{c,dart,objc,lua,hlsl}_extractor.rs` `ExtractionState::node_text` — 10–26 internal callers each, all unreachable from tests

These are real, heavily-used functions. The chain breaks because tests call a generic extraction entry point and the per-language work is selected at runtime.

### Gap D — Trait / dynamic-dispatch and wrapper resolution
When a test calls through a trait object or a thin wrapper, the `Calls` edge lands on the trait method / generic / wrapper, **not** the concrete implementation the test actually exercises:

| Source function | depth‑1 test-callers | note |
|---|---|---|
| `src/tracedecay.rs:941` `TraceDecay::sync` | **30** | concrete type — resolves |
| `src/tracedecay.rs:277` `TraceDecay::init` | **106** | concrete type — resolves |
| `src/extraction/c_extractor.rs:77` `CExtractor::extract_source` | **0** | the literal thing `tests/c_extraction_test.rs` exercises — edge resolves to the generic/trait stub instead |
| `src/extraction/go_extractor.rs:73` `GoExtractor::extract_source` | **0** | same |
| `src/tracedecay.rs:2433` `TraceDecay::get_all_nodes` | 0 (depth 5) | tests call `Database::get_all_nodes` (16 test-callers); the wrapper is invisible at depth 1 |
| `src/tracedecay.rs:2361` `TraceDecay::get_callers` | 0 | edge lands on `GraphTraverser::get_callers` (6 test-callers) |

1,788 of the 2,839 unreachable src functions are method nodes — the footprint of this dispatch gap.

---

## 5. Recommendations — what is feasible to fix heuristically

Ranked by expected accuracy gain vs. implementation risk.

1. **Depth‑3 transitive closure in `test_risk` (HIGH value, LOW risk).** Reuse the exact machinery `test_map` already has (`get_callers(.., 3)`). Lifts attributed coverage **12.3% → ~25%** with zero false positives (still pure static reachability). *This single change closes most of the gap that is legitimately closable.* Cap depth at 3 to match `test_map` and bound cost; expose depth as a parameter if desired.

2. **Resolve trait/dynamic dispatch to concrete impls (MEDIUM value, MEDIUM risk).** When a reached node is a trait method, attribute coverage to its concrete `impl` methods (`impls`/`type_hierarchy` data already exists). Recovers the per-language `extract_source`/`node_text` families (Gap C/D). Risk: over-attribution across unrelated impls — mitigate by requiring same crate + same method name + reachable impl body.

3. **Cross-language + wrapper attribution (MEDIUM value, MEDIUM risk).** Attribute `Database::get_all_nodes` coverage up to its `TraceDecay::get_all_nodes` wrapper (single-callee forwarder), and recognize that `dashboard/` Python handlers are exercised by the Rust-spawned dashboard. Heuristic, opt-in.

4. **Subprocess / CLI entry attribution (LOW breadth, easy).** Gap B is real but narrow (a handful of binary entry points). Cheapest fix: an opt-in docstring convention (`/// tested-by: cli-integration`) feeding the existing `skip-test-coverage`-style annotation path, or detecting `Command::new(<crate>)` call sites and attributing to `main::run` / declared `[[bin]]` targets. Low priority — affects few functions, but they are high-risk ones (`run` is the #1 risk).

5. **Do NOT chase the 72.7% unreachable remainder as "untested."** 1,776 of those have *zero* incoming edges (orphans/public entries/dead) and 357 are non-Rust (`dashboard/` Python, scripts, benches). Reporting these as "test gaps" is misleading. Recommend `test_risk` separately report (a) attributed coverage, (b) unreachable-with-callers (the genuine attribution backlog, ~1,063), and (c) orphan/entry count — so the headline number reflects attribution quality, not a false alarm about missing tests.

**Bottom line:** the cheapest, highest-integrity improvement is item 1 (depth‑3 closure) plus item 5 (honest bucketing). Together they turn a misleading "12% tested" into a defensible "~25% statically attributed, ~X reachable-but-unattributed via dispatch, Y orphan/entry" picture — without overstating real test coverage.
