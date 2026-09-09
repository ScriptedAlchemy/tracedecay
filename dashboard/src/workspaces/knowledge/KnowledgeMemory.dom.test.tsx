/**
 * The Knowledge workspace's memory views, over the routes the daemon mounts.
 *
 * Two invariants carry this file.
 *
 * The first is the camera: four positions over one store, the position living
 * in the address so a link reopens it, and switching never fetching a view's
 * reads until that view is looked at.
 *
 * The second is the state taxonomy against supplied feedback state. Canonical
 * `redacted` and `unknown` details reach the screen as their own chips rather
 * than as blank cells.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  MemoryFactRowV1,
  MemoryGraphPayloadV1,
} from "../../contracts/generated.ts";
import { fixtureEnvelope } from "../../test/fixtureEnvelope.ts";
import { KnowledgePage } from "./KnowledgePage.tsx";

/* ---- route bodies -------------------------------------------------------- */

function memoryGraph(facts: readonly MemoryFactRowV1[]): MemoryGraphPayloadV1 {
  const graphRoots = facts.filter(
    (fact) => fact.payload_access === "eligible",
  );
  const unavailableFactCandidates = facts.length - graphRoots.length;
  const coverage: MemoryGraphPayloadV1["coverage"] =
    unavailableFactCandidates === 0
      ? {
          completeness: "complete",
          eligible: graphRoots.length,
          examined: graphRoots.length,
          matched: graphRoots.length,
          excluded: 0,
          omitted: 0,
          unknown: 0,
          denominator: graphRoots.length,
          unit: "memory_graph_roots",
          omission_reasons: [],
        }
      : {
          completeness: "unknown",
          eligible: null,
          examined: null,
          matched: null,
          excluded: null,
          omitted: null,
          unknown: null,
          denominator: null,
          unit: null,
          omission_reasons: ["unavailable_fact_roots"],
        };
  return {
    nodes: graphRoots.map((fact) => ({
      id: `fact:${fact.fact_id}`,
      kind: "fact",
      label: fact.content === null ? fact.fact_id : fact.content,
      fact_id: fact.fact_id,
      payload_access: fact.payload_access,
      projected_as_of: fact.projected_as_of,
      content: fact.content,
      category: fact.category,
      trust_score: fact.trust_score,
      retrieval_count: fact.retrieval_count,
      helpful_count: fact.helpful_count,
    })),
    edges: [],
    coverage,
    fact_universe_count: facts.length,
    fact_candidates_examined: facts.length,
    unavailable_fact_candidates: unavailableFactCandidates,
    root_count: graphRoots.length,
    relation_limit: 100,
    relation_count: 0,
  };
}

/** `memory_api::overview` seeds the whole holographic block before it reads
 * anything, so `reads` and `facts_coverage` are always present — a body without
 * them is one the route cannot produce. */
