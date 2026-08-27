# TraceDecay UI concept V2

Jayse Hansen / Cantina Avengers FUI grammar applied to the fourteen dashboard channels. Steal the grammar (night glass, hairline frames, amber alert, cyan signal, measured fields). Do not copy Marvel marks.

These are lookbook stills, not visual-audit goldens. Do not treat them as dashboard/audit-baselines/.

## Layout

One folder per channel, versions stacked inside:

```
mockups/ui-concept-v2/
  <NN>-<channel>/
    v<N>-<short-slug>.png
```

- Add new iterations as `vN-short-slug.png` inside the channel folder (for example the next Brain synapse/hook-firing plate is `01-brain/v2-hook-synapses.png`).
- Highest `vN` in a folder is the current plate.
- 2026-08-27 correction: Brain current is `v4-measured-registry.png` (prior versions kept).
- Do not flatten plates into the root. Do not dump iterations into a `drafts/` folder.
- Never overwrite an older version; add the next `vN`.
- No git symlinks.

## Canonical plates

Current (highest `vN`) plate per channel:

| # | File | Hero |
|---|---|---|
| 01 | 01-brain/v4-measured-registry.png | Unscoped measured registry field. X=recency bucket. Y/size=indexed mass. Hub only when checkouts share git_common_dir. One-hop pulse. CONCEPT/SYNTHETIC. |
| 02 | 02-explorer/v4-lane-lifecycle.png | Four lanes, independent source states. Create/poll/cancel. Semantic absent/indexing/unavailable, never all LIVE. CONCEPT/SYNTHETIC. |
| 03 | 03-loom/v3-measured-weave.png | Time down, hosts across, thickness = messages. Causal crossings listed, not drawn. Playback over loaded LCM page. CONCEPT/SYNTHETIC. |
| 04 | 04-sessions/v3-provenance-inspector.png | Provider-qualified sessions, token provenance, paged inspector, coverage/redaction. exists:false vs empty vs transport vs unavailable. CONCEPT/SYNTHETIC. |
| 05 | 05-agents/v3-authority-tree.png | Independent authorities. Usage, tree, handoff frontier, tokens, failures. No PID/CPU theatre. CONCEPT/SYNTHETIC. |
| 06 | 06-code/v3-lenses.png | CORTEX/TRACE/CORE with direct labels. Graph totals, symbol path, strata, freshness, diagnostic warming/stale/unavailable. CONCEPT/SYNTHETIC. |
| 07 | 07-knowledge/v4-four-cameras.png | Facts / Geometry / Curation / Oplog. Trust as ticks. PCA only if method=pca; otherwise unserved. Independent camera states. CONCEPT/SYNTHETIC. |
| 08 | 08-delivery/v3-independent-authorities.png | Independent local Git vs provider. Labeled recency field. Denied/rate-limited/stale/unavailable/not-published never green. CONCEPT/SYNTHETIC. |
| 09 | 09-automations/v3-scheduler-ledger.png | Scheduler ledger. Pause/resume, due/skip, jobs, skills, receipts, run ledger, artifacts, integrity. No Approvals. CONCEPT/SYNTHETIC. |
| 10 | 10-observatory/v2-overview-stack.png | Overview stack: Doctor, observations, budgets, hooks, store telemetry. |
| 11 | 11-costs/v1-provider-burn.png | Provider/model spend, tokens, latency. Segmented bars, not pies. |
| 12 | 12-settings/v2-effective-values.png | Effective values only. Provenance named. No invented layer cake. |
| 13 | 13-work/v2-six-cameras.png | Six cameras: Board / DAG / Timeline / Causal / Workload / Topology. |
| 14 | 14-workflows/v3-definition-ledger.png | Definition ledger. Immutable versions, pinned digests, CAS activate/retire/reject, on-demand run lookup. No ARM/PAUSE/CANCEL. CONCEPT/SYNTHETIC. |
