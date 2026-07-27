# PR8–PR14 plan-status correction ledger

**Status (2026-07-27): authoritative companion to the numbered plans.**

This file records plan-status adjudications and retractions so later audits do
not turn abandoned design, later-PR ownership, or unexecuted contract seams
into current work. It is not a replacement for [NEXT.md](NEXT.md), which
remains the active PR12/PR13 execution slice, and it is not a code-gap queue.
The owning numbered plan remains authoritative for retained behavior.

The earlier ledger on `codex/v2-gap-ledger` at `e780fd660` was superseded
because it carried findings later retracted or fixed, including the Sigma
single-mount claim and the dashboard Doctor `target` mismatch. Integration
later merged that branch at `5e868bd8a`. Reconciliation confirmed that conflict
resolution retained this authoritative file, its complete retraction register,
and one companion link from `NEXT.md`; no second ledger or withdrawn/refuted
finding survived. Valid numbered-plan edits remain re-derived and selective,
not inherited merely because the superseded branch entered history.

## Delivery-band authority

- PR8 is complete.
- PR9, PR10, PR12, and PR13 remain active.
- PR14 has a substantial implemented and focused-suite-verified checkpoint but
  remains blocked on its named Plan 11 gaps and until the active production
  contracts, direct tests, and normal CI are stable.
- The delivery band is not green: `cargo dogfood` still exits nonzero, semantic
  search is disabled by an invalid configuration snapshot, incremental-index
  cadence is suspect after a 237-minute stale observation, roughly eight known
  test failures remain, and roughly 4,169 tests have never been measured in a
  completed full-suite run.
- No CI has run since 01:24 UTC on 2026-07-27 because PR #421 has been
  conflicting since 05:13 UTC. Roughly 60 commits, including every repair
  recorded below, are locally verified only. See the verification-status section
  below before treating any of them as proven.
- Plan 32 is PR17-only and SCOPE-OUT for PR8–PR14 audits.
- Plan 34 is split: the published read-only rename preview is implemented and
  reachable; apply-grade rename is not certified by that preview, and the
  unimplemented API-migration planner/apply journey is a PR11–PR12 band
  deliverable that PR19 consumes. Only PR19's temporary-alias deletion slices
  are SCOPE-OUT.

### Stabilization slices closed 2026-07-27

The following shipped behavior must not be reopened from older "missing" or
"unverified" text:

- cooperative daemon shutdown reaches startup transcript/provider ingest and
  code-index reconcile work, with cancelled startup finalization suppressed;
- configuration startup forward-repairs older snapshots with newly registered
  defaults, while production exact-project mutations republish the pinned
  snapshot and record runtime activation;
- dogfood health distinguishes terminal corruption/authority failure from
  retryable convergence, scopes convergence to the active project store, and
  source-stamp-caches the dashboard stage while reporting stage timings;
- code search serves the prior complete immutable generation while refresh owns
  the scheduler, and bundled-SQLite FTS blob corruption self-heals on open;
- Memory V2 owner archives cover all 33 authoritative families with adapter
  parity, referential closure, digest-bound cutover receipts, idempotent import,
  and public-read regressions; and
- the daemon-hosted dashboard retains the production invocation executor and
  directly commits Settings through the daemon control plane.

These closures do not erase the open operational evidence above. Plan 09 owns
the Doctor `authority_audit_unavailable` blocker; Plan 27 owns the Cursor Core
component-ownership conflict; Plans 20/31 own semantic snapshot/activation
recovery; Plan 25 owns incremental freshness cadence; and the active PR12/PR13
slice owns the incomplete full-suite evidence.

## Closed adjudications

### Abandoned frontend contract names

`ProjectionView` and `ProjectionManifest` have no Rust or TypeScript
implementation and are abandoned design names. They are not missing PR14
work. Renderer-neutral behavior remains required through generated workspace
payloads and adapter inputs.

