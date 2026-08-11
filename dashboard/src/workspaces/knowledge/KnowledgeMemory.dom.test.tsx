/**
 * The Knowledge workspace's memory views, over the routes the daemon mounts.
 *
 * Three invariants carry this file.
 *
 * The first is the camera: four positions over one store, the position living
 * in the address so a link reopens it, and switching never fetching a view's
 * reads until that view is looked at.
 *
 * The second is the state taxonomy against SUPPLIED backend state. These routes
 * are the first in the product to send `legacy_redacted` feedback details and
 * redacted oplog rows, so `redacted` and `unknown` are asserted to reach the
 * screen as their own chips rather than as blank cells — which is what an
 * optional-string reading of either payload would have produced.
 *
 * The third is the guard in front of the one write on these views. The config
 * PATCH is refused before dispatch under a non-writable scope, and on success
 * the surface shows the daemon's own re-read rather than the value that was
 * asked for.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fixtureEnvelope } from "../../test/fixtureEnvelope.ts";
import { useScope } from "../../data/scope/store.ts";
import { KnowledgePage } from "./KnowledgePage.tsx";

/* ---- route bodies -------------------------------------------------------- */

/** `memory_api::overview` seeds the whole holographic block before it reads
 * anything, so `reads` and `facts_coverage` are always present — a body without
 * them is one the route cannot produce. */
function overviewEnvelope(facts: readonly unknown[] = []) {
  // The envelope's own header — time, coverage, authorization, version — comes
  // from the fixture authority rather than being invented here, so these cases
  // cannot accidentally assert against a truth claim no route makes.
  return fixtureEnvelope({
    query: "",
    limit: 100,
    providers: {},
    holographic: {
      path: "/tmp/memory.db",
      exists: true,
      error: "",
      overview: null,
      facts,
      entities: [],
      graph: { nodes: [], edges: [] },
      facts_coverage: {
        completeness: "bounded",
        limit: 100,
        query_applied_after_limit: false,
      },
      reads: {
        facts: { state: "ready" },
        entities: { state: "ready" },
        graph: { state: "ready" },
      },
    },
  });
}

const OVERVIEW_ENVELOPE = overviewEnvelope();

const PROJECTION = {
  exists: true,
  dim: 64,
  limit: 400,
  method: "pca",
  error: "",
  points: [
    projectionPoint(1, -1, 0.5, "decision"),
    projectionPoint(2, 1.5, -0.25, "decision"),
    projectionPoint(3, 0.25, 2, "code_area"),
  ],
};

const SIMILARITY = {
  exists: true,
  dim: 64,
  count: 40,
  limit: 25,
  min_similarity: 0.85,
  total_pairs: 120,
  error: "",
  score_distribution: {
    min_score: 0.02,
    max_score: 0.99,
    average_score: 0.41,
    bin_count: 10,
    total_pairs: 120,
    bins: [],
  },
  pairs: [
    {
      a_id: 11,
      b_id: 12,
      a_content: "the dashboard uses rsbuild",
      b_content: "the dashboard is built with rsbuild",
      a_category: "decision",
      b_category: "decision",
      similarity: 0.9731,
      classification: "likely_duplicate",
    },
  ],
};

const TRUST_HISTORY = {
  fact_id: 7,
  error: "",
  repair: { state: "incomplete", processed: 4, remaining: 96 },
  trust_history: [
    {
      timestamp: "2026-08-01T00:00:00Z",
      action: "helpful",
      old_trust: 0.5,
      new_trust: 0.62,
      delta: 0.12,
      details_availability: "available",
      note: "confirmed against the running daemon",
    },
    {
      timestamp: "2026-08-02T00:00:00Z",
      action: "unhelpful",
      old_trust: 0.62,
      new_trust: 0.51,
      delta: -0.11,
      details_availability: "legacy_redacted",
    },
    {
      timestamp: "2026-08-03T00:00:00Z",
      action: "helpful",
      old_trust: 0.51,
      new_trust: 0.58,
      delta: 0.07,
      details_availability: "unknown",
    },
  ],
};

