import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import SettingsPanel from "../settings/src/SettingsPanel";
import type { SettingsPayload } from "../settings/src/types";

function basePayload(): SettingsPayload {
  return {
    project: {
      config_path: "/profile/.tracedecay/projects/fixture/config.json",
      config: {
        version: 1,
        root_dir: "/work/project",
        include: [".github/**"],
        exclude: ["target/**", "**/node_modules/**"],
        max_file_size: 1048576,
        extract_docstrings: true,
        track_call_sites: true,
        git_ignore: true,
        telemetry: {
          timings: true,
        },
      },
      tracedecay_dir_gitignored: true,
    },
    user: {
      config_path: "/profile/.tracedecay/config.toml",
      upload_enabled: true,
      watcher_debounce: "2s",
      extraction_timeout_secs: 60,
      installed_agents: ["cursor"],
    },
    automation: {
      config_endpoint: "/api/plugins/holographic/curation/config",
      enabled: false,
      backend: "disabled",
      host_mode: "standalone",
    },
    environment: {
      global_accounting_mode: "default",
      global_accounting_enabled: true,
      pricing_offline: false,
      variables: [
        {
          name: "TRACEDECAY_OFFLINE",
          active: false,
          value: null,
          description: "Skips network pricing fetches.",
        },
        {
          name: "TRACEDECAY_GLOBAL_DB",
          active: true,
          value: "/tmp/global.db",
          description: "Pins the global accounting database.",
        },
      ],
    },
    storage: {
      project_id: "fixture",
      project_root: "/work/project",
      storage_mode: "profile_sharded",
      store_root: "/profile/.tracedecay/projects/fixture",
      dashboard_root: "/profile/.tracedecay/projects/fixture/dashboard",
      graph_db: "/profile/.tracedecay/projects/fixture/tracedecay.db",
      memory_db: "/profile/.tracedecay/projects/fixture/tracedecay.db",
      lcm_db: "/profile/.tracedecay/projects/fixture/sessions.db",
      lcm_scope: "profile_sharded",
      savings_db: "/tmp/global.db",
    },
    version: {
      version: "0.9.0",
      channel: "stable",
      cached_latest_version: null,
    },
  };
}

const apiMock = vi.hoisted(() => ({
  getSettings: vi.fn(),
  patchProject: vi.fn(),
  patchUser: vi.fn(),
}));

vi.mock("../settings/src/api", () => ({ api: apiMock }));

async function renderPanel(payload = basePayload()) {
  apiMock.getSettings.mockResolvedValue(payload);
  render(<SettingsPanel />);
  await waitFor(() => expect(screen.getByText("Indexing")).toBeDefined());
}