function overviewEnvelope(facts: readonly MemoryFactRowV1[] = []) {
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
      graph: memoryGraph(facts),
      facts_coverage: {
        completeness: "partial",
        limit: 100,
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
  coverage: {
    completeness: "complete",
    examined: 3,
    limit: 400,
    omission_reasons: [],
  },
  points: [
    projectionPoint("fact-project-1", -1, 0.5, "decision"),
    projectionPoint("fact-project-2", 1.5, -0.25, "decision"),
    projectionPoint("fact-project-3", 0.25, 2, "code_area"),
  ],
};

const SIMILARITY = {
  exists: true,
  dim: 64,
  count: 40,
  limit: 1,
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
      a_id: "fact-project-11",
      b_id: "fact-project-12",
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
  fact_id: "fact-project-7",
  limit: 300,
  completeness: "complete",
  next_after: null,
  error: "",
  trust_history: [
    {
      event_id: "event-project-1",
      timestamp: 1_754_006_400_000_000,
      action: "helpful",
      old_trust: 0.5,
      new_trust: 0.62,
      delta: 0.12,
      details_availability: "available",
      note: "confirmed against the running daemon",
    },
    {
      event_id: "event-project-2",
      timestamp: 1_754_092_800_000_000,
      action: "unhelpful",
      old_trust: 0.62,
      new_trust: 0.51,
      delta: -0.11,
      details_availability: "redacted",
    },
    {
      event_id: "event-project-3",
      timestamp: 1_754_179_200_000_000,
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
      ts: 1_754_179_200_000_000,
      op: "created",
      fact_id: "fact-project-9",
    },
    {
      id: 2,
      ts: 1_754_092_800_000_000,
      op: "created",
      fact_id: "fact-project-8",
    },
    {
      id: 1,
      ts: 1_754_006_400_000_000,
      op: "payload_access_changed",
      fact_id: null,
    },
  ],
};

const RUNS = {
  count: 2,
  limit: 50,
  has_more: false,
  malformed_row_count: 0,
  completeness: "known",
  error: "",
  runs: [
    {
      run_id: "run-a",
      trigger: "scheduler",
      task: "memory_curator",
      backend: "codex_app_server",
      model: "gpt-5-codex",
      status: "succeeded",
      reviewed_count: 5,
      accepted_count: 3,
      rejected_count: 2,
      skipped_count: 0,
      error: null,
      started_at: "2026-08-01T00:00:00Z",
      completed_at: "2026-08-01T00:01:00Z",
      artifact_kinds: [],
    },
    {
      run_id: "run-b",
      trigger: "manual",
      task: "skill_writer",
      backend: "codex_app_server",
      model: null,
      status: "failed",
      reviewed_count: 0,
      accepted_count: 0,
      rejected_count: 0,
      skipped_count: 0,
      started_at: "2026-08-02T00:00:00Z",
      completed_at: "2026-08-02T00:00:30Z",
      error: "backend timed out after 60s",
      artifact_kinds: [],
    },
  ],
};

function projectionPoint(
  factId: string,
  x: number,
  y: number,
  category: string,
) {
  return {
    fact_id: factId,
    payload_access: "eligible",
    x,
    y,
    category,
    content: `fact ${factId}`,
    trust_score: 0.7,
    retrieval_count: 2,
    access_count: 3,
    helpful_count: 1,
    unhelpful_count: 0,
    created_at: 1_754_006_400_000_000,
    updated_at: 1_754_006_400_000_000,
    projected_as_of: 1_754_006_400_000_000,
    last_recalled_at: null,
    tags: [],
    entities: [],
    metadata: {},
    entity_count: 1,
  };
}

/* ---- harness ------------------------------------------------------------- */

/** Route bodies by the path suffix that identifies them. */
const ROUTES: readonly (readonly [string, unknown])[] = [
  ["/trust-history", TRUST_HISTORY],
  ["/projection", PROJECTION],
  ["/similarity", SIMILARITY],
  ["/automation/runs", RUNS],
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

function stubRoutes() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requested.push(url);
      const path = url.split("?")[0] ?? url;
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
    expect(await screen.findByText("fact-project-9")).toBeTruthy();
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

  it("surfaces bounded projection coverage instead of implying the page is the whole store", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/projection")) {
          return json({
            ...PROJECTION,
            coverage: {
              completeness: "bounded",
              examined: 400,
              limit: 400,
              omission_reasons: ["request_limit_reached"],
            },
          });
        }
        if (url.includes("/similarity")) return json(SIMILARITY);
        return json(OVERVIEW_ENVELOPE);
      }),
    );
    renderPage("/knowledge?view=geometry");
    expect(
      await screen.findByText(/projection coverage is bounded; examined 400 under a limit of 400/i),
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

  it("does not call an incoherent oplog response complete", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/oplog")) {
          return json({
            events: [],
            count: 2,
            limit: 100,
            error: "",
          });
        }
        return json(OVERVIEW_ENVELOPE);
      }),
    );
    renderPage("/knowledge?view=oplog");

    expect(
      await screen.findByText(/the store counted 2 operations but sent 0/),
    ).toBeTruthy();
    expect(screen.queryByText(/nothing has ever written/)).toBeNull();
  });
});

/* ---- trust history ------------------------------------------------------- */

/** One fact in the overview, so the explorer has a row to open. Every member of
 * `MemoryFactRowV1` is present: the summary never attaches entities, and the
 * counters are real columns rather than absences. */
const FACT_ROW: MemoryFactRowV1 = {
  fact_id: "fact-project-7",
  payload_access: "eligible",
  content: "a memory fact",
  category: "code_area",
  trust_score: 0.58,
  retrieval_count: 4,
  access_count: 9,
  helpful_count: 2,
  unhelpful_count: 1,
  created_at: 1_784_000_000_000_000,
  updated_at: 1_784_000_000_000_000,
  last_recalled_at: null,
  projected_as_of: 1_784_000_000_000_000,
  tags: [],
  entities: [],
  metadata: {},
  source_label: null,
  linked_entities: null,
};

describe("Fact trust history", () => {
  function stubWithFact(history: unknown = TRUST_HISTORY) {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        requested.push(url);
        const path = url.split("?")[0] ?? url;
        if (path.endsWith("/trust-history")) return json(history);
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
    // The route takes a canonical fact-ID path segment; asking it about no fact would
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
      /feedback detail withheld/,
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

  it("labels partial trust arithmetic and preserves the continuation state", async () => {
    stubWithFact({
      ...TRUST_HISTORY,
      completeness: "partial",
      next_after: {
        occurred_at: 1_754_179_200_000_000,
        event_id: "event-project-3",
      },
    });
    renderPage();
    await userEvent.click(await screen.findByText("a memory fact"));

    expect(await screen.findByText(/this is a partial history window/i)).toBeTruthy();
    expect(screen.getByText("window opening")).toBeTruthy();
    expect(screen.getByText("window net")).toBeTruthy();
    expect(screen.getByText("window closing")).toBeTruthy();
  });
});