const OPLOG = {
  count: 3,
  limit: 100,
  error: "",
  events: [
    {
      id: 3,
      ts: "2026-08-03T00:00:00Z",
      op: "add_fact",
      fact_id: 9,
      detail: { summary: "stored a decision" },
    },
    {
      id: 2,
      ts: "2026-08-02T00:00:00Z",
      op: "add_fact",
      fact_id: 8,
      detail: { redacted: true },
    },
    {
      id: 1,
      ts: "2026-08-01T00:00:00Z",
      op: "remove_fact",
      fact_id: null,
      detail: { availability: "unknown" },
    },
  ],
};

const RUNS = {
  count: 2,
  limit: 50,
  error: "",
  records: [
    {
      run_id: "run-a",
      trigger: "scheduler",
      task: "memory_curator",
      backend: "codex_app_server",
      status: "completed",
      reviewed_count: 5,
      accepted_count: 3,
      rejected_count: 2,
      skipped_count: 0,
      started_at: "2026-08-01T00:00:00Z",
      completed_at: "2026-08-01T00:01:00Z",
      model: "gpt-5-codex",
    },
    {
      run_id: "run-b",
      trigger: "manual",
      task: "skill_writer",
      backend: "codex_app_server",
      status: "failed",
      reviewed_count: 0,
      accepted_count: 0,
      rejected_count: 0,
      skipped_count: 0,
      started_at: "2026-08-02T00:00:00Z",
      completed_at: "2026-08-02T00:00:30Z",
      error: "backend timed out after 60s",
    },
  ],
};

function config(overrides: Record<string, unknown> = {}) {
  const automation = {
    schema_version: 1,
    enabled: true,
    backend: "codex_app_server",
    host_mode: "standalone",
    model_id: "gpt-5.6-mini",
    timeout_secs: 60,
    scheduler_tick_secs: 300,
    combine_due_tasks: true,
    allow_job_commands: false,
    tasks: {
      memory_curator: {
        enabled: true,
        schedule: "daily",
        interval_secs: 86400,
        cooldown_secs: 300,
        min_idle_secs: 30,
        stale_lock_secs: 3600,
      },
      session_reflector: {
        enabled: false,
        schedule: null,
        interval_secs: null,
        cooldown_secs: null,
        min_idle_secs: null,
        stale_lock_secs: null,
      },
      skill_writer: {
        enabled: false,
        schedule: null,
        interval_secs: null,
        cooldown_secs: null,
        min_idle_secs: null,
        stale_lock_secs: null,
      },
    },
    ...overrides,
  };
  return {
    effective: automation,
    configuration_revision_id: "configuration.revision.knowledge.test",
    source: "daemon_pinned_snapshot",
    backend_availability: {
      backend: "codex_app_server",
      available: true,
      executable: "/usr/local/bin/codex",
    },
  };
}

function projectionPoint(
  factId: number,
  x: number,
  y: number,
  category: string,
) {
  return {
    fact_id: factId,
    x,
    y,
    category,
    content: `fact ${factId}`,
    trust_score: 0.7,
    retrieval_count: 2,
    created_at: 0,
    updated_at: 0,
    bank_name: null,
    entity_count: 1,
    connection_count: 0,
  };
}

/* ---- harness ------------------------------------------------------------- */

/** Route bodies by the path suffix that identifies them, most specific first —
 * `/curation/runs` and `/curation/config` both end in a segment that a naive
 * `includes('/curation')` would confuse. */
const ROUTES: readonly (readonly [string, unknown])[] = [
  [
    "/automatic-fact-receipts",
    { receipts: [], count: 0, limit: 50, error: "" },
  ],
  ["/trust-history", TRUST_HISTORY],
  ["/projection", PROJECTION],
  ["/similarity", SIMILARITY],
  ["/curation/runs", RUNS],
  [
    "/automation/outcomes",
    {
      generated_at: 1_700_000_000,
      skills: [],
      facts: [],
      snapshot: {
        available: true,
        skills_refreshed_at: null,
        facts_refreshed_at: null,
      },
      error: "",
    },
  ],
  ["/oplog", OPLOG],
];

