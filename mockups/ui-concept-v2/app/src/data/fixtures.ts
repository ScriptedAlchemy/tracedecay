import CORE_BRANCHES from "./core-branches.json";

export type RecencyBucket =
  | "<5min"
  | "5min-1h"
  | "1h-1d"
  | "1d-1w"
  | "1w-1m"
  | "1m-6m"
  | "6m-1y"
  | ">1y";

export const RECENCY_AXIS: { id: RecencyBucket; label: string; sub: string }[] = [
  { id: "<5min", label: "< 5 MIN", sub: "very recent" },
  { id: "5min-1h", label: "5 MIN – 1 H", sub: "recent" },
  { id: "1h-1d", label: "1 H – 1 D", sub: "active" },
  { id: "1d-1w", label: "1 D – 1 W", sub: "aged" },
  { id: "1w-1m", label: "1 W – 1 M", sub: "seasoned" },
  { id: "1m-6m", label: "1 M – 6 M", sub: "long-lived" },
  { id: "6m-1y", label: "6 M – 1 Y", sub: "dormant" },
  { id: ">1y", label: "> 1 Y", sub: "deep dormant" },
];

export type Checkout = {
  alias: string;
  path: string;
  lastSeen: string;
};

export type ProjectBody = {
  id: string;
  name: string;
  storeCount: number;
  artifactCount: number;
  indexedMass: number;
  recency: RecencyBucket;
  age: string;
  color: string;
  colorDim: string;
  canonicalRoot?: string;
  defaultBranch?: string;
  checkouts: Checkout[];
  /** Local git refs; canopy fingerprint when sessions are empty. */
  branches?: string[];
  linkedHub?: boolean;
  row: 0 | 1 | 2;
};

export type EvidenceGrade = "EXACT" | "EXPLICIT" | "INFERRED" | "AMBIGUOUS" | "STALE" | "UNAVAILABLE";

/** Live slim snapshot (no message bodies) is at profile-pack/. These fixture names/counts are CONCEPT HUD, not production evidence. */
export const PROFILE_SOURCE = {
  path: "~/.tracedecay",
  brainId: "brain.396439351c3e4a543ddb195baa7546b0",
  profileId: "profile.81b4dfa5969a906669cff59ee12eb87e",
  capturedAt: "2026-08-30T07:57:00Z",
  daemon: "foreground-run",
};

export const PROJECTS: ProjectBody[] = [
  {
    id: "proj_ae394425f7837d4f",
    name: "tracedecay",
    storeCount: 4,
    artifactCount: 229,
    indexedMass: 887,
    recency: "<5min",
    age: "00:00:40",
    color: "#5ee7ff",
    colorDim: "#1a6f86",
    canonicalRoot: "/Volumes/bigssd/projects/tracedecay",
    defaultBranch: "master",
    checkouts: [
      { alias: "redesign", path: "/Volumes/bigssd/projects/tracedecay", lastSeen: "just now" },
      { alias: "ui-concept-first-party", path: "/Volumes/bigssd/projects/tracedecay/.worktrees/ui-concept-first-party", lastSeen: "—" },
      { alias: "ui-concept-v2-final-followup", path: "/Volumes/bigssd/projects/tracedecay/.worktrees/ui-concept-v2-final-followup", lastSeen: "—" },
    ],
    linkedHub: true,
    row: 0,
  },
  {
    id: "proj_2c558f8ce42d5cdf",
    name: "core",
    storeCount: 0,
    artifactCount: 0,
    indexedMass: CORE_BRANCHES.length,
    recency: "1m-6m",
    age: "7w",
    color: "#7dd3fc",
    colorDim: "#2a5a78",
    canonicalRoot: "/Volumes/bigssd/projects/core",
    defaultBranch: "main",
    checkouts: [
      { alias: "main", path: "/Volumes/bigssd/projects/core", lastSeen: "enrolled" },
    ],
    branches: CORE_BRANCHES,
    linkedHub: true,
    row: 0,
  },
  {
    id: "proj_e19f6f383c982ea8",
    name: "ZeroFS",
    storeCount: 26,
    artifactCount: 1662,
    indexedMass: 4374,
    recency: "<5min",
    age: "00:06:10",
    color: "#67e8f9",
    colorDim: "#1f5c68",
    canonicalRoot: "/Volumes/bigssd/projects/ZeroFS",
    defaultBranch: "feat/sftp-object-store",
    checkouts: [
      { alias: "sftp-object-store", path: "/Volumes/bigssd/projects/ZeroFS", lastSeen: "6m ago" },
    ],
    linkedHub: true,
    row: 0,
  },
  {
    id: "proj_8007f36f3654e9be",
    name: "zerofs-ios-companion",
    storeCount: 40,
    artifactCount: 5966,
    indexedMass: 12946,
    recency: "1h-1d",
    age: "02:32:00",
    color: "#f0b429",
    colorDim: "#7a5a14",
    canonicalRoot: "/Volumes/bigssd/projects/zerofs-ios-companion",
    defaultBranch: "main",
    checkouts: [
      { alias: "main", path: "/Volumes/bigssd/projects/zerofs-ios-companion", lastSeen: "2h ago" },
    ],
    linkedHub: true,
    row: 1,
  },
  {
    id: "proj_a812b7edf6fab331",
    name: "movie-library",
    storeCount: 0,
    artifactCount: 0,
    indexedMass: 1,
    recency: "1d-1w",
    age: "1d 23h",
    color: "#9be15d",
    colorDim: "#3d6a24",
    canonicalRoot: "/Users/zackjackson/plugins/movie-library",
    defaultBranch: "master",
    checkouts: [
      { alias: "master", path: "/Users/zackjackson/plugins/movie-library", lastSeen: "1d ago" },
    ],
    linkedHub: true,
    row: 1,
  },
  {
    id: "proj_2f28a204e57c569f",
    name: "mqvpn",
    storeCount: 1,
    artifactCount: 14,
    indexedMass: 30,
    recency: "1d-1w",
    age: "2d 12h",
    color: "#c084fc",
    colorDim: "#5b3d80",
    canonicalRoot: "/Volumes/bigssd/projects/mqvpn",
    defaultBranch: "main",
    checkouts: [
      { alias: "main", path: "/Volumes/bigssd/projects/mqvpn", lastSeen: "2d ago" },
    ],
    linkedHub: true,
    row: 2,
  },
];

