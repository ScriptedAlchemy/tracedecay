// @vitest-environment jsdom
import type { CallToolResult } from "@modelcontextprotocol/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectRef, ViewState } from "../src/shared/view.js";

type HostApp = {
  ontoolinput?: (params: { arguments?: Record<string, unknown> }) => void;
  ontoolresult?: (result: CallToolResult) => void;
  onhostcontextchanged?: (context: Record<string, unknown>) => void;
  onteardown?: () => Promise<unknown>;
};

const host = vi.hoisted(() => ({
  app: null as HostApp | null,
  callServerTool: vi.fn<(params: { name: string; arguments: Record<string, unknown> }, options?: { signal?: AbortSignal }) => Promise<CallToolResult>>(),
  updateModelContext: vi.fn(async (_context: unknown) => ({})),
}));

// Only the host boundary is replaced. Every DOM node, navigation decision,
// cancellation signal and result-adoption guard comes from the shipped app.
vi.mock("@modelcontextprotocol/ext-apps", () => ({
  App: class {
    constructor() { host.app = this as HostApp; }
    connect() { return Promise.resolve(); }
    getHostContext() { return undefined; }
    callServerTool = host.callServerTool;
    updateModelContext = host.updateModelContext;
  },
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

type Request = ReturnType<typeof deferred<CallToolResult>> & {
  readonly name: string;
  readonly args: Record<string, unknown>;
  readonly signal: AbortSignal;
};

const requests: Request[] = [];
const trackListeners = () => vi.spyOn(window, "addEventListener");
let addedListeners: ReturnType<typeof trackListeners>;

beforeEach(async () => {
  vi.resetModules();
  requests.length = 0;
  host.app = null;
  host.updateModelContext.mockClear();
  host.callServerTool.mockReset().mockImplementation((params, options) => {
    if (options?.signal === undefined) throw new Error("app tool request did not carry cancellation");
    const response = deferred<CallToolResult>();
    requests.push({ ...response, name: params.name, args: params.arguments, signal: options.signal });
    return response.promise;
  });
  document.body.innerHTML = '<div id="root"></div>';
  addedListeners = trackListeners();
  await import("../src/app/main.js");
  expect(host.app).not.toBeNull();
});

afterEach(async () => {
  await host.app?.onteardown?.();
  for (const [type, listener, options] of addedListeners.mock.calls) {
    window.removeEventListener(type, listener, options);
  }
  addedListeners.mockRestore();
  document.body.replaceChildren();
});

function project(index: number): ProjectRef {
  return {
    project_id: `proj_${index}`,
    label: `project-${index}`,
    project_root: `/repos/project-${index}`,
    head_branch: "feature",
    default_branch: "main",
  };
}

function projectsView(projects: readonly ProjectRef[], query = "", truncated = false): Extract<ViewState, { page: "projects" }> {
  return {
    page: "projects",
    query,
    daemon: { state: "connected", profile_root: "/isolated/profile", version: "test", pid: 123 },
    projects: {
      state: "ready",
      data: {
        projects,
        limit: 10,
        truncated,
        summary: { project_count: projects.length, repo_count: projects.length, truncated },
      },
    },
  };
}

function toolResult(view: ViewState): CallToolResult {
  return { content: [], structuredContent: view };
}

function pushView(view: ViewState): void {
  host.app!.ontoolresult!(toolResult(view));
}

function element<T extends HTMLElement = HTMLElement>(testId: string): T {
  const node = document.querySelector<T>(`[data-testid="${testId}"]`);
  if (node === null) throw new Error(`missing rendered element: ${testId}`);
  return node;
}

function submitProjectQuery(query: string, keyboard = false): Request {
  const input = element<HTMLInputElement>("project-search-input");
  input.value = query;
  if (keyboard) {
    input.focus();
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  } else {
    element<HTMLButtonElement>("project-search-submit").click();
  }
  const request = requests.at(-1);
  if (request === undefined) throw new Error("project search did not dispatch a request");
  return request;
}

async function deliver(request: Request, view: ViewState): Promise<void> {
  request.resolve(toolResult(view));
  // The app attached its continuation before this test awaits the same promise.
  await request.promise;
}

function expectSelected(projectId: string): void {
  expect(document.getElementById("root")?.dataset["page"]).toBe("search");
  const latest = host.updateModelContext.mock.calls.at(-1)?.[0];
  expect(latest).toMatchObject({ structuredContent: { page: "search", project_id: projectId } });
}

describe("project picker DOM", () => {
  it("labels the bounded recent list as partial without presenting its page count as a registry total", () => {
    pushView(projectsView(Array.from({ length: 10 }, (_, index) => project(index)), "", true));
    expect(document.querySelectorAll('[data-testid="project-row"]')).toHaveLength(10);
    expect(element("projects-panel").textContent).toContain("Recent registered projects");
    expect(element("projects-completeness").textContent).toContain("10 project(s) shown");
    expect(element("projects-completeness").textContent).toContain("outside this recent list");
    expect(element("projects-completeness").textContent).not.toContain("All registered projects");
    expect(element<HTMLInputElement>("project-search-input").getAttribute("aria-label")).toBe("Find registered projects");
  });

  it("finds a project outside the recent list and selects its exact identity before searching its code", async () => {
    const recent = project(0);
    const older = { ...project(11), label: recent.label };
    pushView(projectsView([recent], "", true));
    const request = submitProjectQuery(`  ${older.project_root}  `, true);
    expect(request).toMatchObject({ name: "tracedecay_search_projects", args: { query: older.project_root } });
    expect(element<HTMLInputElement>("project-search-input").value).toBe(older.project_root);
    expect(document.activeElement).toBe(element("project-search-input"));
    await deliver(request, projectsView([older], older.project_root));
    expect(element("projects-panel").textContent).toContain(`Projects matching "${older.project_root}"`);
    expect(element("project-open").getAttribute("data-project-id")).toBe(older.project_id);
    element<HTMLButtonElement>("project-open").click();
    expectSelected(older.project_id);
    element<HTMLInputElement>("search-input").value = "invoice";
    element<HTMLButtonElement>("search-submit").click();
    expect(requests.at(-1)).toMatchObject({ name: "tracedecay_search_code", args: { project_id: older.project_id, query: "invoice" } });
  });

  it("renders empty registration, zero matches and unavailable registry as distinct states", () => {
    const recent = projectsView([]);
    pushView({ ...recent, projects: { state: "empty", message: "No projects are registered in this TraceDecay profile." } });
    expect(element("projects-empty").textContent).toContain("No projects are registered");
    expect(element("projects-panel").textContent).toContain("Recent registered projects");

    pushView({ ...recent, query: "missing", projects: { state: "empty", message: 'No registered projects match "missing".' } });
    expect(element("projects-empty").textContent).toContain('No registered projects match "missing"');
    expect(element<HTMLInputElement>("project-search-input").value).toBe("missing");

    pushView({ ...recent, query: "missing", projects: { state: "failed", failure: { kind: "unavailable", code: "registry_unavailable", message: "Registry could not be read" } } });
    expect(element("projects-failed-kind").textContent).toBe("unavailable");
    expect(element("projects-failed").textContent).toContain("registry_unavailable");
    expect(document.querySelector('[data-testid="projects-empty"]')).toBeNull();
  });

  it("labels truncated search matches and lets the user return to the recent listing", async () => {
    pushView(projectsView([project(1)], "project", true));
    expect(element("projects-completeness").textContent).toContain("More matches are available; refine your search");
    element<HTMLButtonElement>("project-search-recent").click();
    const request = requests.at(-1)!;
    expect(request).toMatchObject({ name: "tracedecay_search_projects", args: { query: "" } });
    await deliver(request, projectsView([project(0)]));
    expect(element<HTMLInputElement>("project-search-input").value).toBe("");
    expect(element("projects-panel").textContent).toContain("Recent registered projects");
  });

  it("keeps the query editable while pending, aborts the replaced call and ignores its late success", async () => {
    pushView(projectsView([project(0)]));
    const first = submitProjectQuery("older");
    expect(element("loading").textContent).toContain("Searching projects");
    expect(element<HTMLButtonElement>("project-search-submit").disabled).toBe(false);
    const second = submitProjectQuery("shipping", true);
    expect(requests).toHaveLength(2);
    expect(first.signal.aborted).toBe(true);
    expect(second.signal.aborted).toBe(false);
    await deliver(first, projectsView([project(11)], "older"));
    expect(element<HTMLInputElement>("project-search-input").value).toBe("shipping");
    expect(element("loading").textContent).toContain('"shipping"');
    await deliver(second, projectsView([project(12)], "shipping"));
    expect(element("project-open").getAttribute("data-project-id")).toBe("proj_12");
    expect(document.querySelector('[data-testid="loading"]')).toBeNull();
  });

  it.each(["project-search-submit", "project-search-recent"])("keeps keyboard focus on %s through loading and results", async (testId) => {
    pushView(projectsView([project(0)], "older"));
    const control = element<HTMLButtonElement>(testId);
    // jsdom does not perform native keyboard activation. Focus the tab stop
    // and dispatch the click that Enter/Space generates on a button.
    control.focus();
    control.click();
    const request = requests.at(-1)!;
    const query = testId === "project-search-submit" ? "older" : "";
    expect(request).toMatchObject({ name: "tracedecay_search_projects", args: { query } });
    expect(control.isConnected).toBe(false);
    expect(document.activeElement).toBe(element(testId));
    await deliver(request, projectsView([project(11)], query));
    expect(document.activeElement).toBe(element(testId));
    // The restored control remains usable for the next keyboard activation.
    element<HTMLButtonElement>(testId).click();
    expect(requests).toHaveLength(2);
    expect(document.activeElement).toBe(element(testId));
  });

  it("keeps focus and the complete draft when typing successive characters over a pending query", async () => {
    pushView(projectsView([project(0)]));
    const first = submitProjectQuery("older");
    const originalInput = element<HTMLInputElement>("project-search-input");
    originalInput.focus();
    originalInput.setSelectionRange(0, originalInput.value.length);
    let typed = "";
    for (const character of "shipping") {
      const focused = document.activeElement;
      expect(focused).toBeInstanceOf(HTMLInputElement);
      const input = focused as HTMLInputElement;
      const start = input.selectionStart!;
      const end = input.selectionEnd!;
      input.value = `${input.value.slice(0, start)}${character}${input.value.slice(end)}`;
      input.setSelectionRange(start + 1, start + 1);
      input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: character }));
      typed += character;
      expect(document.activeElement).toBe(element("project-search-input"));
      expect(element<HTMLInputElement>("project-search-input").value).toBe(typed);
      expect(element<HTMLInputElement>("project-search-input").selectionStart).toBe(typed.length);
    }
    expect(first.signal.aborted).toBe(true);
    expect(requests).toHaveLength(1);
    element<HTMLInputElement>("project-search-input").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const second = requests.at(-1)!;
    expect(second.args).toEqual({ query: "shipping" });
    await deliver(first, projectsView([project(11)], "older"));
    expect(element<HTMLInputElement>("project-search-input").value).toBe("shipping");
    await deliver(second, projectsView([project(12)], "shipping"));
    expect(element("project-open").getAttribute("data-project-id")).toBe("proj_12");
  });

  it("does not replace newer results with a superseded request failure", async () => {
    pushView(projectsView([project(0)]));
    const first = submitProjectQuery("older");
    const second = submitProjectQuery("shipping");
    await deliver(second, projectsView([project(12)], "shipping"));
    const rendered = document.getElementById("root")!.innerHTML;
    first.reject(new Error("late registry failure"));
    await expect(first.promise).rejects.toThrow("late registry failure");
    expect(first.signal.aborted).toBe(true);
    expect(document.getElementById("root")!.innerHTML).toBe(rendered);
    expect(document.querySelector('[data-testid="host-error"]')).toBeNull();
  });

  it.each(["success", "failure"] as const)("does not replace an explicitly selected project with a pending search's late %s", async (outcome) => {
    pushView(projectsView([project(0)]));
    const request = submitProjectQuery("older");
    element<HTMLButtonElement>("project-open").click();
    expectSelected("proj_0");
    expect(request.signal.aborted).toBe(true);
    if (outcome === "success") await deliver(request, projectsView([project(11)], "older"));
    else {
      request.reject(new Error("late failure after project selection"));
      await expect(request.promise).rejects.toThrow("late failure after project selection");
    }
    expectSelected("proj_0");
    expect(element("crumb-project").textContent).toBe("project-0");
    expect(document.querySelector('[data-testid="host-error"]')).toBeNull();
  });

  it("invalidates a pending project search when the host deep-links to a registered project", async () => {
    pushView(projectsView([project(0)]));
    const request = submitProjectQuery("older");
    host.app!.onhostcontextchanged!({ "openai/deepLink": "/projects/proj_0" });
    expect(request.signal.aborted).toBe(true);
    expectSelected("proj_0");
    await deliver(request, projectsView([project(11)], "older"));
    expectSelected("proj_0");
  });

  it("invalidates pending app requests when an external host tool result arrives", async () => {
    pushView(projectsView([project(0)]));
    const request = submitProjectQuery("older");
    pushView(projectsView([project(12)], "shipping"));
    expect(request.signal.aborted).toBe(true);
    await deliver(request, projectsView([project(11)], "older"));
    expect(element<HTMLInputElement>("project-search-input").value).toBe("shipping");
    expect(element("project-open").getAttribute("data-project-id")).toBe("proj_12");
  });

  it.each(["teardown", "pagehide"] as const)("aborts a pending app request on %s and ignores its late result", async (event) => {
    pushView(projectsView([project(0)]));
    const request = submitProjectQuery("older");
    if (event === "teardown") await host.app!.onteardown!();
    else window.dispatchEvent(new Event("pagehide"));
    expect(request.signal.aborted).toBe(true);
    const rendered = document.getElementById("root")!.innerHTML;
    await deliver(request, projectsView([project(11)], "older"));
    expect(document.getElementById("root")!.innerHTML).toBe(rendered);
  });
});
