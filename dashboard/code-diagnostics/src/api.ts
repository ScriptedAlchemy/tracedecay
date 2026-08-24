import { fetchJSON } from "../../lib/sdk";
import type { DiagnosticsSnapshot, IdleBackfillMode, LanguageSettings } from "./types";

const BASE = "/api/plugins/code-diagnostics";

export const api = {
  overview: () => fetchJSON<DiagnosticsSnapshot>(BASE),
  patchSettings: (patch: {
    idle_backfill?: IdleBackfillMode;
    languages?: Record<string, LanguageSettings>;
  }) =>
    fetchJSON<DiagnosticsSnapshot>(BASE, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    }),
  refreshAll: () =>
    fetchJSON<DiagnosticsSnapshot>(`${BASE}/refresh`, { method: "POST" }),
  refreshLanguage: (language: string) =>
    fetchJSON<DiagnosticsSnapshot>(
      `${BASE}/refresh/${encodeURIComponent(language)}`,
      { method: "POST" },
    ),
};
