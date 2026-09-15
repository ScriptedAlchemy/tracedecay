# #1280 operating envelope — spawn brief

Tip: origin/codex/tracedecay-total-redesign-plan-reopened @ ec35eb7f48 (post-#1314).
Path-isolated from #1269/#1324 (redundancy) and from family-read (`agent/1275-family-read`).

## Issue
**perf(clones): qualify the 5k-file and 100k-symbol operating envelope**

Decision: `docs/plans/tracedecay-v2/00-plan-set-index.md`, rejected decision 11, and `docs/V2-OPERATING-MODEL.md` (Grafeo graph-only). Campaign root: #1260.
Target branch: `codex/tracedecay-total-redesign-plan-reopened` (#707). Depends on: #1268–#1272.

## Owns
On a pinned ~5,000-file corpus record: eligible bytes, source tokens, body count, symbol count, language counts, changed-body count, cold/warm cache state, hardware and OS, elapsed time, peak RSS, disk bytes, postings read, candidates admitted, pairs verified.

Targets: clone indexing adds ≤ 1 GiB peak RSS; clone work adds ≤ 60 s on the declared corpus; one-symbol indexed lookup p95 ≤ 250 ms; one ordinary changed function refreshes within 1 s after extraction; unchanged functions are not renormalized; adversarial workloads stop, page, resume, or return partial coverage rather than growing without bound. Cancellation and restart are exercised.

The graph-publication RSS problem (#852) stays measured independently.

## Acceptance
A simple Linux developer benchmark per the roadmap acceptance rule — raw samples and practical deltas, no gate scaffold.

## Rulings applied (GPT-6 Pro, 2026-09-14)
- Start measurements with the **first usable slice** (exact lookup on full-rebuild generations) and keep them running through #1270–#1273; closure still requires full acceptance.
- **Validate the starting bounded policy** in #1271/#1272 (k/w, 30-token minimum, 0.70 ratio, 16,384 posting rows, 256 candidates, 64 verifications, 2M steps, 200 ms, absolute 1,024 hot-posting threshold) before claiming the performance envelope; those numbers are starting decisions, not measured safe maxima.
- **Corpus (cost and exhaustion):** a thousand-copy exact family without quadratic pair creation; long repetitive bodies; hot-only fingerprints; many plausible candidates; cancellation; every budget boundary with truthful partial results. **Real-world usefulness:** independently reviewed real-repository clone pairs and hard negatives, reported by supported language and declared overlap band.


## Constraints
- Measure-first via Hauler CLI only.
- Own worktree: `scripts/agent-worktree.sh /fast/tmp/td-1280-envelope -b agent/1280-envelope origin/codex/tracedecay-total-redesign-plan-reopened`
- Do not touch MCP redundancy handlers or similar-catalog mount (#1324).
- Do not touch family-read dashboard transport (#1275-family-read).
- No timeout bumps. Amended supersede ≥30m MOVING from CLI-built.
- PR against campaign tip; do not merge.

## Done
PR URL + Hauler receipts naming the measured corpus + typed envelope numbers.
