# Test-Map / Test-Risk Calibration — Design Note

**Task:** `t_d000933c` (Design integration-test coverage calibration heuristics)
**Built on:** `docs/TEST-MAP-AUDIT.md` (`t_b1feb03c`) — the root-cause audit of the ~12% signal.
**Scope:** how `tracedecay_test_risk` and `tracedecay_test_map` should change so their coverage
signal is *defensible* on integration-heavy Rust repos. This is a design spec, not an implementation.

> **Thesis.** The headline number is wrong because it is a shallow *attribution* artifact, not a
> missing-tests signal. Calibration has two independent jobs: (1) **accuracy** — close the part of
> the undercount that is statically provable (transitive closure, trait dispatch, public-API
> imports, CLI entry points); and (2) **honesty** — stop conflating "statically attributed" with
> "tested," stop reporting orphans / non-Rust code as gaps, and label what is known to undercount.
> The two are equally important: a bigger number that still overstates is a regression.

---

## 1. Design principles (govern every heuristic below)

1. **Never overstate.** A heuristic is only admitted if its false-positive surface is bounded and
   the attribution is *auditable* (each covered function records *how* it was deemed covered).
2. **Attribution, not coverage.** The headline metric is "statically attributed to a test," a
   lower bound on exercised coverage. We say so explicitly and never round it up to "is tested."
3. **Distinguish direct vs. indirect.** Direct unit attribution (depth-1 `#[test]` caller) and
   integration/indirect attribution (closure, trait resolution, CLI entry) are reported as separate
   buckets so a reader can weight them differently.
4. **Match the existing tools, don't fork them.** `test_map` already computes depth-3; `test_risk`
   must converge to the same answer so zoom-in and aggregate agree.
5. **Cheap first.** Ship the zero-FP, high-yield change (closure + bucketing) before any heuristic
   with a real false-positive surface.

---

## 2. Heuristic set (phased)

Each heuristic lists **what**, **inputs**, **expected delta** (against the audit's reproduced
baseline of 4397 src fns / 542 attributed / 12.3%), and the **risk class**.

### H1 — Transitive closure to depth 3 in `test_risk`  *(Phase 1, mandatory, zero-FP)*

**What.** Replace the depth-1 seed step with a single seeded forward BFS. Seed = every node in a
test file **or** every `#[test]`-annotated fn (identical seed to today). Walk outgoing `Calls`
edges up to depth 3, marking every reached function attributed. Cap at 3 to match `test_map`.

**Inputs.** Same graph data the handler already loads (`all_nodes`, `all_edges`) plus the existing
`get_test_annotated_node_ids` + `is_test_file`. **No new extraction.**

**Implementation shape (required).** Do **not** call `get_callers(node, 3)` per source function
(that is ~4,397 backward BFS passes and will be slow on large graphs). Build a reverse-adjacency
view once and run **one forward BFS from the seed set** — exactly the shape the audit's Python
reimplementation used to reproduce the depth table. This is O(edges) total, not O(nodes × edges).

**Expected delta.** `attributed` 542 → ~1,103 (25.1% at depth 3; full-transitive ceiling 27.3%).
**Risk:** none (pure static reachability); the only "over-count" is cfg-gated/panic-only callees,
which the depth-1 path already suffers identically.

### H2 — Trait / dynamic-dispatch resolution to concrete impls  *(Phase 2, bounded-FP)*

**What.** When a reached node is a trait method (or a method reached only through a trait object),
attribute coverage to its concrete `impl` methods using the **`Implements`/`Extends` edges already
in the graph**. Recovers the per-language `CExtractor::extract_source` / `GoExtractor::extract_source`
/ `ExtractionState::node_text` families (audit Gap C/D).

**Inputs.** `Implements` and `Extends` edges; `nodes.kind == "method"`/`"impl"`; qualified names
for same-name matching. **No new extraction.**

**Attribution rule (conservatism gate).** Only attribute an impl method `M` of trait method `T` when:
- `T` is itself reached from a test (H1 already proved `T`), **and**
- the impl method `M` has the same simple name as `T`, **and**
- `M` lives in the **same crate** as the reached call site (block cross-crate fan-out), **and**
- the impl body is present in the graph (a resolved node, not a stub).

