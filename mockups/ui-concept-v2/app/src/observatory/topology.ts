import type { EvidenceState } from "./data";

export type ObservatoryTopologyRole = "source" | "derived" | "affected" | "independent";
export type ObservatoryTopologyNode = {
  id: string;
  well: string;
  label: string;
  state: EvidenceState;
  role: ObservatoryTopologyRole;
  x: number;
  y: number;
  source: string;
  summary: string;
  target: { surface: string; params: Record<string, string> };
};

export type ObservatoryTopologyEdge = {
  id: string;
  from: string;
  to: string;
  kind: "data" | "comparison";
  summary: string;
};

/**
 * Stable locations are semantic: source authorities on the left, derived
 * observations in the middle, and their affected consumers on the right.
 * Unconnected nodes remain independent; a visual adjacency is never a claim.
 */
export const OBSERVATORY_TOPOLOGY: {
  nodes: ObservatoryTopologyNode[];
  edges: ObservatoryTopologyEdge[];
} = {
  nodes: [
    {
      id: "runtime",
      well: "budgets",
      label: "runtime / provider",
      state: "unavailable",
      role: "source",
      x: 44,
      y: 34,
      source: "runtime/provider observation",
      summary: "No runtime or provider observation was copied into the profile pack.",
      target: { surface: "settings", params: { observatory_finding: "budgets", topology: "runtime" } },
    },
    {
      id: "storage",
      well: "storage",
      label: "store manifests",
      state: "exact",
      role: "source",
      x: 44,
      y: 117,
      source: "store_manifest.json",
      summary: "Store identity is exact; capacity telemetry was not copied.",
      target: { surface: "settings", params: { observatory_finding: "storage", topology: "storage" } },
    },
    {
      id: "index",
      well: "pipeline",
      label: "code index",
      state: "unsealed",
      role: "derived",
      x: 154,
      y: 76,
      source: "tracedecay.db / profile-pack",
      summary: "Verified heads exist, but no seal receipt or serve observation is available.",
      target: { surface: "code", params: { observatory_finding: "pipeline", topology: "index" } },
    },
    {
      id: "retrieval",
      well: "retrieval",
      label: "retrieval anchors",
      state: "empty",
      role: "affected",
      x: 284,
      y: 76,
      source: "retrieval_anchors",
      summary: "The captured table is measured empty; quality metrics remain unavailable.",
      target: { surface: "knowledge", params: { observatory_finding: "retrieval", topology: "retrieval" } },
    },
    {
      id: "budgets",
      well: "budgets",
      label: "budget comparison",
      state: "unavailable",
      role: "affected",
      x: 284,
      y: 34,
      source: "named runtime/provider budget",
      summary: "The comparison is unserved because its required observations are unavailable.",
      target: { surface: "costs", params: { observatory_finding: "budgets", topology: "budgets" } },
    },
    {
      id: "hooks",
      well: "hooks",
      label: "hook census",
      state: "host",
      role: "independent",
      x: 146,
      y: 136,
      source: "hook_analytics.jsonl",
      summary: "Host-measured events are not a Doctor verdict or adoption percentage.",
      target: { surface: "automations", params: { observatory_finding: "hooks", topology: "hooks" } },
    },
    {
      id: "branches",
      well: "findings",
      label: "branch census",
      state: "measured",
      role: "independent",
      x: 260,
      y: 136,
      source: "core local refs",
      summary: "Local-reference counts are observed independently of storage recovery or provider state.",
      target: { surface: "delivery", params: { observatory_finding: "findings", topology: "branches" } },
    },
  ],
  edges: [
    {
      id: "runtime-budgets",
      from: "runtime",
      to: "budgets",
      kind: "comparison",
      summary: "Named budget comparisons require runtime/provider observations.",
    },
    {
      id: "index-retrieval",
      from: "index",
      to: "retrieval",
      kind: "data",
      summary: "Indexed artifacts populate retrieval anchors; an unsealed index does not turn an empty anchor table into a failure.",
    },
  ],
};