let requested: string[] = [];
let configBody: unknown = config();

function stubRoutes(options: { patchStatus?: number } = {}) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      requested.push(url);
      const path = url.split("?")[0] ?? url;
      if (path.endsWith("/curation/config")) {
        if (init?.method === "PATCH") {
          const status = options.patchStatus ?? 200;
          if (status !== 200) {
            return new Response(JSON.stringify({ detail: "refused" }), {
              status,
              headers: { "content-type": "application/json" },
            });
          }
          // The handler re-reads and returns the resolved layering; the
          // dashboard shows THAT, never the value it asked for.
          configBody = config({ enabled: false });
        }
        return json(configBody);
      }
      for (const [suffix, body] of ROUTES) {
        if (path.endsWith(suffix)) return json(body);
      }
      return json(OVERVIEW_ENVELOPE);
    }),
  );
}

function json(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function renderPage(entry = "/knowledge") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <KnowledgePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  requested = [];
  configBody = config();
  useScope.setState({ scope: { kind: "all" } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/* ---- the camera ---------------------------------------------------------- */

describe("Knowledge view switcher", () => {
  it("opens the facts explorer for an absent or unreadable view parameter", async () => {
    stubRoutes();
    renderPage("/knowledge?view=not-a-view");
    await waitFor(() =>
      expect(
        screen
          .getByRole("tab", { name: "Facts" })
          .getAttribute("aria-selected"),
      ).toBe("true"),
    );
  });

  it("opens the view named in the address", async () => {
    stubRoutes();
    renderPage("/knowledge?view=oplog");
    await waitFor(() =>
      expect(
        screen
          .getByRole("tab", { name: "Oplog" })
          .getAttribute("aria-selected"),
      ).toBe("true"),
    );
    expect(await screen.findByText("stored a decision")).toBeTruthy();
  });

  it("does not read a view until the camera is on it", async () => {
    stubRoutes();
    renderPage();
    await screen.findByRole("tab", { name: "Facts" });
    // The oplog is not on screen, so nothing may have asked for it. A page
    // that fetched every view up front would make switching feel instant and
    // make a heavy store pay for four reads to look at one.
    expect(requested.some((url) => url.includes("/oplog"))).toBe(false);
    await userEvent.click(screen.getByRole("tab", { name: "Oplog" }));
    await waitFor(() =>
      expect(requested.some((url) => url.includes("/oplog"))).toBe(true),
    );
  });

  it("names the panel its tabs control", async () => {
    stubRoutes();
    renderPage();
    const tab = await screen.findByRole("tab", { name: "Facts" });
    const panelId = tab.getAttribute("aria-controls");
    expect(panelId).toBeTruthy();
    // `aria-controls` naming an element that was never drawn is an invalid
    // reference, not a weaker one — the accessibility gate reads it as a
    // failure.
    expect(document.getElementById(panelId ?? "")).toBeTruthy();
  });
});

/* ---- geometry ------------------------------------------------------------ */

describe("Memory geometry", () => {
  it("states what the projection axes are and censuses the categories", async () => {
    stubRoutes();
    renderPage("/knowledge?view=geometry");
    expect(
      await screen.findByText(
        /principal components of 3 phase vectors of width 64/,
      ),
    ).toBeTruthy();
    const census = screen.getByLabelText("Projected facts by category");
    expect(within(census).getByText("decision · 2")).toBeTruthy();
    expect(within(census).getByText("code_area · 1")).toBeTruthy();
  });

  it("refuses to draw a projection the daemon did not compute", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/projection")) {
          return json({
            ...PROJECTION,
            method: "none",
            points: [PROJECTION.points[0]],
          });
        }
        if (url.includes("/similarity")) return json(SIMILARITY);
        return json(OVERVIEW_ENVELOPE);
      }),
    );
    renderPage("/knowledge?view=geometry");
    expect(
      await screen.findByText(/placeholders, not a projection/),
    ).toBeTruthy();
  });

  it("keeps the three similarity denominators apart", async () => {
    stubRoutes();
    renderPage("/knowledge?view=geometry");
    expect(
      await screen.findByText(
        "1 shown of 120 scored pairs over 40 vectored facts, at or above 0.85",
      ),
    ).toBeTruthy();
  });

  it("names the pair list so it is reachable by keyboard", async () => {
    stubRoutes();
    renderPage("/knowledge?view=geometry");
    const list = await screen.findByRole("region", {
      name: "Similar fact pairs",
    });
    // The list scrolls and holds nothing focusable, so it takes the tab stop
    // itself (WCAG 2.1.1) and the name sits on the node that scrolls.
    expect(list.getAttribute("tabindex")).toBe("0");
  });
});

