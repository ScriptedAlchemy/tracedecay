/**
 * Holographic-memory API client.
 *
 * All calls route through the plugin's own backend at
 * `/api/plugins/holographic/*` (served by `plugin_api.py`) via the host SDK's
 * `fetchJSON`, which transparently attaches the dashboard session token,
 * profile header, and base-path prefix.
 */

import { fetchJSON } from "./sdk";
import type {
  AutomationRunRequest,
  AutomationSchedulerStatusResponse,
  FactProposalListResponse,
  FactProposalResponse,
  MemoryAgentPlanResponse,
  MemoryAutomationConfigPatch,
  MemoryAutomationConfigResponse,
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunArtifactsResponse,
  MemoryAutomationRunResponse,
  MemoryAutomationRunsResponse,
  MemoryCurateApplyResponse,
  MemoryCurateOp,
  MemoryCurateResponse,
  MemoryCuratorActivityResponse,
  MemoryCuratorPreviewResponse,
  MemoryCuratorStatusResponse,
  MemoryDashboardResponse,
  MemoryFactDetailResponse,
  MemoryOplogResponse,
  MemoryProjectionResponse,
  MemorySimilarityResponse,
  MemoryStatusResponse,
  ManagedSkillListResponse,
  ManagedSkillResponse,
} from "./types";

const BASE = "/api/plugins/holographic";
const AUTOMATION_BASE = "/api/automation";

function withLimit(path: string, limit?: number): string {
  const qs = new URLSearchParams();
  if (limit) qs.set("limit", String(limit));
  const suffix = qs.toString();
  return `${path}${suffix ? `?${suffix}` : ""}`;
}

function managedSkillPath(id: string, action?: string): string {
  const path = `${AUTOMATION_BASE}/skills/${encodeURIComponent(id)}`;
  return action ? `${path}/${action}` : path;
}

function factProposalPath(id: string, action?: string): string {
  const path = `${AUTOMATION_BASE}/fact-proposals/${encodeURIComponent(id)}`;
  return action ? `${path}/${action}` : path;
}

