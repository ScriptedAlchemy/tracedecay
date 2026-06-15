# Reading the `test_risk` / `test_map` coverage signal

> **Audience:** maintainers reading `tracedecay_test_risk` and `tracedecay_test_map` output.
> **One-line summary:** the headline `coverage_pct` is a **static attribution
> lower bound**, not line/branch coverage from a profiler. Treat it as a floor,
> weight it by *how* a function was attributed, and expect it to read low on
> integration-heavy repos — that is the tool being honest, not a verdict that
> your code is untested.

This guide explains what the two tools actually measure, where they are known to
undercount, and how to read a risk report without overstating coverage
certainty. For the *why* behind the calibration (root-cause audit and the
heuristic design), see
[`TEST-MAP-AUDIT.md`](./TEST-MAP-AUDIT.md) and
[`TEST-MAP-CALIBRATION-DESIGN.md`](./TEST-MAP-CALIBRATION-DESIGN.md).

---

## 1. What "coverage" means here

Neither tool runs your tests. Both walk the **static call graph** extracted by
tracedecay and ask: *"is there a path of `Calls` edges from a test to this
function?"* The answer is an **attribution** decision, not an execution
measurement.

| Term you'll see | What it actually means |
|---|---|
| `coverage_pct` | `attributed / total_functions` — the share of functions **statically reachable** from a test within the configured depth. A lower bound on exercised coverage. |
| `has_test` (per risk item) | `true` when the function was attributed by *any* method (direct or closure). It does **not** mean a test asserts this function's behavior. |
| `confidence: "static_lower_bound"` | The aggregate is explicitly labeled a floor. Real executed coverage is higher. |

