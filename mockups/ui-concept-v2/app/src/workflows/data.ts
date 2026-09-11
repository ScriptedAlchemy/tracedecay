export type DefinitionRow = { name: string; version: string; status: "ACTIVE"; updated: string };

export const DEFINITIONS: DefinitionRow[] = [
  ["code-intake", "v3", "12:12:44"], ["ingest-normalize", "v7", "18:03:21"],
  ["signal-detect", "v5", "17:45:09"], ["enrich-graph", "v4", "18:11:02"],
  ["cluster-topology", "v6", "16:33:18"], ["anomaly-score", "v4", "16:58:27"],
  ["rank-prioritize", "v5", "17:22:31"], ["explain-causality", "v4", "18:41:15"],
  ["plan-remediation", "v3", "19:01:07"], ["generate-artefacts", "v4", "19:08:36"],
  ["publish-delivery", "v3", "19:14:03"], ["feedback-learn", "v5", "18:55:42"],
  ["retain-archive", "v2", "19:20:11"], ["policy-guardrails", "v6", "17:05:54"],
].map(([name, version, time]) => ({ name, version, status: "ACTIVE", updated: `2025-05-09 ${time}` }));

export const REGISTRY_DIGESTS = [
  { set: "policy set", id: "pol_7f3c9a1e", sha: "sha256:9b9f3c2d…a9e0d1b7" },
  { set: "config set", id: "cfg_7f3c9a1e", sha: "sha256:4d1a8e76…c2b7f9e4" },
  { set: "catalog set", id: "cat_7f3c9a1e", sha: "sha256:7e6b2f11…d8f3c6aa" },
];

export const PINNED_REFS = [
  { set: "Policy Set", id: "pol_7f3c9a1e", sha: "sha256:9b9f3c2d…a9e0d1b7" },
  { set: "Config Set", id: "cfg_7f3c9a1e", sha: "sha256:4d1a8e76…c2b7f9e4" },
  { set: "Catalog Set", id: "cat_7f3c9a1e", sha: "sha256:7e6b2f11…d8f3c6aa" },
];

export const VERSION_TRACK = [
  ["v4", "ACTIVE", "2025-04-18 14:22:31", "2025-04-18 14:25:02", "—", "—"],
  ["v3", "RETIRED", "2025-03-22 11:07:19", "2025-03-22 11:12:04", "2025-04-18 14:24:55", "superseded"],
  ["v2", "RETIRED", "2025-02-10 09:33:41", "2025-02-10 09:36:10", "2025-03-22 11:11:58", "superseded"],
] as const;

export type WorkflowNodeState = "succeeded" | "failed-attempt" | "waiting" | "unexercised";
export type WorkflowNode = {
  id: string;
  label: string;
  operation: string;
  input: string;
  output: string;
  x: number;
  y: number;
  attempt?: string;
  changed?: "added" | "rewired";
};

export const WORKFLOW_NODES: WorkflowNode[] = [
  { id: "s1", label: "load entities", operation: "ingest.load", input: "sources.entities", output: "graph.raw", x: 9, y: 44 },
  { id: "s2", label: "resolve identities", operation: "identity.resolve", input: "graph.raw", output: "graph.resolved", x: 25, y: 44 },
  { id: "s3", label: "enrich attributes", operation: "graph.enrich", input: "graph.resolved", output: "graph.attributes", x: 41, y: 21, attempt: "attempt 1 failed · attempt 2 sealed", changed: "rewired" },
  { id: "s4", label: "link topology", operation: "graph.link", input: "graph.resolved", output: "graph.topology", x: 41, y: 67 },
  { id: "s5", label: "compute signals", operation: "signal.compute", input: "graph.attributes + graph.topology", output: "graph.signals", x: 60, y: 36, changed: "added" },
  { id: "s6", label: "validate consistency", operation: "policy.validate", input: "graph.signals", output: "graph.validated", x: 60, y: 68, attempt: "provider-capacity receipt required" },
  { id: "s7", label: "persist graph", operation: "store.persist", input: "graph.validated", output: "store.graph", x: 75, y: 53 },
  { id: "s8", label: "publish artifacts", operation: "artifact.publish", input: "store.graph", output: "artifact.manifest", x: 92, y: 53 },
];

export const WORKFLOW_EDGES: Array<[string, string]> = [
  ["s1", "s2"], ["s2", "s3"], ["s2", "s4"], ["s3", "s5"], ["s4", "s5"], ["s5", "s6"], ["s6", "s7"], ["s7", "s8"],
];

export const WORKFLOW_PINS = [
  { id: "policy", label: "POLICY PIN", value: "pol_7f3c9a1e", x: 48, y: 4, target: "s6" },
  { id: "config", label: "CONFIG PIN", value: "cfg_7f3c9a1e", x: 66, y: 4, target: "s5" },
  { id: "catalog", label: "CATALOG PIN", value: "cat_7f3c9a1e", x: 84, y: 4, target: "s8" },
] as const;

export const VERSION_GHOSTS = [
  { id: "v3-enrich", label: "enrich facts", x: 41, y: 21 },
  { id: "v3-validate", label: "validate links", x: 60, y: 58 },
] as const;

