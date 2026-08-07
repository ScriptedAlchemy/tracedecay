# Current V2 delivery

**Status:** active product delivery.

`00-plan-set-index.md` is the sole precedence, rejection, and acceptance
authority. This file tracks only current product outcomes, blockers, and the
next direct user journeys. Numbered plans own the detailed semantics.

## Current outcomes

- CLI, MCP, HTTP, SSE, LSP, dashboard, and supported host integrations reach
  the same daemon/application owners and preserve one typed result model.
- Post-edit diagnostics, impact, affected tests, CI localization, read-only
  GitHub review evidence, and agent proximity remain generation-bound,
  authorized, and read-only.
- Saved edits trigger bounded background code and semantic indexing without
  delaying project open or exact, lexical, and graph retrieval.
- Only complete compatible generations publish. Stale, partial, denied,
  cancelled, failed, and unavailable work remains distinguishable from a
  complete empty result.
- Default release artifacts include the supported semantic runtime and pass
  real package, install, startup, and host-integration journeys.
- The implemented dashboard checkpoint is retained. Remaining dashboard work
  is the direct Plan 11 renderer, browser, accessibility, and usability
  journeys rather than reconstruction of old frontend scaffolding; the
  performance and payload budgets were withdrawn by owner decision 2026-07-31.

## Current blockers

- Doctor reports unavailable authority audit data and Cursor Core has an
  unresolved component-ownership conflict.
  Plans 09 and 27 own the corresponding product repairs.
  (Update 2026-08-07: both code repairs are landed — the doctor report now
  runs the real read-only authority-audit pass instead of hardcoding
  unavailable, and Cursor Core drift was separated from ownership conflict
  with the component-set transaction as the sole receipt-owned writer.
  Closure still requires the real journeys: a doctor run showing measured
  audit coverage, and a clean Cursor install → version bump → doctor pass on
  an operator machine, whose lifecycle receipts root does not currently
  exist.)
- Semantic search is unavailable because the active configuration snapshot is
  invalid. Plan 20 owns snapshot repair and Plan 31 owns semantic activation;
  exact, lexical, and graph retrieval must remain available.
  (Update 2026-08-07: the snapshot-invalidity cause is repaired at tip. The
  semantic retrieval state, pending-transition, accepted-profile, and
  receipt-key tables are now provisioned by the canonical configuration
  schema and the shadow `ensure_schema` paths that made an admitted store
  fail exact-final-shape validation are deleted — `fix(config): own semantic
  retrieval tables in configuration schema` (863e6a0a87). Recovery from a
  verified model-lifecycle event is owned by
  `src/daemon/semantic_activation_reconciler.rs`, mounted per project by
  `feat(daemon): mount graph publication and staged evaluation lanes`
  (36cd35b19c). Semantic search remaining inactive is therefore no longer a
  snapshot defect: activation is still gated *by design* on the Plan 15 Linux
  evaluation, which this docs lane does not close. Whether a live profile now
  admits a valid snapshot is unverified here.)
- Incremental indexing has shown unacceptable refresh staleness. Plan 25 owns
  cadence and freshness while preserving serve-during-refresh behavior.
- The repository still has unresolved test and CI failures. Focused local
  success does not establish product acceptance; normal repository CI must
  execute the affected journeys non-vacuously.
- The dashboard remains open on the direct Plan 11 renderer fallback, real-browser,
  assistive-technology, and usability journeys. The performance,
  sustained-update, and payload budgets were withdrawn by owner decision
  2026-07-31 and no longer block acceptance.

## Next direct journeys

1. **Production surface reachability.** Exercise representative CLI, MCP,
   HTTP/SSE, and negotiated LSP clients through the same application
   operations, including cancellation, continuation, restart, and truthful
   unavailable behavior.
2. **Host installation and feedback.** Use the official install, update,
   repair, and uninstall operations on supported hosts; trigger real edit and
   stop events; observe authorized feedback; preserve unrelated configuration;
   and recover from interruption and competing ownership.
3. **Incremental indexing.** Save, rename, delete, switch refs, overflow a hint
   source, cancel, and restart while exact identity is preserved, unrelated
   work is avoided, prior complete generations remain queryable, and semantic
   results appear only after complete publication.
4. **Distribution.** Build, package, install, start, and use the default
   distribution with semantic search and supported host bundles enabled.
   Unsupported platforms or capabilities must report typed unavailable state.
5. **Flagship dashboard.** Start from a real feedback finding, navigate to
   exact evidence, diagnose a real injected fault, perform an authorized
   setting or remediation action, observe the resulting state, and complete
   the Plan 11 browser, accessibility, and usability journeys.
6. **Repository verification.** Run focused direct product tests for changed
   behavior and ordinary Linux, macOS, and Windows CI. Treat missing,
   skipped, empty-filter, partial, or timed-out coverage as unresolved.

## Completion condition

The active slice is complete when supported surfaces and host installations
exercise the same production behavior, incremental indexing is bounded and
fresh, ordinary retrieval remains available during background work, only
complete compatible generations publish, distribution journeys succeed, and
direct product tests plus normal CI report truthful results.