A function can be exercised by your test suite yet still report
`has_test: false`, because the exercise happens through a boundary static
analysis cannot see (see [§3](#3-known-undercount-scenarios)). The reverse is
also possible but rare and conservative: a cfg-gated/panic-only callee may be
counted as attributed because the static `Calls` edge exists even though it is
never run in your configuration.

---

## 2. Direct vs. calibrated (closure) attribution

Every attributed function records *how* it was deemed covered, in
`attribution_method`. There are two live methods today:

| `attribution_method` | `attribution_depth` | Meaning | Risk multiplier |
|---|---|---|---|
| `direct_unit` | `1` | A test calls this function **directly** (depth-1 `#[test]` caller, or a caller that lives in a test file). This is the strongest, smallest signal — a real, named test points straight at this function. | `0.1` |
| `closure` | `2` or `3` | This function is reachable from a test through 1–2 intermediate functions. It is **calibrated integration-style evidence**: a test exercises code that eventually reaches this function, but no test names it directly. | `0.4` |
| `none` | `null` | No static path from any test reaches this function within depth 3. | `1.0` |

(Other methods — `trait_resolved`, `public_api`, `cli_entry` — are defined in
the design but **not yet implemented**; their counts are reported as `0` in the
`attribution` block. See [§3](#3-known-undercount-scenarios).)

**How to read the two methods differently.** `direct_unit` is the signal you can
rely on the way you'd rely on a unit-test mapping. `closure` is intentionally
weaker: it tells you "some test reaches the neighborhood of this function," so
it *reduces* the residual risk score (multiplier `0.4` vs `1.0`) but does **not**
erase it (it stays well above `direct_unit`'s `0.1`). When you are deciding
where to write the next test, prefer a `closure`-attributed high-complexity
function over a `direct_unit`-attributed one — the former has only broad
behavioral evidence today.

The aggregate `summary.attribution` block breaks the numerator out by method:

```jsonc
"attribution": {
  "depth": 3,
  "direct_unit_attributed": <depth-1 count — strongest signal>,
  "closure_attributed":     <depth 2–3 count — calibrated integration evidence>,
  "trait_resolved_attributed": 0,   // designed (H2), not shipped
  "public_api_attributed":     0,   // designed (H3), not shipped
  "cli_entry_attributed":      0,   // designed (H4), not shipped
  "total_attributed":          <numerator of coverage_pct>
}
```

`test_map` finds test callers via the same depth-3 walk, but its output lists
each matching test without a per-test `depth`/`attribution_method` tag. So in
the zoom-in `test_map` view you cannot currently tell a direct test edge from a
depth-3 transitive one by the field alone — use `test_risk`'s
`attribution_method`/`attribution_depth` when you need that distinction. (Adding
the tag to `test_map` output is tracked as design §4.3, not yet shipped.)

---

## 3. Known undercount scenarios

These are the cases where a function **is** tested but reports no static
attribution. They are why `coverage_pct` is a *lower bound*.

1. **Beyond the depth-3 cap.** A function reached from a test only through 4+
   call hops is reported unattributed. The cap exists so `test_risk` and
   `test_map` agree and the walk stays cheap; it is a deliberate ceiling, not a
   measurement of "how far tests reach."
2. **Dynamic dispatch (trait objects, function pointers).** A `Calls` edge lands
   on the *trait method* node, not its concrete `impl` methods, so the concrete
   impls are not attributed even when the trait method is reached. Trait/impl
   resolution (`trait_resolved`, design H2) is designed to recover these but is
   **not shipped**.
3. **Subprocess / CLI entry points.** Integration tests that spawn the binary
   via `Command::new("tracedecay")` emit **no `Calls` edge**, so an entry like
   `src/main.rs::run` is unreachable at any depth. CLI-entry attribution
   (`cli_entry`, design H4) is designed (docstring `/// tested-by:` always-on
   path + opt-in `Command::new` scan) but **not shipped**. Until then, mark such
   entries with `/// skip-test-coverage` if you want them out of the risk view.
4. **Cross-language boundaries.** tracedecay does not invent Rust↔Python
   attribution. A Rust function exercised only through a dashboard Python
   handler is not credited to either side.
5. **Macros and generated code.** Node identity is unreliable inside generated
   code, so attributions there may be missing or imprecise.

The `summary.confidence_note` field states the floor property inline so the
report never appears more certain than it is.

---

## 4. How to read a risk report

Read the report top-down:

1. **Start with the buckets, not the percentage.**
   ```jsonc
   "buckets": {
     "attributed":             <reachable from a test, depth ≤ 3>,
     "reachable_unattributed": <has incoming Calls, but no static test path>,
     "orphan_entry":           <zero incoming Calls edges>,
     "excluded":               <non-src removed from the denominator — non-zero, repo-dependent>
   }
   ```
   - `reachable_unattributed` is the **genuine attribution backlog**: functions
     that are called by real code (so not dead) but that no static test path
     reaches. These are *likely tested through a dispatch/process boundary we
     can't see* — **never call them "untested."** On an integration-heavy repo
     this bucket is large, and that is expected.
   - `orphan_entry` is **not** dead code. It includes real entry points (`main`,
     which has no static caller), trait impls whose only caller is dynamic, and
     genuine dead code. Surface it for human triage, don't treat it as a cleanup
     list.
   - `excluded` (non-`src/`: dashboard Python, scripts, benches, `build.rs`)
     is removed from the denominator (Phase-1 bucketing, **shipped**). On an
     integration-heavy repo it is a large number — those are real non-`src/` nodes
     correctly kept out of the attribution numerator, not a coverage gap.

2. **Then read `coverage_pct` as a floor.** "25% statically attributed" means
   *at least* 25% of functions are statically reachable from a test. The
   unattributed remainder is split into "likely tested but we can't see how"
   (`reachable_unattributed`) and "no static caller at all" (`orphan_entry`).

3. **Use `attribution_method` to weight what you see.** A `closure`-attributed
   function is covered by *broad behavioral evidence* only — if it is also
   high-complexity or high-churn, it is a better "next test" candidate than a
   `direct_unit`-attributed function of the same raw risk. `top_risk_unattributed`
   (`== top_risk_untested` today) names the single highest-risk function with no
   attribution at all — usually the best place to start.

4. **Cross-check with `test_map`.** `test_map(file=...)` gives the zoom-in:
   which tests (file + name) reach a file's functions, and which functions have
   no test caller up to depth 3. Remember a listed test may be a direct caller
   *or* a depth-2/3 transitive caller — the per-test depth is not currently
   exposed, so when it matters, confirm with `test_risk(node/...)`'s
   `attribution_method`.

5. **Do not round the signal up to "tested."** The whole point of the
   `direct_unit` / `closure` / `confidence` split is to keep "statically
   attributed" honest. A function attributed only via `closure` is *probably*
   exercised and *not* asserted. State findings that way in reviews and PRs.

---

## 5. Related

- [`TEST-MAP-AUDIT.md`](./TEST-MAP-AUDIT.md) — root-cause audit of the original
  shallow-attribution signal.
- [`TEST-MAP-CALIBRATION-DESIGN.md`](./TEST-MAP-CALIBRATION-DESIGN.md) — the
  phased heuristic design (H1 closure + H5 bucketing shipped as Phase 1;
  H2 trait/impl, H3 public-API, H4 CLI-entry designed, not yet shipped).
- `/// skip-test-coverage` docstring convention — marks genuinely-untestable
  functions so they leave the risk view cleanly (see the User Guide).