`EvidencePacket` is different: it is a live, publicly exported Rust
application type with production consumers. Only its fictional frontend
contract role is abandoned. No audit may use this correction to remove or
deprecate the Rust type. The frontend's `EvidenceTruthStrip` is separately
live.

### External-source contract seam

Plans 01, 02, and 03 now split their completion status. The eight-provider
session-observation path is production-reached through one shared
admission/sanitizer/store composition. The retained external-source stack is
also production-composed for the host-observation specialization: MCP Hook V2
reaches host admission, `SourceCaptureApplicationV1`,
`apply_source_commit`, `ExternalSourceExecutor`, and
`external_source_states_v1`. The earlier “no production callers” adjudication
was wrong for this path. Broader provider acquisition, scheduled refresh, and
canonical refetch remain dormant; that narrower seam is neither to be rebuilt
nor deleted in the PR8–PR14 band.

### Dashboard reachability

All twelve PR14 workspaces are routed and reachable, and `build.rs` implements
the single-app source-stamp and embedded-asset contract. A prior zero
"implemented and reachable" score was a scoring-label artifact. Specific
sub-surfaces may remain unverified or backend-only; the dashboard and embed
path as a whole are not absent.

### Dashboard suite execution (closed 2026-07-27)

`dashboard_api_test` completes successfully — 58 run, 58 passed, on two
consecutive `--all-features` runs — so no audit may re-derive a gap from "the
suite has not completed". The two verification qualifications recorded in
`00-plan-set-index.md` on 2026-07-26 are closed rather than restated: Loom's
backend test is declared and executes, and `InjectedConfigurationClient` no
longer exists. The dashboard takes a host-supplied
`Arc<dyn DaemonInvocationExecutor>`, so the production Settings write is proven
by `dashboard_project_settings_commit_through_the_daemon_control_plane`
(`tests/pr11_pr12_runtime_acceptance.rs`) against a real daemon-hosted
dashboard, while the control-plane-less in-process fixture correctly withholds
the apply and answers a typed `configuration_authority_unavailable`.

This closes those two items only. Plan 11 carries the audited list of what PR14
acceptance still owes, including the absent performance, import-boundary,
virtualization, and viewport-matrix gates and the named per-workspace
capability gaps.

One refuted finding, recorded so it is not re-reported: an audit claimed
`/api/plugins/graph/strata` has no Code-workspace consumer. It does —
`dashboard/src/workspaces/code/Strata.tsx` reads the route through the
generated `StructureReadV12Schema`, and `CodePage.tsx` renders it. All five
Plan 11b structure routes are consumed; do not reopen that as a gap.

### Later-plan placement

Plan 32 belongs to PR17 and must not be filed as unmet PR14 work. Plan 34 is
listed in both PR11–PR12 and PR19: the callable API-migration planner/apply
journey belongs to the active PR11–PR12 band, while PR19 consumes it and owns
the temporary-alias deletion slices.

## Numbered-plan corrections

- `00-plan-set-index.md`: the former Loom declaration and injected-only
  Settings verification limits are closed; the roadmap now records the
  daemon-hosted production mutation proof and the still-open acceptance work.
- `01-domain-crate.md`, `02-store-crate.md`, `03-capture-crate.md`: completion
  includes both the shared session-observation path and the external-source
  host-observation specialization. Broader acquisition/refetch remains a
  retained, unmounted seam.
- `08-tool-catalog-crate.md`: host discovery no longer advertises
  handle-gated feedback operations or unsupported symbol-search `AsOf`;
  internal handlers are not mistaken for host-constructible requests.
- `09-application-crate.md`: the legacy root `RequestContext` has no scope;
  the crate model carries `ResolvedScope`. Temporal source access now separates
  exact-root authorization from six expressible lifecycle states, and Doctor's
  default registry truthfully exposes nine owning operations rather than a
  tenth handle-gated feedback read.