function postAutomationRun<TReport = Record<string, unknown>>(
  path: string,
  body: AutomationRunRequest = {},
) {
  return fetchJSON<MemoryAutomationRunResponse<TReport>>(`${AUTOMATION_BASE}/run/${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ dry_run: true, ...body }),
  });
}

function postManagedSkillAction(id: string, action: string) {
  return fetchJSON<ManagedSkillResponse>(managedSkillPath(id, action), {
    method: "POST",
  });
}

export const api = {
  /** Overview + facts + entities + graph (GET /). */
  getMemoryDashboard: (
    params: { q?: string; limit?: number; graphLimit?: number } = {},
  ) => {
    const qs = new URLSearchParams();
    if (params.q) qs.set("q", params.q);
    if (params.limit) qs.set("limit", String(params.limit));
    if (params.graphLimit) qs.set("graph_limit", String(params.graphLimit));
    const suffix = qs.toString();
    return fetchJSON<MemoryDashboardResponse>(
      `${BASE}/${suffix ? `?${suffix}` : ""}`,
    );
  },

  /** Rich health/status surface for the holographic memory store (GET /status). */
  getMemoryStatus: () => fetchJSON<MemoryStatusResponse>(`${BASE}/status`),

  /** Full fact content + linked entities (GET /fact/{id}). */
  getMemoryFact: (factId: number) =>
    fetchJSON<MemoryFactDetailResponse>(
      `${BASE}/fact/${encodeURIComponent(factId)}`,
    ),

  /** 2D PCA projection of HRR vectors (GET /projection). */
  getMemoryProjection: (params: { q?: string; limit?: number } = {}) => {
    const qs = new URLSearchParams();
    if (params.q) qs.set("q", params.q);
    if (params.limit) qs.set("limit", String(params.limit));
    const suffix = qs.toString();
    return fetchJSON<MemoryProjectionResponse>(
      `${BASE}/projection${suffix ? `?${suffix}` : ""}`,
    );
  },

  /**
   * Near-duplicate fact pairs above a similarity floor (GET /similarity).
   * `minSimilarity` is sent as both `min_similarity` (new servers, supports
   * values below 0.5 and full-population `score_distribution`) and
   * `threshold` (older servers) so either backend honors the floor.
   */
  getMemorySimilarity: (
    params: { minSimilarity?: number; limit?: number } = {},
  ) => {
    const qs = new URLSearchParams();
    if (params.minSimilarity != null) {
      qs.set("min_similarity", String(params.minSimilarity));
      qs.set("threshold", String(params.minSimilarity));
    }
    if (params.limit) qs.set("limit", String(params.limit));
    const suffix = qs.toString();
    return fetchJSON<MemorySimilarityResponse>(
      `${BASE}/similarity${suffix ? `?${suffix}` : ""}`,
    );
  },

  /** Preview / apply memory curation maintenance (POST /curate). */
  postMemoryCurate: (body: { dry_run: boolean }) =>
    fetchJSON<MemoryCurateResponse>(`${BASE}/curate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  /** Standalone backend memory-curator review (POST /curation/agent-plan). */
  postMemoryAgentPlan: (
    body: { dry_run?: true; max_clusters?: number; min_confidence?: number } = {},
  ) =>
    fetchJSON<MemoryAgentPlanResponse>(`${BASE}/curation/agent-plan`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ dry_run: true, ...body }),
    }),

  /** Standalone memory-curator run (POST /api/automation/run/memory-curator). */
  postAutomationRunMemoryCurator: (body: AutomationRunRequest = {}) =>
    postAutomationRun<MemoryCurateResponse>("memory-curator", body),

  /** Standalone session-reflection run (POST /api/automation/run/session-reflection). */
  postAutomationRunSessionReflection: (body: AutomationRunRequest = {}) =>
    postAutomationRun("session-reflection", body),

  /** Standalone managed skill-writer run (POST /api/automation/run/skill-writing). */
  postAutomationRunSkillWriting: (body: AutomationRunRequest = {}) =>
    postAutomationRun("skill-writing", body),

  /** Apply explicit curation ops — delete/merge (POST /curate/apply). */
  postMemoryCurateApply: (body: { ops: MemoryCurateOp[] }) =>
    fetchJSON<MemoryCurateApplyResponse>(`${BASE}/curate/apply`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  /** Last saved dry-run preview for this project/profile (GET /curation/preview). */
  getMemoryCuratorPreview: () =>
    fetchJSON<MemoryCuratorPreviewResponse>(`${BASE}/curation/preview`),

  /** Read-only memory curator state/history metadata (GET /curation/status). */
  getMemoryCuratorStatus: () =>
    fetchJSON<MemoryCuratorStatusResponse>(`${BASE}/curation/status`),

  /** Effective automation config plus project override sidecar (GET /curation/config). */
  getMemoryAutomationConfig: () =>
    fetchJSON<MemoryAutomationConfigResponse>(`${BASE}/curation/config`),

  /** Persist project automation config overrides (PATCH /curation/config). */
  patchMemoryAutomationConfig: (body: MemoryAutomationConfigPatch) =>
    fetchJSON<MemoryAutomationConfigResponse>(`${BASE}/curation/config`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  /** Remove project automation overrides and return effective defaults (DELETE /curation/config). */
  resetMemoryAutomationConfig: () =>
    fetchJSON<MemoryAutomationConfigResponse>(`${BASE}/curation/config`, {
      method: "DELETE",
    }),

  /** Dashboard-visible automation scheduler state. */
  getAutomationSchedulerStatus: () =>
    fetchJSON<AutomationSchedulerStatusResponse>(`${AUTOMATION_BASE}/scheduler/status`),

  /** Pause scheduler dispatch through the scheduler control sidecar. */
  pauseAutomationScheduler: () =>
    fetchJSON<AutomationSchedulerStatusResponse>(`${AUTOMATION_BASE}/scheduler/pause`, {
      method: "POST",
    }),

  /** Resume scheduler dispatch through the scheduler control sidecar. */
  resumeAutomationScheduler: () =>
    fetchJSON<AutomationSchedulerStatusResponse>(`${AUTOMATION_BASE}/scheduler/resume`, {
      method: "POST",
    }),

  /** Recent standalone automation backend runs (GET /curation/runs). */
  getMemoryAutomationRuns: (params: { limit?: number } = {}) =>
    fetchJSON<MemoryAutomationRunsResponse>(
      withLimit(`${BASE}/curation/runs`, params.limit),
    ),

  /** Artifact metadata for one automation run (GET /api/automation/runs/{run_id}/artifacts). */
  getMemoryAutomationRunArtifacts: (runId: string) =>
    fetchJSON<MemoryAutomationRunArtifactsResponse>(
      `${AUTOMATION_BASE}/runs/${encodeURIComponent(runId)}/artifacts`,
    ),

  /** Verified artifact payload for one automation run artifact kind. */
  getMemoryAutomationRunArtifact: (runId: string, kind: string) =>
    fetchJSON<MemoryAutomationRunArtifactPayloadResponse>(
      `${AUTOMATION_BASE}/runs/${encodeURIComponent(runId)}/artifacts/${encodeURIComponent(kind)}`,
    ),

  /** Recent structured curator activity (GET /curation/activity). */
  getMemoryCuratorActivity: (params: { limit?: number } = {}) =>
    fetchJSON<MemoryCuratorActivityResponse>(
      withLimit(`${BASE}/curation/activity`, params.limit),
    ),

  /** Recent memory operations from the append-only oplog (GET /oplog). */
  getMemoryOplog: (params: { limit?: number } = {}) =>
    fetchJSON<MemoryOplogResponse>(withLimit(`${BASE}/oplog`, params.limit)),

  /** Profile-owned managed skill packages (GET /api/automation/skills). */
  getManagedSkills: () =>
    fetchJSON<ManagedSkillListResponse>(`${AUTOMATION_BASE}/skills`),

  /** Full managed skill body and metadata (GET /api/automation/skills/{id}). */
  getManagedSkill: (id: string) =>
    fetchJSON<ManagedSkillResponse>(managedSkillPath(id)),

  /** Approve a pending managed skill (POST /api/automation/skills/{id}/approve). */
  approveManagedSkill: (id: string) => postManagedSkillAction(id, "approve"),

  /** Discard a staged managed skill update without mutating the active revision. */
  discardManagedSkillUpdate: (id: string) => postManagedSkillAction(id, "discard-update"),

  /** Disable an active managed skill (POST /api/automation/skills/{id}/disable). */
  disableManagedSkill: (id: string) => postManagedSkillAction(id, "disable"),

  /** Archive a managed skill without deleting its package. */
  archiveManagedSkill: (id: string) => postManagedSkillAction(id, "archive"),

  /** Restore an archived/disabled skill to pending approval. */
  restoreManagedSkill: (id: string) => postManagedSkillAction(id, "restore"),

  /** Durable session-reflection fact proposals awaiting approval. */
  getFactProposals: (params: { state?: string; limit?: number } = {}) => {
    const qs = new URLSearchParams();
    if (params.state) qs.set("state", params.state);
    if (params.limit) qs.set("limit", String(params.limit));
    const suffix = qs.toString();
    return fetchJSON<FactProposalListResponse>(
      `${AUTOMATION_BASE}/fact-proposals${suffix ? `?${suffix}` : ""}`,
    );
  },

  /** Apply an approved fact proposal to the memory store. */
  applyFactProposal: (id: string) =>
    fetchJSON<FactProposalResponse>(factProposalPath(id, "apply"), {
      method: "POST",
    }),

  /** Reject a pending fact proposal without mutating memory. */
  rejectFactProposal: (id: string, reason?: string) =>
    fetchJSON<FactProposalResponse>(factProposalPath(id, "reject"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ reason: reason || null }),
    }),
};