Do **not** enumerate every impl of a trait blindly; attribute only impls reachable within the same
crate boundary as the test's call.

**Expected delta.** Recovers the extractor families (~tens of fns per language × ~15 languages).
Modest in count, high in value (these are the literal things the `*_extraction_test.rs` suites run).
**Risk:** over-attribution across unrelated impls — see gate above.

### H3 — Public-API / integration-test import mapping  *(Phase 2, confidence-tagged)*

**What.** Integration tests in `tests/` exercise the crate via its **public surface**. Map a
`tests/` file to the **`pub`** symbols it both `use`s **and** calls: a `Uses` edge from the test to
a `pub` symbol, combined with at least one `Calls` edge into that symbol's reachable closure, is
evidence the public API is exercised. Attribute that symbol (and, via H1, its closure).

**Inputs.** `Uses` edges (already extracted for `use` statements); `visibility` column (`pub`).
**No new extraction.**

**Attribution rule.** Require **both** an import (`Uses` → `pub` symbol) **and** a call path into
that symbol's closure — import alone is *not* attribution (a test may import and never call). Symbols
attributed only this way carry `attribution_method: "public_api"` and a lower confidence than direct
calls, so they are separable in the report.

**Expected delta.** Lifts the `Database::*` / `TraceDecay::*` wrapper families reachable from
`dashboard_api_test.rs` etc. **Risk:** import-without-exercise → mitigated by the dual requirement.

### H4 — CLI / binary entry attribution  *(Phase 3, opt-in by default)*

**What.** `src/main.rs::run` is the #1 "top-risk-untested" yet is spawned by 10+ integration test
files via `Command::new("tracedecay")`. A process spawn emits **no `Calls` edge**, so it is
unreachable at any depth. Two complementary, opt-in mechanisms:

- **(a) Docstring convention (default path).** Extend the existing `skip-test-coverage` precedent:
  a `/// tested-by: cli-integration` (or `/// tested-by: <test-suite>`) docstring on a `main`/`run`
  fn marks it as integration-covered. Cheapest, zero inference, fully auditable.
- **(b) `Command::new(<bin>)` detection (opt-in flag).** When enabled, scan test-file source for
  `Command::new`/`Command::from` whose string-literal arg matches a declared `[[bin]]` name
  (from `Cargo.toml`) or the package name; attribute coverage to that bin's `main`/`run` entry.

**Inputs.** (a) docstrings (already queryable like `skip-test-coverage`); (b) `Cargo.toml` `[[bin]]`
names + a source scan of test files. Source scan is the one new *read*, not new *extraction*.

**Attribution rule.** Attribute to the bin entry (`run`/`main`) only; tag
`attribution_method: "cli_entry"`. **Conservative default:** mechanism (a) is always on; mechanism
(b) is off unless a flag/env is set, because (b) cannot distinguish a `--version`-only spawn from a
real exercise. When (b) fires, it lowers the bin's risk multiplier but never marks unrelated fns.

**Expected delta.** Recovers `run` and other `[[bin]]` entries (small count, but they are the
highest-risk functions). **Risk:** `--help`/`--version` over-attribution → mitigated by opt-in + the
`cli_entry` tag so a human can audit.

### H5 — Honest bucketing + calibrated confidence labels  *(Phase 1, mandatory, no FP)*

**What.** Stop reporting one number. Split the population and label the signal's nature. Computed
*after* H1–H4 attribution:

- **attributed** — statically reachable from a test (H1) or resolved (H2/H3/H4).
- **reachable_unattributed** — has incoming `Calls` (so not an orphan) but no static path from any
  test reaches it. This is the genuine attribution backlog (audit's ~1,063) — *likely tested via
  dispatch/process boundaries we can't see statically*. Never call these "untested."
- **orphan_entry** — zero incoming `Calls` edges. Includes real public entry points (e.g. `main`,
  which is orphan until H4 attributes it), trait impls whose only caller is dynamic, and genuine dead
  code. **`orphan_entry` ≠ dead code** — label it as "no static caller" and surface separately.
- **excluded** — non-`src/` code (dashboard Python, scripts, benches, `build.rs`). Removed from the
  denominator entirely (audit's 357).

Plus a **confidence label** on the aggregate: when any of H2/H3/H4 is active or the
`reachable_unattributed` bucket is large, emit `confidence: "static_lower_bound"` with a one-line
human note, and a `known_undercount` array
(`[{category, count}]`: `trait_dispatch`, `subprocess_cli`, `cross_language`) so a reader sees *why*
the number is a floor.

---

## 3. Inputs required (summary)

| Heuristic | New extraction? | Graph data used | Extra inputs |
|---|---|---|---|
| H1 closure | No | `Calls` edges + seed set | — |
| H2 trait/impl | No | `Implements`, `Extends`, `method`/`impl` nodes, qualified names | crate-boundary check |
| H3 public-API | No | `Uses` edges + `visibility` column | — |
| H4 CLI entry | No (docstring path) / source scan (opt-in path) | docstrings | `Cargo.toml` `[[bin]]`; opt-in flag for scan |
| H5 bucketing | No | incoming-edge counts, `file_path` for non-`src/` exclusion | — |

**Net new extraction: none.** Every heuristic is computable from data already in the graph plus
(optional, H4b) a source-text scan. This is a deliberate property: it keeps the change inside the
analysis layer and avoids touching tree-sitter extractors.

---

## 4. Expected output / wording changes

### 4.1 `tracedecay_test_risk` — summary

Current:
```json
"summary": { "total_functions", "tested", "skipped", "coverage_pct", "top_risk_untested" }
```

Proposed (additive; old fields preserved for one release, then `tested` is deprecated in favor of
`attribution`):
```json
"summary": {
  "total_functions": <denominator = attributed + reachable_unattributed + orphan_entry>,
  "coverage_pct": <attributed / total_functions, rounded — semantics now "statically attributed">,
  "top_risk_untested": <unchanged, but computed over reachable_unattributed + orphan_entry>,

  "attribution": {
    "depth": 3,
    "direct_unit_attributed":   <depth-1 #[test]-caller count — the old "tested" = 542 baseline>,
    "closure_attributed":       <added by depth 2–3 BFS>,
    "trait_resolved_attributed": <H2>,
    "public_api_attributed":     <H3>,
    "cli_entry_attributed":      <H4>,
    "total_attributed":          <sum = numerator of coverage_pct>
  },
  "buckets": {
    "attributed":              <total_attributed>,
    "reachable_unattributed":  <has callers, no static test path>,
    "orphan_entry":            <zero incoming Calls edges>,
    "excluded":                <non-src/: dashboard/scripts/benches — removed from denom>
  },
  "confidence": "static_lower_bound",
  "confidence_note": "coverage_pct is a static attribution lower bound; real exercised coverage is higher (see known_undercount).",
  "known_undercount": [
    { "category": "trait_dispatch",  "count": <n> },
    { "category": "subprocess_cli",  "count": <n> },
    { "category": "cross_language",  "count": <n> }
  ]
}
```

### 4.2 `tracedecay_test_risk` — per risk item

Add `attribution_method` (one of `direct_unit`, `closure`, `trait_resolved`, `public_api`,
`cli_entry`, `none`) so indirect/integration attribution is separable from direct unit mapping at
the row level. This is the mechanism that satisfies "weight broad integration suites separately
from direct unit mappings."

### 4.3 `tracedecay_test_map`

- Per test-caller: add `depth` (closure depth at which the test was found) and
  `attribution_method`, so the zoom-in view carries the same confidence information as the
  aggregate.
- Add an `inferred` section listing attributions that came from H2/H3/H4 (trait-resolved,
  public-API, CLI-entry) with a `confidence: "inferred"` flag, so a reader can tell exact edges
  from heuristic ones.

### 4.4 Wording

- Any prose/`top_risk_untested` framing changes from "untested" to **"no static test attribution"**.
- The headline number's caption becomes **"statically attributed to tests"**, not "tested."

---

## 5. False-positive risks & where the tool stays conservative

| Heuristic | False-positive risk | Mitigation / conservatism |
|---|---|---|
| H1 closure | cfg-gated / panic-only callees counted | Same exposure as depth-1 today; cap at depth 3. |
| H2 trait/impl | Over-attribution across unrelated impls | Same crate + same method name + reachable impl body only; never fan across crates. |
| H3 public-API | Import-without-exercise | Require import **and** a call path into the symbol's closure; tag `public_api` as separable/lower-confidence. |
| H4 CLI entry | `--version`/`--help` spawn marked as exercising `run` | (a) docstring path is opt-in by author intent; (b) `Command::new` scan is **off by default**, tag `cli_entry`, attribute bin entry only. |
| H5 bucketing | Mislabeling `orphan_entry` as dead code, or `reachable_unattributed` as untested | Explicit labels + human note; compute buckets *after* attribution. |

**Hard conservatism rules (do not relax):**
- **Cross-language edges:** do not invent Rust↔Python attribution. `dashboard/` Python handlers
  stay in `excluded`; never silently credited to Rust functions. (A future opt-in heuristic only.)
- **Dynamic dispatch beyond traits** (fn pointers, broad trait objects): do not enumerate all impls;
  attribute only impls that are themselves statically reachable.
- **Macros / generated code:** skip — unreliable node identity.
- **Benches / `build.rs` / scripts:** excluded from the denominator, never attributed.
- **Never mark a function `has_test: true` on inference alone without an `attribution_method` tag**
  — inference is always auditable/visible, never silent.

---

## 6. Validation plan

Reuse the audit's reproducible oracle (CLI output + direct SQLite on `.tracedecay/tracedecay.db` +
a Python reimplementation of the algorithm that already matched the tool to the function).

1. **Baseline parity (H1).** Before/after `tracedecay tool test_risk --json`: `direct_unit_attributed`
   must equal the old `tested` (542); `total_attributed` must match the audit's depth-3 figure (~1,103,
   25.1%). Assert exact match against the depth table.