- `11-dashboard-frontend.md`: twelve-workspace/embed reachability is recorded;
  abandoned projection names are removed; live Rust `EvidencePacket` is
  protected. Generated graph contracts/consumers, generated Explorer query
  schemas, and typed scheduler pause/resume controls are now recorded.
- `11a-dashboard-design.md`: the visual audit now exits nonzero on render
  failure, uncaught page errors, axe violations, or pixel drift.
- `11b-structure-visualization.md`: all five endpoints are registered and now
  consumed through typed Code-workspace surfaces; the former backend-only gap
  is closed, without converting implementation into acceptance.
- `12-root-compatibility-migration.md`: Memory V2 cutover coverage is receipted,
  all 33 authoritative owner-archive families have physical adapter and
  referential-closure coverage, migrated-fact reclamation is production-reached,
  and the permanent V1-shaped compatibility projection is distinguished from a
  superseded writer. Direct init/open/read-only/branch lifecycle entry points
  now own production maintenance authority. A schema-disposition label is
  explicitly not migration evidence because the old gate accepted `"merged"`
  without proving merge SQL.
- `13-research-provenance-and-context-anchors.md`: core anchors are delivered;
  dedicated GitHub-stack targets remain a named PR13 follow-up.
- `14-historical-failure-regression-matrix.md`: already states that it owns
  regression classes, not a numbered failure ledger or fixture inventory.
- `16-cross-project-repository-worktree-scope.md`: linked worktrees collapse to
  primary-checkout project/store identity while retaining exact worktree
  snapshot authority; durable profiles refuse ephemeral roots and registry
  reaping retains rows backed by nonempty stores.
- `18-secret-detection-redaction-and-private-data-safety.md`: structural
  sanitization remains delivered; LCM raw sensitive-value redaction is
  conditionally delivered through `lcm_sensitive_redaction_enabled`, defaults
  off for losslessness, is irreversible, and does not rewrite payloads already
  at rest.
- `20-configuration-control-plane.md`: the former unconditional
  production-write refusal is closed. The production client issues a
  short-lived exact-project grant, commits through the control plane,
  republishes the pinned snapshot, and records runtime activation; the
  daemon-hosted dashboard path has direct commit/CAS coverage. PR17's complete
  work-execution snapshot and broader component activation/drift journey remain
  open, and the live semantic snapshot is currently invalid.
- `23-session-lcm-temporal-retrieval-and-evaluation.md`: retrieval-time expiry
  and `RetentionWithheld` remain here; retention writers and
  `source_cursor_advances` reclamation belong to Plan 38.
- `25-code-intelligence-indexing-crate.md`: indexing remains a root module and
  crate extraction is optional and evidence-driven. Foreground search now
  serves the previous complete generation during an in-flight refresh; cadence
  remains open after the live stale-index incident.
- `26-observability-accounting-and-usage.md`: the focused backend/frontend
  suite blocker is closed for Observatory and Costs. Their canonical model
  keeps failed/timed-out outcomes distinct from known zero and withholds values
  under unavailable/partial coverage; named presentation, performance, and
  end-to-end acceptance gaps remain owned by Plan 11/PR14.
- `27-cross-host-agent-plugin-bundles.md`: four declared safety/evidence guards
  are not production-reached, and Cursor Cloud has no default component set.
- `31-native-fastembed-semantic-code-search.md`: daemon-owned immutable
  `hf-hub` background acquisition shipped in `dd4adbe2a`, including verified
  online acquisition and packaged offline-cache acceptance. Manual local/HTTPS
  import APIs remain production-unwired. Semantic results remain omitted on the
  live profile because its configuration snapshot is invalid; Plan 20 owns
  snapshot repair and this plan owns successful semantic activation.
- `32-dynamic-workflow-runtime-and-sdk.md`: explicitly PR17-only.
- `34-workspace-refactoring-and-api-migration.md`: rename preview is live;
  API migration is an unimplemented PR11–PR12 deliverable consumed by PR19,
  while only temporary-alias deletion is later PR19 scope.
