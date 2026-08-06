# Current V2 delivery

**Status:** active product delivery.

`00-plan-set-index.md` is the sole precedence, rejection, and acceptance
authority. This file tracks only current product outcomes, blockers, and the
next direct user journeys. Numbered plans own the detailed semantics.
`GAP-LEDGER-PR8-PR14.md` is history, not a work queue.

## Intended final outcome

- CLI, MCP, HTTP, SSE, LSP, dashboard, SDK, automation, and supported host
  integrations are thin clients over the same daemon/application authority and
  preserve one typed result model.
- TraceDecay V2 accepts only final V2 stores. Old TraceDecay stores return
  typed `ResetRequired`; historical host transcripts/logs are ingested as
  ordinary V2 capture when available and authorized.
- Durable facts are project-wide. Branch/ref/worktree/commit/PR/session/agent
  identity is provenance only, never a fact-storage shard.
- LCM compression, live-turn projection, and session-boundary mutation are
  daemon hook-runtime lifecycle effects. Public LCM surfaces are read-only
  retrieval/diagnostics except explicit refresh; Doctor is read-only.
- Grafeo through `tracedecay-graph-db` is the embedded durable graph/vector
  authority. SQLite remains for relational, transactional, content-bearing
  records.
- Post-edit diagnostics, impact, affected tests, CI localization, Git review
  evidence, and agent proximity are generation-bound, authorized, and truthful.
- Saved edits trigger bounded background code and semantic indexing without
  delaying project open or exact, lexical, and graph retrieval.
- Only complete compatible generations publish. Stale, partial, denied,
  cancelled, failed, unavailable, and reset-required states remain
  distinguishable from complete empty results.
- Default release artifacts include the supported semantic runtime and pass
  real package, install, startup, and host-integration journeys.

## Current blockers

- Doctor reports unavailable authority audit data and Cursor Core has an
  unresolved component-ownership conflict.
  Plans 09 and 27 own the corresponding product repairs.
- Semantic search is unavailable because the active configuration snapshot is
  invalid. Plan 20 owns snapshot repair and Plan 31 owns semantic activation;
  exact, lexical, and graph retrieval must remain available.
- Incremental indexing has shown unacceptable refresh staleness. Plan 25 owns
  cadence and freshness while preserving serve-during-refresh behavior.
- The repository still has unresolved test and CI failures. Focused local
  success does not establish product acceptance; normal repository CI must
  execute the affected journeys non-vacuously.
- PR14 remains open on the direct Plan 11 renderer fallback, real-browser,
  assistive-technology, and usability journeys. The performance,
  sustained-update, and payload budgets were withdrawn by owner decision
  2026-07-31 and no longer block acceptance.
- Remaining implementation work includes removing direct database/tool
  bypasses from MCP/dashboard/host code, deleting branch-fact archive/cutover
  mechanics, replacing legacy hookEvent paths with hook-runtime calls, wiring
  Work/Workflow/Feedback parity, completing the Grafeo authority cutover, and
  recontracting stale tests/docs against the final V2 decisions.

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