export const HUB = {
  id: "repo:git_common_dir",
  label: "repo:git_common_dir",
  caption: "hub — massless",
};

export const SIGNAL_FAMILIES = [
  { name: "Code Changes", count: 12843, color: "#5ee7ff" },
  { name: "Ingestion Runs", count: 9217, color: "#67e8f9" },
  { name: "Analysis Runs", count: 4982, color: "#f0b429" },
  { name: "Exports", count: 1183, color: "#fbbf24" },
  { name: "Rule Evaluations", count: 7664, color: "#c084fc" },
  { name: "Memory Writes", count: 18562, color: "#e879f9" },
];

export const SYNAPSE_EVENT = {
  projectId: "proj_ae394425f7837d4f",
  family: "PostToolUse",
  streamId: "hook_analytics",
  at: "2026-08-29T06:13:28Z",
  hopEnergy: 0.33,
  siblingCheckout: 0,
  grade: "EXACT" as EvidenceGrade,
};

export const CHANNELS = [
  "Brain",
  "Explorer",
  "Loom",
  "Sessions",
  "Agents",
  "Code",
  "Knowledge",
  "Delivery",
  "Automations",
  "Observatory",
  "Costs",
  "Settings",
  "Work",
  "Workflows",
] as const;

export type ChannelName = (typeof CHANNELS)[number];
export type Surface =
  | "brain"
  | "explorer"
  | "loom"
  | "sessions"
  | "agents"
  | "code"
  | "knowledge"
  | "delivery"
  | "automations"
  | "observatory"
  | "costs"
  | "settings"
  | "work"
  | "workflows";

export const SURFACES: Surface[] = CHANNELS.map((c) => c.toLowerCase() as Surface);

export function channelToSurface(name: string): Surface {
  const hit = SURFACES.find((s) => s === name.toLowerCase());
  return hit ?? "brain";
}

export function surfaceToChannel(s: Surface): ChannelName {
  return CHANNELS.find((c) => c.toLowerCase() === s) ?? "Brain";
}

export type GraphNode = {
  id: string;
  label: string;
  cluster: string;
  color: string;
};

export type GraphEdge = { a: string; b: string };

const C = {
  graph: "#5ee7ff",
  index: "#9be15d",
  ingest: "#67e8f9",
  analysis: "#f0b429",
  memory: "#c084fc",
  rules: "#fbbf24",
  export: "#7dd3fc",
};