- `35-daemon-lsp-gateway-and-universal-diagnostics.md`: the shipped design is
  the daemon gateway, `src/lsp_bridge.rs`, `src/diagnostics/lsp/`, and
  `tracedecay lsp servers|bridge`; the old `--no-lsp`/environment/config/module
  proposal is not a missing plan requirement.
- `38-storage-retention-size-and-efficiency.md` and `NEXT.md`: raw LCM
  offload/drop, projected-message dedupe, legacy session/raw pruning, and
  observation-evidence release now have bounded defaults. Superseded
  `source_cursor_advances` are reclaimed by the daemon-authorized retention
  transaction while preserving the current-frontier receipt and restoring the
  immutable delete trigger before commit. No historical per-table byte claim
  has been reproduced through the product. `SqliteStoreSizeTelemetryPort` at
  `crates/tracedecay-rusqlite-runtime/src/telemetry/store_size.rs` implements
  scoped store-size and `dbstat` table-growth reads, but the daemon Doctor
  kernel emits per-table samples only as tracing. The dashboard exposes
  per-store size/free ratio and whole-store history; no Doctor finding,
  dashboard payload, or CLI Doctor output exposes per-table samples. Plan 38
  also records that the reachable debris collector originally missed bare
  `.corrupt` artifacts
  until `985cc5d4b`, that live branch stores are full graph copies rather than
  lightweight deltas, and that code-index generation publication has no
  retention pass. The storage-budget dashboard checkpoint is no longer blocked
  on `dashboard_api_test`; that suite now completes, without converting the
  broader Plan 38 requirements into acceptance.

No numbered plan claims `.tracedecay/domain-symbols.toml` as a delivered
capability, so the no-op `DOMAIN-EXTRACTORS.md` proposal requires no plan-side
correction.

## Verification status of the 2026-07-27 night commits

Everything recorded in the three sections below landed on
`codex/tracedecay-total-redesign-plan` during the night of 2026-07-27. **None of
the 24 commits has been validated by CI.** PR #421 has been in a conflicting
state since 05:13 UTC, and `pull_request` workflows cannot build a merge ref for
a conflicting pull request, so no CI run has started since 01:24 UTC — a gap now
covering roughly 60 commits. `gh pr view 421` still reports
`mergeable: CONFLICTING`.

Local verification during that window was scoped and contended. Several lanes
committed on `cargo check` plus a filtered test run rather than a completed
suite. Where a fix below is described as fixed, that means the repair landed and
its author observed a scoped local run — not that the repository's aggregate
gates agree. No claim here upgrades the delivery band, which remains not green
per the authority section above.

## Verification-coverage corrections (2026-07-27)

Commit `9e3ca9fd2` (2026-07-24, 617 files changed) deleted 124 first-party
tests. 75 of those covered behavior that still ships. Three of them intersect
claims this ledger or its numbered plans record as **delivered**.

These are **verification-coverage corrections, not retractions**. Nothing here
withdraws a delivered claim, and no audit may cite this section as evidence that
the underlying behavior is broken or must be reimplemented. The behavior may
well be correct. What is missing is the assertion that it is. A later audit's
only licensed action is to restore or replace the coverage — never to reopen the
capability as unbuilt.

- **Plan 16 — worktree/store identity.** The corrections list states that linked
  worktrees "collapse to primary-checkout project/store identity while retaining
  exact worktree snapshot authority". The mechanism enforcing the immutable half
  of that is the `store_instances_project_immutable_v1` trigger, and its test
  `store_project_identity_cannot_be_reparented` was among the deletions. Between
  2026-07-24 and 2026-07-27 nothing asserted that the trigger fires. `b8ef48ec5`
  restored it, together with the cross-table identity-constraint and
  sanitization-receipt-immutability tests. Three restored tests against 67
  declared guards is a partial restoration, not closure.
