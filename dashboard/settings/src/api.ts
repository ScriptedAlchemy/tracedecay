import { fetchJSON } from "../../lib/sdk";
import type { ProjectSettingsPatch, SettingsPayload, UserSettingsPatch } from "./types";

const BASE = "/api/settings";

export const api = {
  getSettings: () => fetchJSON<SettingsPayload>(BASE),
  patchProject: (patch: ProjectSettingsPatch) =>
    fetchJSON<SettingsPayload>(`${BASE}/project`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    }),
  patchUser: (patch: UserSettingsPatch) =>
    fetchJSON<SettingsPayload>(`${BASE}/user`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    }),
};