export const SCOPED_NODES: GraphNode[] = [
  { id: "g-index", label: "td::graph::index", cluster: "graph", color: C.graph },
  { id: "g-walk", label: "td::graph::walk", cluster: "graph", color: C.graph },
  { id: "g-build", label: "td::graph::build", cluster: "graph", color: C.graph },
  { id: "g-resolve", label: "td::graph::resolve", cluster: "graph", color: C.graph },
  { id: "g-persist", label: "td::graph::persist", cluster: "graph", color: C.graph },
  { id: "g-prune", label: "td::graph::prune", cluster: "graph", color: C.graph },
  { id: "g-parser", label: "td::ingest::parser", cluster: "graph", color: C.graph },
  { id: "i-defs", label: "td::index::defs", cluster: "index", color: C.index },
  { id: "i-symbols", label: "td::index::symbols", cluster: "index", color: C.index },
  { id: "i-refs", label: "td::index::refs", cluster: "index", color: C.index },
  { id: "i-usages", label: "td::index::usages", cluster: "index", color: C.index },
  { id: "n-match", label: "td::ingest::match", cluster: "ingest", color: C.ingest },
  { id: "n-rs", label: "td::ingest::rs", cluster: "ingest", color: C.ingest },
  { id: "n-git", label: "td::source::git", cluster: "ingest", color: C.ingest },
  { id: "n-deluge", label: "td::ingest::deluge", cluster: "ingest", color: C.ingest },
  { id: "n-http-src", label: "td::source::http", cluster: "ingest", color: C.ingest },
  { id: "n-http", label: "td::ingest::http", cluster: "ingest", color: C.ingest },
  { id: "a-centrality", label: "td::analysis::centrality", cluster: "analysis", color: C.analysis },
  { id: "a-communities", label: "td::analysis::communities", cluster: "analysis", color: C.analysis },
  { id: "a-paths", label: "td::analysis::paths", cluster: "analysis", color: C.analysis },
  { id: "a-rank", label: "td::analysis::rank", cluster: "analysis", color: C.analysis },
  { id: "a-similarity", label: "td::analysis::similarity", cluster: "analysis", color: C.analysis },
  { id: "m-embed", label: "td::memory::embed", cluster: "memory", color: C.memory },
  { id: "m-search", label: "td::memory::search", cluster: "memory", color: C.memory },
  { id: "m-store", label: "td::memory::store", cluster: "memory", color: C.memory },
  { id: "m-evict", label: "td::memory::evict", cluster: "memory", color: C.memory },
  { id: "m-hydrate", label: "td::memory::hydrate", cluster: "memory", color: C.memory },
  { id: "r-match", label: "td::rules::match", cluster: "rules", color: C.rules },
  { id: "r-engine", label: "td::rules::engine", cluster: "rules", color: C.rules },
  { id: "r-apply", label: "td::rules::apply", cluster: "rules", color: C.rules },
  { id: "r-score", label: "td::rules::score", cluster: "rules", color: C.rules },
  { id: "e-json", label: "td::export::json", cluster: "export", color: C.export },
  { id: "e-pack", label: "td::export::pack", cluster: "export", color: C.export },
  { id: "e-parquet", label: "td::export::parquet", cluster: "export", color: C.export },
  { id: "e-delta", label: "td::export::delta", cluster: "export", color: C.export },
];

function star(ids: string[]): GraphEdge[] {
  const [h, ...rest] = ids;
  const edges: GraphEdge[] = rest.map((b) => ({ a: h, b }));
  for (let i = 0; i < rest.length - 1; i++) {
    if (i % 2 === 0) edges.push({ a: rest[i], b: rest[i + 1] });
  }
  return edges;
}

export const SCOPED_EDGES: GraphEdge[] = [
  ...star(["g-build", "g-index", "g-walk", "g-resolve", "g-persist", "g-prune", "g-parser"]),
  ...star(["i-symbols", "i-defs", "i-refs", "i-usages"]),
  ...star(["n-rs", "n-match", "n-git", "n-deluge", "n-http-src", "n-http"]),
  ...star(["a-rank", "a-centrality", "a-communities", "a-paths", "a-similarity"]),
  ...star(["m-store", "m-embed", "m-search", "m-evict", "m-hydrate"]),
  ...star(["r-engine", "r-match", "r-apply", "r-score"]),
  ...star(["e-pack", "e-json", "e-parquet", "e-delta"]),
  { a: "g-parser", b: "n-rs" },
  { a: "n-rs", b: "i-symbols" },
  { a: "n-rs", b: "a-rank" },
  { a: "n-rs", b: "m-store" },
  { a: "r-engine", b: "n-rs" },
  { a: "e-pack", b: "n-rs" },
  { a: "a-rank", b: "m-store" },
];

export const SCOPED_KPI = [
  { label: "NODES", value: "7,842", note: "synthetic" },
  { label: "EDGES", value: "26,381", note: "synthetic" },
  { label: "FILES", value: "3,217", note: "synthetic" },
  { label: "FACTS", value: "18,964", note: "synthetic" },
  { label: "ENTITIES", value: "1,492", note: "synthetic" },
  { label: "EVENTS", value: "—", note: "Not captured in scope." },
];

export type BrainView = "overview" | "hover" | "repo-zoom" | "scoped" | "synapse" | "firing-tree" | "neuron-lab";
