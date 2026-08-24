import { fetchJSON } from "../../lib/sdk";
import { qs } from "../../lib/qs";
import type {
  GraphNeighborsResponse,
  GraphNodeResponse,
  GraphOverview,
  GraphPathResponse,
  GraphSearchResponse,
  GraphSubgraphResponse,
} from "./types";

const BASE = "/api/plugins/graph";

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
