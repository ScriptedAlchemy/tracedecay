/**
 * Observatory system-evidence overview — CONCEPT / PROFILE SNAPSHOT.
 *
 * Every displayed value is derived from the bundled profile pack
 * (src/data/pack-index.json, hook-census.json, branch-census.json) or is a
 * typed unavailable/empty state. The plate
 * (lookbook/pngs/10-observatory/final/01-system-evidence-overview.png) is an
 * interaction reference for layout only: the pack has no Doctor run, no
 * adoption metrics, zero retrieval anchors, no budget/topology/live-pipeline
 * series, and no orphan-segment census, so none of those pictured metrics are
 * reported here. The code index is never claimed sealed and SERVE stays
 * unavailable because the pack has no serve. Chart geometry (bar tracks,
 * spark rails, dot matrices, the topology silhouette) is always drawn — an
 * absent authority renders its instrument at zero/unlit, never as a bare
 * one-line void — but no absent value is ever painted as a lit reading.
 */
import { PACK, PROFILE } from "../data/pack";
import hookRaw from "./hook-census.json";
import branchRaw from "./branch-census.json";

export { PACK, PROFILE };

export type NamedCount = { name: string; count: number };
export type PrefixCount = { prefix: string; count: number };

export type HookCensus = {
  source: string;
  bytes: number;
  lines: number;
  parseFailures: number;
  coverage: string;
  events: NamedCount[];
  hookNames: NamedCount[];
  dispositionStatus: NamedCount[];
  reasonCodes: NamedCount[];
  agents: NamedCount[];
};

export type BranchCensus = {
  source: string;
  project: string;
  projectId: string;
  localRefCount: number;
  prefixCount: number;
  prefixes: PrefixCount[];
};

export const HOOKS = hookRaw as HookCensus;
export const BRANCHES = branchRaw as BranchCensus;

export const CAPTURE_SHORT = "01:08 PT";
export const CAPTURE_ISO = PACK.capturedAt;

/* ---- typed evidence states ---- */

export type EvidenceState = "measured" | "partial" | "empty" | "unsealed" | "host" | "exact" | "unavailable";

export const STATE_LABEL: Record<EvidenceState, string> = {
  measured: "MEASURED",
  partial: "PARTIAL",
  empty: "MEASURED EMPTY",
  unsealed: "UNSEALED",
  host: "HOST_MEASURED",
  exact: "MANIFESTS EXACT",
  unavailable: "UNAVAILABLE",
};

/* ---- doctor inspection (no run in pack; check names are the instrument) ---- */

export const DOCTOR_CHECKS = [
  "invariant violations",
  "type integrity",
  "surface drift",
  "dead code",
  "contract gaps",
  "naming collisions",
  "security smells",
  "performance smells",
];

/* ---- adoption coverage (surfaces axis only; series not copied) ---- */

export const ADOPTION_SURFACES = ["api", "cli", "sdk", "workers", "webhooks", "docs"];

/* ---- retrieval quality (anchors table measured empty; no metric series) ---- */

export const RETRIEVAL_METRICS = ["precision@10", "recall@10", "mrr@10"];

/* ---- code-index pipeline silhouette (no counts in pack) ---- */

export type PipeStage = { id: string; unit: string; state: "unsealed" | "unavailable" };

export const PIPE_STAGES: PipeStage[] = [
  { id: "ingest", unit: "docs", state: "unsealed" },
  { id: "parse", unit: "ast", state: "unsealed" },
  { id: "normalize", unit: "nodes", state: "unsealed" },
  { id: "embed", unit: "vectors", state: "unsealed" },
  { id: "link", unit: "edges", state: "unsealed" },
  { id: "index", unit: "ready", state: "unsealed" },
  { id: "serve", unit: "qps", state: "unavailable" },
];

/* ---- performance budgets (no budget or spend series in snapshot) ---- */

export const BUDGET_ROWS = ["p95 latency (ms)", "error rate (%)", "cpu (cores)", "index build (h)", "queue depth"];

/* ---- live pipeline (capture is a still; no NOW series) ---- */

export const LIVE_STATS = ["throughput", "lag", "backlog", "eta"];

/* ---- storage telemetry (manifest identity only; no bytes copied) ---- */

export const STORE_MANIFESTS = PACK.projects.map((p) => ({
  name: p.name,
  kind: "code_project",
  mode: "profile_sharded",
  graph: "tracedecay.db",
}));

export const TELEMETRY_ROWS = ["hot tier", "warm tier", "cold tier", "object count"];

/* ---- storage / branch findings (host-measured census + typed absences) ---- */

export type FindingRow = {
  id: string;
  state: EvidenceState;
  label: string;
  value: string;
  selected?: boolean;
};

export const FINDING_ROWS: FindingRow[] = [
  { id: "census", state: "measured", label: "stale branch census", value: String(branchRaw.localRefCount), selected: true },
  { id: "prefixes", state: "measured", label: "branch prefix spread", value: String(branchRaw.prefixCount) },
  { id: "orphans", state: "unavailable", label: "orphaned segment references", value: "—" },
  { id: "prune", state: "unavailable", label: "prune plan / recovery eta", value: "—" },
];

export const ANCHOR_TOTAL = PACK.projects.reduce((n, p) => n + (p.retrievalAnchors ?? 0), 0);
export const HEADS_KNOWN = PACK.projects.filter((p) => p.graphVerifiedHeads != null).length;
export const FACTS_ABSENT = PACK.projects.filter((p) => p.factsTable === "absent" || p.factsTable == null).length;

export const TIMELINE_TICKS: { pos: number; label: string }[] = [
  { pos: 0, label: "-12h" },
  { pos: 25, label: "-6h" },
  { pos: 50, label: "-3h" },
  { pos: 75, label: "-1h" },
  { pos: 100, label: "CAPTURE" },
];

export function fmt(n: number) {
  return n.toLocaleString("en-US");
}