- **Plan 38 — `source_cursor_advances` reclamation.** The corrections list
  states that superseded advances are reclaimed "while preserving the current-
  frontier receipt and restoring the immutable delete trigger before commit".
  The adjacent recovery logic in
  `src/global_db/schema_contract/invariants/repair.rs` — 391 lines that run on
  every reopen and can rewind cursors and requeue projection suffixes — lost all
  nine of its tests in the same commit and currently has zero. The retention
  transaction's own coverage is a separate question from this one; what is
  unasserted is the repair path that can move the same cursors outside it.
- **Plans 18 and 23 — end-to-end sanitization.** Plan 18 lists structural
  sanitization as delivered and Plan 23 requires every imported row to carry a
  verified sanitization receipt. `tests/session_suite/temporal_privacy.rs` (1,081
  lines) was deleted outright, taking the three end-to-end tests that a redacted
  value cannot resurface through temporal summary, context, reopen, or rebuild
  replay. Unit sanitization coverage in `src/privacy/tests.rs` survives, so the
  detector and sanitizer are still asserted; the end-to-end non-resurfacing
  property is not. The fixtures those tests consumed are still on disk, so
  restoration does not require regenerating inputs.

### Structural residue of the same deletion

Three files carrying schema-enforcement and registry authority were left with no
direct test coverage at all, and two of them still are:

- `src/global_db/schema_contract/invariants/triggers.rs` — 67 `RAISE(ABORT)`
  guards across roughly 1,800 lines of trigger definitions, of which three are
  asserted by the module `b8ef48ec5` restored and 64 are not;
- `src/global_db/schema_contract/invariants/repair.rs` — 391 lines, zero tests;
- `src/global_db/project_registry.rs` — 1,691 lines, zero tests.

The cause was mechanical rather than a judgement that the coverage was
worthless: a `GlobalDb` → `RegisteredGlobalDb` refactor whose compile breaks were
cleared by deleting callers. The evidence is that 20 tests in the same file were
successfully re-pointed at the new type while 63 were deleted. A later audit
should treat this as a restoration backlog against known-good prior assertions,
not as new test design.

## Gates that attest to what they never checked

Six independent lanes each found the same failure family on 2026-07-27: a gate
that reports success without having exercised the thing it names. Recording the
family so future gate review looks for it directly.

1. `windows-pr8-temporal-durable` filtered on
   `binary(=session_suite) & test(/^lcm_schema::/)`, which matched zero tests
   while the job reported green. 30 LCM schema tests had never run under the
   Windows DELETE+FULL pragmas the job exists to cover. Fixed in `7a92b147a`.
2. `platform_lifecycle.passed` receipts were written unconditionally on two
   operating systems, under a comment claiming they proved a test had run. Fixed
   in `2758e5b97`.
3. `pr13_lite_grammar_contract` was satisfiable by all-features junit evidence
   without the lite build ever running. Fixed in `6b0417935`.
4. MCP fixtures wired test doubles into test doubles, and `production_joins.rs`
   has no production implementor — the seam is proven only against itself.
5. `pr13_advisory_proximity_overlap` filtered on a test deleted a month earlier,
   which reddened the `test` job at HEAD. Fixed in `3ec8f086b`.
6. `tests/pr12_production_reachability.rs` asserts that a symbol *name appears
   in source text* rather than that the path executes. That is why symbol-graph
   pagination was dead from the day it was written without any gate noticing.

The asymmetry that hides this family: **libtest exits 0 when a name filter
matches nothing, while nextest can be made to fail on an empty filter.** A
dangling `cargo test --exact` is therefore silently vacuous forever, and stays
green through the deletion of the very test it names. Treat a name-filtered gate
as unverified until something proves the filter selects a nonempty set.

## Product defects found and fixed 2026-07-27

Each landed with scoped local verification only; see the CI-status section
above.

- Symbol-graph pagination failed any read exceeding one page, from the day it
  was written — `e864c9bf1` binds the cursors to a derived digest.
