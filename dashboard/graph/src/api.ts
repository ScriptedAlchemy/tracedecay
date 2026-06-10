import { fetchJSON } from "./sdk";
import type {
  GraphNeighborsResponse,
  GraphNodeResponse,
  GraphOverview,
  GraphPathResponse,
  GraphSearchResponse,
  GraphSubgraphResponse,
} from "./types";

const BASE = "/api/plugins/graph";

function qs(params: Record<string, string | number | undefined>) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") search.set(key, String(value));
  }
  const suffix = search.toString();
  return suffix ? `?${suffix}` : "";
}

export const api = {
  overview: () => fetchJSON<GraphOverview>(`${BASE}/overview`),
  search: (params: { q?: string; limit?: number; offset?: number } = {}) =>
    fetchJSON<GraphSearchResponse>(`${BASE}/search${qs(params)}`),
  node: (id: string) =>
    fetchJSON<GraphNodeResponse>(`${BASE}/node/${encodeURIComponent(id)}`),
  neighbors: (id: string, params: { limit?: number } = {}) =>
    fetchJSON<GraphNeighborsResponse>(
      `${BASE}/node/${encodeURIComponent(id)}/neighbors${qs(params)}`,
    ),
  subgraph: (params: { node_id?: string; q?: string; limit_nodes?: number; limit_edges?: number }) =>
    fetchJSON<GraphSubgraphResponse>(`${BASE}/subgraph${qs(params)}`),
  path: (params: { from: string; to: string; max_depth?: number }) =>
    fetchJSON<GraphPathResponse>(`${BASE}/path${qs(params)}`),
};