describe("SettingsPanel", () => {
  it("renders every settings section with read-only environment info", async () => {
    await renderPanel();

    expect(screen.getByText("Indexing")).toBeDefined();
    expect(screen.getByText("User & Uploads")).toBeDefined();
    expect(screen.getByText("Automation")).toBeDefined();
    expect(screen.getByText("Storage (read-only)")).toBeDefined();
    expect(screen.getByText("Environment (read-only)")).toBeDefined();

    // Project config values populate the form.
    expect(screen.getByLabelText("Exclude globs")).toHaveProperty(
      "value",
      "target/**\n**/node_modules/**",
    );
    expect(screen.getByLabelText("Max file size")).toHaveProperty("value", "1048576");

    // Env gates render name + set/unset verdict.
    expect(screen.getByText("TRACEDECAY_OFFLINE")).toBeDefined();
    expect(screen.getByText("unset")).toBeDefined();
    expect(screen.getByText("set: /tmp/global.db")).toBeDefined();

    // Version/channel and storage paths are shown.
    expect(screen.getByText("0.9.0 (stable)")).toBeDefined();
    // graph_db and memory_db intentionally share a path in this fixture.
    expect(
      screen.getAllByText("/profile/.tracedecay/projects/fixture/tracedecay.db", {
        selector: "code",
      }).length,
    ).toBe(2);

    // Automation links to the existing editor instead of re-implementing it.
    const automationLink = screen.getByText("Open automation config");
    expect(automationLink.getAttribute("href")).toBe("?tab=holographic");

    // Save buttons start disabled (no dirty state yet).
    for (const button of screen.getAllByText("Save")) {
      expect((button as HTMLButtonElement).disabled).toBe(true);
    }
  });

  it("saves edited project settings and surfaces the re-sync banner", async () => {
    await renderPanel();

    fireEvent.change(screen.getByLabelText("Exclude globs"), {
      target: { value: "target/**\ndist/**" },
    });
    expect(screen.getByText("Unsaved changes")).toBeDefined();

    const response = basePayload();
    response.project.config.exclude = ["target/**", "dist/**"];
    response.resync_recommended = true;
    apiMock.patchProject.mockResolvedValue(response);

    const [projectSave] = screen.getAllByText("Save");
    expect((projectSave as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(projectSave);

    await waitFor(() => expect(apiMock.patchProject).toHaveBeenCalledTimes(1));
    expect(apiMock.patchProject).toHaveBeenCalledWith({
      exclude: ["target/**", "dist/**"],
    });

    await waitFor(() =>
      expect(screen.getByText(/a re-sync is required/)).toBeDefined(),
    );
    // Draft resets to the saved payload — dirty badge disappears.
    expect(screen.queryByText("Unsaved changes")).toBeNull();
  });

  it("saves telemetry timing opt-out from project settings", async () => {
    await renderPanel();

    fireEvent.click(screen.getByLabelText("Capture timing telemetry"));

    const response = basePayload();
    response.project.config.telemetry.timings = false;
    apiMock.patchProject.mockResolvedValue(response);

    const [projectSave] = screen.getAllByText("Save");
    fireEvent.click(projectSave);

    await waitFor(() => expect(apiMock.patchProject).toHaveBeenCalledTimes(1));
    expect(apiMock.patchProject).toHaveBeenCalledWith({
      telemetry: { timings: false },
    });
  });

  it("shows inline validation errors from the API on save failure", async () => {
    await renderPanel();

    fireEvent.change(screen.getByLabelText("Exclude globs"), {
      target: { value: "[invalid" },
    });

    const error = Object.assign(new Error("settings validation failed"), {
      body: {
        validation_errors: [
          { field: "exclude", message: "invalid glob pattern '[invalid'" },
        ],
      },
    });
    apiMock.patchProject.mockRejectedValue(error);

    const [projectSave] = screen.getAllByText("Save");
    fireEvent.click(projectSave);

    await waitFor(() =>
      expect(screen.getByText("invalid glob pattern '[invalid'")).toBeDefined(),
    );
    // The draft stays dirty so the user can fix and retry.
    expect(screen.getByText("Unsaved changes")).toBeDefined();
  });

  it("saves user settings and surfaces the restart-required banner", async () => {
    await renderPanel();

    fireEvent.change(screen.getByLabelText("Watcher debounce"), {
      target: { value: "15s" },
    });

    const response = basePayload();
    response.user.watcher_debounce = "15s";
    response.restart_recommended = true;
    apiMock.patchUser.mockResolvedValue(response);

    const userSave = screen.getAllByText("Save")[1];
    expect((userSave as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(userSave);

    await waitFor(() => expect(apiMock.patchUser).toHaveBeenCalledTimes(1));
    expect(apiMock.patchUser).toHaveBeenCalledWith({
      watcher_debounce: "15s",
    });

    await waitFor(() =>
      expect(screen.getByText(/restart the tracedecay/)).toBeDefined(),
    );
  });

  it("discards edits back to the saved values", async () => {
    await renderPanel();

    fireEvent.change(screen.getByLabelText("Max file size"), {
      target: { value: "2048" },
    });
    expect(screen.getByText("Unsaved changes")).toBeDefined();

    const [projectDiscard] = screen.getAllByText("Discard");
    fireEvent.click(projectDiscard);

    expect(screen.getByLabelText("Max file size")).toHaveProperty("value", "1048576");
    expect(screen.queryByText("Unsaved changes")).toBeNull();
    expect(apiMock.patchProject).not.toHaveBeenCalled();
  });
});
