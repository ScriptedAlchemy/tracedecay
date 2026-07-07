import { describe, expect, it } from "vitest";
import { projectPatchFrom, userPatchFrom } from "../settings/src/SettingsPanel";

describe("settings panel patch builders", () => {
  it("sends only changed project settings", () => {
    const saved = {
      includeText: "src/**\nREADME.md",
      excludeText: "target/**",
      maxFileSize: "1048576",
      extractDocstrings: true,
      trackCallSites: true,
      gitIgnore: true,
      telemetryTimings: true,
    };
    const draft = {
      ...saved,
      includeText: "src/**\nREADME.md\nexamples/**",
    };

    expect(projectPatchFrom(draft, saved)).toEqual({
      include: ["src/**", "README.md", "examples/**"],
    });
  });

  it("sends only changed user settings", () => {
    const saved = {
      uploadEnabled: true,
      watcherDebounce: "2s",
      extractionTimeoutSecs: "60",
    };
    const draft = {
      ...saved,
      watcherDebounce: " 15s ",
    };

    expect(userPatchFrom(draft, saved)).toEqual({
      watcher_debounce: "15s",
    });
  });
});
