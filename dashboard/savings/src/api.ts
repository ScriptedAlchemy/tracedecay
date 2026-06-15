import { fetchJSON } from "../../lib/sdk";
import { qs } from "../../lib/qs";
import type {
  LedgerResponse,
  ModelsResponse,
  PricingResponse,
  SavingsOverview,
  SessionsResponse,
} from "./types";

const BASE = "/api/plugins/savings";

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
