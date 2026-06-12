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
  MemoryCurateApplyResponse,
  MemoryCurateOp,
  MemoryCurateResponse,
  MemoryCuratorActivityResponse,
  MemoryCuratorPreviewResponse,
  MemoryCuratorStatusResponse,
  MemoryDashboardResponse,
  MemoryFactDetailResponse,
  MemoryProjectionResponse,
  MemorySimilarityResponse,
} from "./types";

const BASE = "/api/plugins/holographic";

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

  /** Apply explicit curation ops — delete/merge (POST /curate/apply). */
  postMemoryCurateApply: (body: { ops: MemoryCurateOp[] }) =>
    fetchJSON<MemoryCurateApplyResponse>(`${BASE}/curate/apply`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  /** Last saved dry-run preview for this Hermes profile (GET /curation/preview). */
  getMemoryCuratorPreview: () =>
    fetchJSON<MemoryCuratorPreviewResponse>(`${BASE}/curation/preview`),

  /** Read-only memory curator state/history metadata (GET /curation/status). */
  getMemoryCuratorStatus: () =>
    fetchJSON<MemoryCuratorStatusResponse>(`${BASE}/curation/status`),

  /** Recent structured curator activity (GET /curation/activity). */
  getMemoryCuratorActivity: (params: { limit?: number } = {}) => {
    const qs = new URLSearchParams();
    if (params.limit) qs.set("limit", String(params.limit));
    const suffix = qs.toString();
    return fetchJSON<MemoryCuratorActivityResponse>(
      `${BASE}/curation/activity${suffix ? `?${suffix}` : ""}`,
    );
  },

};
