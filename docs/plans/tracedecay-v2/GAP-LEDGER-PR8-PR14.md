# PR8–PR14 plan-status correction ledger

**Status (2026-07-26): authoritative companion to the numbered plans.**

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
- PR14 remains blocked until the active production contracts, direct tests,
  and normal CI are stable.
- Plan 32 is PR17-only and SCOPE-OUT for PR8–PR14 audits.
- Plan 34 is split: the published read-only rename preview is implemented and
  reachable; apply-grade rename is not certified by that preview, and the
  unimplemented API-migration planner/apply journey is a PR11–PR12 band
  deliverable that PR19 consumes. Only PR19's temporary-alias deletion slices
  are SCOPE-OUT.

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
admission/sanitizer/store composition. The external-source domain,
application, and store contracts are not production-composed: their capture,
authorization, commit, adapter, and migration path is absent. This retained
seam is neither to be implemented nor deleted in the PR8–PR14 band.

### Dashboard reachability

All twelve PR14 workspaces are routed and reachable, and `build.rs` implements
the single-app source-stamp and embedded-asset contract. A prior zero
"implemented and reachable" score was a scoring-label artifact. Specific
sub-surfaces may remain unverified or backend-only; the dashboard and embed
path as a whole are not absent.

### Later-plan placement

Plan 32 belongs to PR17 and must not be filed as unmet PR14 work. Plan 34 is
listed in both PR11–PR12 and PR19: the callable API-migration planner/apply
journey belongs to the active PR11–PR12 band, while PR19 consumes it and owns
the temporary-alias deletion slices.

## Numbered-plan corrections

- `00-plan-set-index.md`: Loom's undeclared backend test and the injected-only
  Settings mutation proof are explicit verification limits.
- `01-domain-crate.md`, `02-store-crate.md`, `03-capture-crate.md`: completion
  is limited to the session-observation path; the external-source block is a
  retained, unmounted seam.
- `09-application-crate.md`: the legacy root `RequestContext` has no scope;
  the crate model carries `ResolvedScope`.
- `11-dashboard-frontend.md`: twelve-workspace/embed reachability is recorded;
  abandoned projection names are removed; live Rust `EvidencePacket` is
  protected.
- `11b-structure-visualization.md`: five backend endpoints are registered;
  their missing frontend consumers are the open work.
- `13-research-provenance-and-context-anchors.md`: core anchors are delivered;
  dedicated GitHub-stack targets remain a named PR13 follow-up.
- `14-historical-failure-regression-matrix.md`: already states that it owns
  regression classes, not a numbered failure ledger or fixture inventory.
- `18-secret-detection-redaction-and-private-data-safety.md`: structural
  sanitization remains delivered; LCM raw sensitive-value redaction is
  conditionally delivered through `lcm_sensitive_redaction_enabled`, defaults
  off for losslessness, is irreversible, and does not rewrite payloads already
  at rest.
- `20-configuration-control-plane.md`: non-MCP production writes fail
  authority admission and production activation rows are never created, so
  user-driven mutation and desired/observed drift are not certified.
- `23-session-lcm-temporal-retrieval-and-evaluation.md`: retrieval-time expiry
  and `RetentionWithheld` remain here; retention writers and
  `source_cursor_advances` reclamation belong to Plan 38.
- `25-code-intelligence-indexing-crate.md`: already states that indexing is a
  root module and crate extraction is optional and evidence-driven.
- `26-observability-accounting-and-usage.md`: Observatory and Costs remain
  implemented but unverified.
- `27-cross-host-agent-plugin-bundles.md`: four declared safety/evidence guards
  are not production-reached, and Cursor Cloud has no default component set.
- `31-native-fastembed-semantic-code-search.md`: daemon-owned immutable
  `hf-hub` background acquisition shipped in `dd4adbe2a`, including verified
  online acquisition and packaged offline-cache acceptance. Manual local/HTTPS
  import APIs remain production-unwired, and semantic results remain omitted
  until compatible indexing is ready.
- `32-dynamic-workflow-runtime-and-sdk.md`: explicitly PR17-only.
- `34-workspace-refactoring-and-api-migration.md`: rename preview is live;
  API migration is an unimplemented PR11–PR12 deliverable consumed by PR19,
  while only temporary-alias deletion is later PR19 scope.
- `35-daemon-lsp-gateway-and-universal-diagnostics.md`: the shipped design is
  the daemon gateway, `src/lsp_bridge.rs`, `src/diagnostics/lsp/`, and
  `tracedecay lsp servers|bridge`; the old `--no-lsp`/environment/config/module
  proposal is not a missing plan requirement.
- `38-storage-retention-size-and-efficiency.md` and `NEXT.md`: projected
  message dedupe is engaged by default, but raw offload/drop, legacy
  session/raw windows, observation-evidence release, and
  `source_cursor_advances` reclamation are not. Its immutable triggers are a
  real obstacle, but the whole `global.db` is only 0.98 GiB and no
  `StoreSizeTelemetryPort` implementation supports a per-table byte claim.
  Plan 38 also records that the reachable debris collector originally missed
  bare `.corrupt` artifacts until `985cc5d4b`, that live branch stores are full
  graph copies rather than lightweight deltas, and that code-index generation
  publication has no retention pass.

No numbered plan claims `.tracedecay/domain-symbols.toml` as a delivered
capability, so the no-op `DOMAIN-EXTRACTORS.md` proposal requires no plan-side
correction.

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
