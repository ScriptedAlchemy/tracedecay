import { fetchJSON } from "./sdk";
import type {
  LedgerResponse,
  ModelsResponse,
  PricingResponse,
  SavingsOverview,
  SessionsResponse,
} from "./types";

const BASE = "/api/plugins/savings";

function qs(params: Record<string, string | number | undefined>) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") search.set(key, String(value));
  }
  const suffix = search.toString();
  return suffix ? `?${suffix}` : "";
}

export const api = {
  overview: () => fetchJSON<SavingsOverview>(`${BASE}/overview`),
  ledger: (params: { range?: string } = {}) =>
    fetchJSON<LedgerResponse>(`${BASE}/ledger${qs(params)}`),
  sessions: (params: { range?: string; limit?: number; offset?: number } = {}) =>
    fetchJSON<SessionsResponse>(`${BASE}/sessions${qs(params)}`),
  models: (params: { range?: string } = {}) =>
    fetchJSON<ModelsResponse>(`${BASE}/models${qs(params)}`),
  pricing: () => fetchJSON<PricingResponse>(`${BASE}/pricing`),
};
