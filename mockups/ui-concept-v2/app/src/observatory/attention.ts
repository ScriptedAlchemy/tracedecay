import type { AttentionItem } from "../app/workspace";
import { PACK } from "../data/pack";

/**
 * Snapshot entries only describe records copied into the profile pack.
 * Fixture entries are authored examples, never a claim about the snapshot.
 */
export const OBSERVATORY_ATTENTION: AttentionItem[] = [
  {
    id: "observatory:index-unsealed",
    title: "Code index has no seal receipt",
    detail: "Verified heads were captured, but the index is unsealed and the retrieval-anchor table is empty.",
    source: "coverage",
    severity: "warning",
    status: "active",
    owner: "system",
    evidence: "exact",
    mode: "snapshot",
    observedAt: PACK.capturedAt,
    sourceRef: "src/data/pack-index.json · code-index note",
    target: { surface: "observatory", params: { observatory_finding: "pipeline", topology: "index" } },
  },
  {
    id: "observatory:branch-census",
    title: "Branch census is a captured observation",
    detail: "The local-ref count is present, but the pack contains no newer branch observation or prune-plan receipt.",
    source: "coverage",
    severity: "information",
    status: "active",
    owner: "system",
    evidence: "exact",
    mode: "snapshot",
    observedAt: PACK.capturedAt,
    sourceRef: "observatory/branch-census.json",
    repository: "tracedecay",
    target: { surface: "observatory", params: { observatory_finding: "findings", topology: "branches" } },
  },
  {
    id: "observatory:fixture-provider-route",
    title: "AUTHORED EXAMPLE · provider route unavailable",
    detail: "A missing provider/runtime observation prevents a budget comparison; this is fixture evidence only.",
    source: "workflow",
    severity: "warning",
    status: "active",
    owner: "external",
    evidence: "explicit",
    mode: "fixture",
    observedAt: "2025-05-09 14:22:07 UTC",
    sourceRef: "AUTHORED EXAMPLE · runtime/provider observation",
    target: { surface: "observatory", params: { observatory_finding: "budgets", topology: "runtime" } },
  },
  {
    id: "observatory:fixture-hook-coverage",
    title: "AUTHORED EXAMPLE · hook coverage needs review",
    detail: "A partial host-hook census needs a source inspection before it is used as an adoption conclusion.",
    source: "coverage",
    severity: "information",
    status: "active",
    owner: "you",
    evidence: "explicit",
    mode: "fixture",
    observedAt: "2025-05-09 14:22:07 UTC",
    sourceRef: "AUTHORED EXAMPLE · hook coverage review",
    target: { surface: "observatory", params: { observatory_finding: "hooks", topology: "hooks" } },
  },
];