export const STEPS = [
  ["1", "s1", "load-entities", "ingest", "sources.entities", "graph.raw", "30s", "2"],
  ["2", "s2", "resolve-identities", "resolve", "graph.raw", "graph.resolved", "45s", "3"],
  ["3", "s3", "enrich-attributes", "transform", "graph.resolved", "graph.attributes", "60s", "3"],
  ["4", "s4", "link-topology", "transform", "graph.resolved", "graph.topology", "60s", "3"],
  ["5", "s5", "compute-signals", "compute", "graph.attributes,graph.topology", "graph.signals", "60s", "3"],
  ["6", "s6", "validate-consistency", "validate", "graph.signals", "graph.validated", "45s", "2"],
  ["7", "s7", "persist-graph", "persist", "graph.validated", "store.graph", "45s", "2"],
  ["8", "s8", "publish-artifacts", "publish", "store.graph", "artifact.manifest", "30s", "1"],
] as const;

export type WorkflowRun = {
  id: string;
  definition: string;
  version: string;
  state: "COMPLETED" | "WAITING" | "FAILED";
  result: "sealed terminal receipt" | "no terminal receipt" | "failed terminal receipt";
  started: string;
  updated: string;
  duration: string;
  steps: string;
  route: string;
  summary: string;
  delivery: { task: string; checks: string; review: string; state: "pending" | "blocked" | "unavailable" };
};

export const WORKFLOW_RUNS: WorkflowRun[] = [
  {
    id: "run_7f3c9a", definition: "enrich-graph", version: "v4", state: "COMPLETED", result: "sealed terminal receipt",
    started: "2025-05-09 16:14:02", updated: "2025-05-09 16:19:38", duration: "00:05:36", steps: "7 exercised · 1 condition false", route: "codex-app-server / gpt-5.6", summary: "Attempt 1 at enrich attributes failed; attempt 2 sealed without refund.",
    delivery: { task: "task_td-413", checks: "not evaluated", review: "not requested", state: "pending" },
  },
  {
    id: "run_wait_4d", definition: "enrich-graph", version: "v4", state: "WAITING", result: "no terminal receipt",
    started: "2025-05-09 17:02:11", updated: "2025-05-09 17:04:22", duration: "00:02:11", steps: "5 exercised · 3 not released", route: "provider capacity class: deferred", summary: "Validate consistency is waiting for admitted provider capacity; no later node is released.",
    delivery: { task: "task_td-418", checks: "not started", review: "not requested", state: "pending" },
  },
  {
    id: "run_fail_1b", definition: "enrich-graph", version: "v4", state: "FAILED", result: "failed terminal receipt",
    started: "2025-05-09 15:42:01", updated: "2025-05-09 15:44:19", duration: "00:02:18", steps: "3 exercised · 5 unexercised", route: "codex-app-server / gpt-5.6", summary: "Schema mismatch sealed a failed terminal receipt before persist graph.",
    delivery: { task: "task_td-407", checks: "blocked by terminal failure", review: "not requested", state: "blocked" },
  },
];

export const RUN_ROWS = (run: WorkflowRun) => [
  ["State", run.state], ["Workflow", `${run.definition} ${run.version}`], ["Started (UTC)", run.started],
  ["Updated (UTC)", run.updated], ["Elapsed", run.duration], ["Terminal truth", run.result],
  ["Released nodes", run.steps], ["Requested / actual route", run.route],
] as const;

export function RUN_STEPS(run: WorkflowRun) {
  if (run.state === "WAITING") return [
    ["s1", "load entities", "success", "17:02:11", "00:00:18"],
    ["s2", "resolve identities", "success", "17:02:29", "00:00:27"],
    ["s3", "enrich attributes", "success", "17:02:56", "00:00:39"],
    ["s4", "link topology", "success", "17:03:35", "00:00:31"],
    ["s5", "compute signals", "success", "17:04:06", "00:00:16"],
    ["s6", "validate consistency", "waiting capacity", "17:04:22", "—"],
    ["s7", "persist graph", "unreleased", "—", "—"],
    ["s8", "publish artifacts", "unreleased", "—", "—"],
  ] as const;
  if (run.state === "FAILED") return [
    ["s1", "load entities", "success", "15:42:01", "00:00:19"],
    ["s2", "resolve identities", "success", "15:42:20", "00:00:28"],
    ["s3", "enrich attributes", "failed receipt", "15:42:48", "00:01:31"],
    ["s4", "link topology", "unreleased", "—", "—"],
    ["s5", "compute signals", "unreleased", "—", "—"],
    ["s6", "validate consistency", "unreleased", "—", "—"],
    ["s7", "persist graph", "unreleased", "—", "—"],
    ["s8", "publish artifacts", "unreleased", "—", "—"],
  ] as const;
  return [
    ["s1", "load entities", "success", "16:14:02", "00:00:19"],
    ["s2", "resolve identities", "success", "16:14:21", "00:00:28"],
    ["s3", "enrich attributes", "retry sealed", "16:14:49", "00:00:41"],
    ["s4", "link topology", "success", "16:15:30", "00:00:47"],
    ["s5", "compute signals", "success", "16:16:17", "00:00:36"],
    ["s6", "validate consistency", "success", "16:16:53", "00:00:24"],
    ["s7", "persist graph", "success", "16:17:17", "00:00:29"],
    ["s8", "publish artifacts", "condition false", "—", "—"],
  ] as const;
}