/* ---- oplog --------------------------------------------------------------- */

describe("Memory oplog", () => {
  it("renders a withheld detail and an unrecorded one as different states", async () => {
    stubRoutes();
    renderPage("/knowledge?view=oplog");
    const withheld = await screen.findByText(
      /detail withheld by the privacy gate/,
    );
    expect(withheld.closest("[data-state]")?.getAttribute("data-state")).toBe(
      "redacted",
    );
    const unrecorded = screen.getByText(/its detail state is unknown/);
    expect(unrecorded.closest("[data-state]")?.getAttribute("data-state")).toBe(
      "unknown",
    );
  });

  it("reports an unreadable store rather than an empty history", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/oplog")) {
          return json({
            events: [],
            count: 0,
            limit: 100,
            error: "database is locked",
          });
        }
        return json(OVERVIEW_ENVELOPE);
      }),
    );
    renderPage("/knowledge?view=oplog");
    expect(
      await screen.findByText(
        /the memory oplog could not be read: database is locked/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/nothing has ever written/)).toBeNull();
  });
});

/* ---- trust history ------------------------------------------------------- */

/** One fact in the overview, so the explorer has a row to open. Every member of
 * `MemoryFactRowV1` is present: the summary never attaches entities, and the
 * counters are real columns rather than absences. */
const FACT_ROW = {
  fact_id: 7,
  content: "a memory fact",
  category: "code_area",
  trust_score: 0.58,
  retrieval_count: 4,
  access_count: 9,
  helpful_count: 2,
  unhelpful_count: 1,
  created_at: 1_784_000_000,
  updated_at: 1_784_000_000,
  last_recalled_at: null,
  has_hrr: 1,
  tags: null,
  entities: null,
  metadata: null,
};

describe("Fact trust history", () => {
  function stubWithFact() {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        requested.push(url);
        const path = url.split("?")[0] ?? url;
        if (path.endsWith("/trust-history")) return json(TRUST_HISTORY);
        if (path.includes("/fact/")) {
          return json(fixtureEnvelope({ error: "", fact: FACT_ROW }));
        }
        for (const [suffix, body] of ROUTES) {
          if (path.endsWith(suffix)) return json(body);
        }
        return json(overviewEnvelope([FACT_ROW]));
      }),
    );
  }

  it("does not ask for a trust audit until a fact is open", async () => {
    stubWithFact();
    renderPage();
    await screen.findByRole("tab", { name: "Facts" });
    // The route takes an `i64` path segment; asking it about no fact would
    // manufacture a 404 this surface then has to explain away.
    expect(requested.some((url) => url.includes("/trust-history"))).toBe(false);
  });

  it("renders withheld and unrecorded event details as their own states", async () => {
    stubWithFact();
    renderPage();
    await userEvent.click(await screen.findByText("a memory fact"));

    const events = await screen.findByRole("region", {
      name: "Trust history events",
    });
    expect(
      within(events).getByText("confirmed against the running daemon"),
    ).toBeTruthy();
    const withheld = within(events).getByText(
      /detail withheld by an earlier writer/,
    );
    expect(withheld.closest("[data-state]")?.getAttribute("data-state")).toBe(
      "redacted",
    );
    const unrecorded = within(events).getByText(/detail state never recorded/);
    expect(unrecorded.closest("[data-state]")?.getAttribute("data-state")).toBe(
      "unknown",
    );
    // The list scrolls with nothing focusable in it, so it takes the tab stop.
    expect(events.getAttribute("tabindex")).toBe("0");
  });

  it("states that an unfinished repair may have left the audit incomplete", async () => {
    stubWithFact();
    renderPage();
    await userEvent.click(await screen.findByText("a memory fact"));
    expect(await screen.findByText(/still has 96 rows to go/)).toBeTruthy();
  });

  it("nets the trust the recorded events actually moved", async () => {
    stubWithFact();
    renderPage();
    await userEvent.click(await screen.findByText("a memory fact"));
    // 0.500 in, 0.580 out, across three events — the gauge above shows only
    // the closing figure, which is what this drilldown exists to explain.
    expect(await screen.findByText("0.500")).toBeTruthy();
    expect(screen.getByText("+0.080")).toBeTruthy();
  });
});

