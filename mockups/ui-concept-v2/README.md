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
| 02 | 02-explorer/v2-four-lanes.png | Four fan-out lanes: code / sessions / knowledge / semantic. |
| 03 | 03-loom/v1-weave.png | Weave canvas. Time down, host across, width = messages. Causal crossings listed, not drawn. |
| 04 | 04-sessions/v1-inspector.png | Session list with real headers + LCM inspector. |
| 05 | 05-agents/v1-host-tree.png | Host tree, handoffs, tool activity, failure context. |
| 06 | 06-code/v1-cortex.png | Cortex → Trace → Core lens on a dark optical field. |
| 07 | 07-knowledge/v2-four-cameras.png | Facts / Geometry / Curation / Oplog. Trust as ticks, never faded text. |
| 08 | 08-delivery/v1-recency-field.png | Same recency field as Brain; bodies are branches and PRs. |
| 09 | 09-automations/v1-cron-strip.png | Schedule strip, run history, artifacts, approvals. |
| 10 | 10-observatory/v2-overview-stack.png | Overview stack: Doctor, observations, budgets, hooks, store telemetry. |
| 11 | 11-costs/v1-provider-burn.png | Provider/model spend, tokens, latency. Segmented bars, not pies. |
| 12 | 12-settings/v2-effective-values.png | Effective values only. Provenance named. No invented layer cake. |
| 13 | 13-work/v2-six-cameras.png | Six cameras: Board / DAG / Timeline / Causal / Workload / Topology. |
| 14 | 14-workflows/v1-lifecycle-tracks.png | Definitions, lifecycle, run projection. |