- macOS and Windows could not mount a store at all, because the
  filesystem-locality classifier was Linux-only — `2edeee16c`, cross-checked on
  four target triples.
- `test_map` reported well-tested functions as untested when a graph read
  failed, presenting a failure as an empty success — `e56c1cc6c`.
- SSE renders were never coalesced, producing roughly 1,000 renders/s against a
  stated ceiling of ten, and one queue overflow fired eleven invalidations —
  `e402d6cfe`.
- Route grant identity was not request-correlated, and `storage_status` wrote a
  history row on every read — `5d2a6d4b9`.
- LSP refused to project context for any freshly bootstrapped project —
  `f3135e9f7` projects degraded feedback instead.

## Refuted defect claims — do not reintroduce

- Plan 35's current capability advertisement is honest; it is not a missing
  capability-advertisement defect.
- Semantic indexing cannot block exact, lexical, or graph retrieval; its
  asynchronous degraded behavior is working product behavior.
- The code-index freshness endpoint's typed `unsupported` result is correct
  behavior for an unsupported authority, not a defect to erase.

## Retraction register — do not re-report

1. A "dead CI gate" result based on a normal `rg` search is invalid until
   hidden workflow directories are searched.
2. `check-distribution-acceptance.sh` runs from release workflows; its scope
   may be defective, but the gate is not dead.
3. Cargo auto-discovers multi-file integration-test binaries. The bounded
   declaration defect is the missing `mod loom;`, not every suite lacking a
   `[[test]]` entry.
4. Apparent missing boundary-test paths inside synthetic `cargo metadata`
   JSON are fixture content, not repository references.
5. The observation fault harness is enabled in Linux CI; only its local
   default reachability is narrower.
6. The Doctor remediation dispatcher has production callers. The later
   frontend `target` mismatch cited by the superseded ledger is also fixed at
   current integration: preview/apply now carry `selected.target`.
7. Sigma is not instantiated at only one guarded site; the audit found three
   mount sites and the guard was mount-only.
8. Plan 04's ancestor-branch fallback is disclosed as stale serving, not a
   silent active-branch substitution.
9. Session/LCM retention code exists. The truthful issue is which windows and
   passes are active by default, not a missing engine.
10. `tracedecay_call_chain` is registered through the application surface;
    the stale dead-code handler is a duplicate.
11. Declaring `CodeIndexGenerationPublished` does not make publication
    observable; its only construction is test-only.
12. Plan 37's `read_workflow_jobs` chain is production-reached despite a stale
    "staged" annotation.
13. `source_cursor_advances` retention is Plan 38 ownership, not a Plan 23
    implementation gap.
14. `BoundedSanitizedText` intentionally enforces size while sanitization proof
    binds at the snapshot receipt; changing that newtype is the wrong repair.
15. Storage multi-connection test modules are pulled in with `include!` and
    are not orphaned.
16. The release-integrity negative test using `HEAD HEAD` still exercises a
    diff-independent tracked-ignored rejection; it is not vacuous.
17. SQLite backup primitives are production-reached. Only the higher-level
    orchestration remains unreachable.
18. The two `Embedding*V1` representations do not corrupt `profile_digest`;
    they remain a mis-import/serde-divergence hazard only.
19. The two HTTP operation enums have 53 matching members. The apparent
    divergence was a count of a different owner-kind enum.
20. Plan 33's requirements are not-yet-due PR20 work, not thirteen active-band
    gaps.
21. No workspace crate is dead; all ten members are declared and imported,
    with the rusqlite parity crate deliberately outside the default binary
    graph.

## Out-of-scope correction requests

The source comments in `Cargo.toml` and
`src/global_db/observation/retention.rs` were not edited because this lane's
write boundary is `docs/plans/tracedecay-v2/`. The correction ledger's
numbering contains no D18 row; no inferred replacement was invented.
