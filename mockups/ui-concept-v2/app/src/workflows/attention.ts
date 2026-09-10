import type { AttentionItem } from "../app/workspace";

/** Authored fixture attention only; the snapshot has no served workflow authority. */
export const WORKFLOW_ATTENTION: AttentionItem[] = [
  {
    id: "workflow:run_wait_4d:capacity", title: "Workflow run waits for provider capacity",
    detail: "validate consistency has not acquired a lease; later nodes remain unreleased.",
    source: "workflow", severity: "warning", status: "active", owner: "system", evidence: "exact", mode: "fixture",
    observedAt: "2025-05-09T17:04:22Z", sourceRef: "run_wait_4d/node/s6", repository: "tracedecay",
    target: { surface: "workflows", params: { state: "enrich-graph", run: "run_wait_4d" } },
  },
  {
    id: "workflow:run_fail_1b:receipt", title: "Workflow terminal receipt records a schema failure",
    detail: "The run is terminal; persist graph and publication were never released.",
    source: "workflow", severity: "error", status: "active", owner: "agent", evidence: "exact", mode: "fixture",
    observedAt: "2025-05-09T15:44:19Z", sourceRef: "run_fail_1b/terminal-receipt", repository: "tracedecay",
    target: { surface: "workflows", params: { state: "enrich-graph", run: "run_fail_1b" } },
  },
];