2. **Trait resolution (H2).** `CExtractor::extract_source`, `GoExtractor::extract_source`, and the
   `ExtractionState::node_text` families must flip to attributed with `attribution_method:
   "trait_resolved"`. Negative spot-check: an unrelated `Display` impl must **not** gain attribution.
3. **Public-API (H3).** `Database::get_all_nodes` and `TraceDecay::get_all_nodes` resolve to
   attributed via `dashboard_api_test.rs`; `attribution_method` populated.
4. **CLI entry (H4).** `src/main.rs::run` becomes attributed with `attribution_method: "cli_entry"`
   only when the opt-in path fires; `--version`-only test does not over-attribute unrelated fns.
5. **Bucket invariants (H5).** `attributed + reachable_unattributed + orphan_entry == src fn count`;
   `excluded` count == audit's 357; `run` moves out of `orphan_entry` only after H4.
6. **Cross-tool parity.** Any function `test_risk` calls attributed must appear covered in
   `test_map`; the `depth`/`attribution_method` values must agree.
7. **Small-crate fixture.** A hand-built fixture crate with known coverage produces an exact expected
   attribution map (golden-file test) — guards against drift on the heuristics.
8. **Performance.** The seeded forward BFS completes within the existing `test_risk` budget on the
   full repo graph (7,669 fn nodes). Assert it does **not** regress vs. the current single-edge scan;
   the per-node `get_callers` anti-pattern is explicitly forbidden.

---

## 7. Phasing / rollout

- **Phase 1 (ship together, zero added FP):** H1 (closure) + H5 (bucketing + confidence labels).
  This alone turns a misleading "12% tested" into "25% statically attributed, ~1,063 reachable but
  unattributed, ~1,776 no static caller, 357 excluded — a lower bound." Mandatory, low-risk.
- **Phase 2 (bounded FP, behind the attribution tag):** H2 (trait/impl) + H3 (public-API). Each adds
  value and is independently auditable via `attribution_method`.
- **Phase 3 (opt-in):** H4 CLI entry. Mechanism (a) docstring always on; mechanism (b)
  `Command::new` scan gated behind a flag.

Phase 1 is the single change that closes most of the legitimately-closable gap with zero false
positives; Phases 2–3 recover the dispatch/process-boundary remainder and are each separable and
auditable. No phase is allowed to mark a function covered without recording *how*.