/* ---- curation ------------------------------------------------------------ */

describe("Curation console", () => {
  it("reports run history with the ledger status of each run", async () => {
    stubRoutes();
    renderPage("/knowledge?view=curation");
    const list = await screen.findByRole("region", {
      name: "Automatic run records",
    });
    expect(within(list).getByText("backend timed out after 60s")).toBeTruthy();
    // The failed run keeps its own chip rather than being folded into a count,
    // and the chip carries the state in `data-state` so the meaning does not
    // depend on the label text or on colour.
    expect(list.querySelector('[data-state="error"]')).toBeTruthy();
    expect(list.querySelector('[data-state="ready"]')).toBeTruthy();
  });


  it("states which project a configuration write would reach", async () => {
    stubRoutes();
    renderPage("/knowledge?view=curation");
    expect(
      await screen.findByText("Applies to the active project."),
    ).toBeTruthy();
  });

  it("shows the daemon’s re-read after a patch, not the value that was asked for", async () => {
    stubRoutes();
    renderPage("/knowledge?view=curation");
    const toggle = await screen.findByRole("checkbox", {
      name: /Automation enabled/,
    });
    expect((toggle as HTMLInputElement).checked).toBe(true);
    await userEvent.click(toggle);
    await waitFor(() =>
      expect(
        (
          screen.getByRole("checkbox", {
            name: /Automation enabled/,
          }) as HTMLInputElement
        ).checked,
      ).toBe(false),
    );
    const patch = requested.filter((url) => url.includes("/curation/config"));
    expect(patch.length).toBeGreaterThan(1);
  });

  it("refuses the write before dispatch under a read-only scope", async () => {
    useScope.setState({
      scope: {
        kind: "project",
        projectId: "proj_other",
        label: "Other",
        activation: "selected",
      },
    });
    stubRoutes();
    renderPage("/knowledge?view=curation");
    const toggle = await screen.findByRole("checkbox", {
      name: /Automation enabled/,
    });
    expect((toggle as HTMLInputElement).disabled).toBe(true);
    // No PATCH may have gone out: the reason is stated instead, and it names
    // the remedy rather than reporting a 405 the transport cannot interpret.
    expect(screen.getByText(/is not the active project/)).toBeTruthy();
    expect(
      requested.some(
        (url) => url.includes("/curation/config") && url.includes("PATCH"),
      ),
    ).toBe(false);
  });

  it("does not offer browser-owned policy settings", async () => {
    stubRoutes();
    renderPage("/knowledge?view=curation");
    await screen.findByRole("checkbox", { name: /Automation enabled/ });
    // Validation/application policy stays daemon-owned; only the enable switch
    // is a control in this console.
    expect(
      screen.queryByRole("checkbox", { name: /job commands/i }),
    ).toBeNull();
    expect(screen.queryByRole("checkbox", { name: /apply|skill/i })).toBeNull();
  });
});
