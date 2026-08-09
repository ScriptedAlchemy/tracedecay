/**
 * Where a scheduler control's answer is allowed to land.
 *
 * The control writes the daemon's post-change reading straight into the status
 * cache, which is what lets the badge update from a measurement instead of an
 * assumption. The question this file settles is *whose* cache entry that is.
 *
 * A pause takes a round trip, and a reader can change project inside it. The
 * hook derives its key from the scope of the render it last ran in, and React
 * Query calls settlement callbacks from the current options — so unless the
 * dispatch scope is captured when the request goes out, project A's answer
 * settles against project B's key. The two ways that shows up are both here:
 * A's reading written into B's entry, and B's entry invalidated because A's
 * write failed. Neither is visible as an error; both make one project's panel
 * answer for another's.
 */
import { act, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createElement, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  automationSchedulerKey,
  schedulerStatusUrl,
  useSchedulerControl,
} from "./automation.ts";
import { payloadQueryKey, usePayload } from "./usePayload.ts";
import {
  UNSCOPED_CACHE_KEY,
  useScope,
  type DashboardScope,
} from "../scope/store.ts";
import { AutomationSchedulerStatusV1Schema } from "../../contracts/generated.ts";

function activeProject(projectId: string, label: string): DashboardScope {
  return { kind: "project", projectId, label, activation: "active" };
}

const PROJECT_A = activeProject("proj_a", "Project A");
const PROJECT_B = activeProject("proj_b", "Project B");
const ALL_PROJECTS: DashboardScope = { kind: "all" };

/**
 * The entry the page's own status read occupies, from the authority that builds
 * it rather than from a second construction of it.
 *
 * Restating the key here as `[...automationSchedulerKey, scopeKey(scope)]` is
 * what let this suite pass while the surface was broken: it repeated the
 * writer's mistake, and both fixtures were selected projects, where
 * `scopeKey` and `requestScopeKey` return the same token. The disagreement
 * only exists under the all-projects default, which nothing here reached.
 */
function statusKeyFor(scope: DashboardScope): unknown[] {
  return [
    ...payloadQueryKey(scope, automationSchedulerKey, schedulerStatusUrl),
  ];
}

/** A scheduler body the contract accepts, distinguishable per project. */
function schedulerBody(paused: boolean) {
  const now = Math.floor(Date.now() / 1000);
  return {
    status: "configured",
    paused,
    enabled: true,
    scheduler_tick_secs: 900,
    now,
    last_session_activity: null,
    configuration_revision_id: "configuration.revision.scope.test",
    control_path: "/x/automation.control.json",
    tasks: [
      {
        task: "memory_curator",
        due: false,
        skip_reason: "scheduler_paused",
        last_scheduler_run: null,
      },
    ],
  };
}

/** The request the test holds open, so the scope can change mid-flight. */
let release: (() => void) | null = null;

/** Answer the in-flight control once the test says so. */
function stubHeldControl(respond: () => Response): void {
  release = null;
  vi.stubGlobal(
    "fetch",
    vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          release = () => resolve(respond());
        }),
    ),
  );
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

let client: QueryClient;

function wrapper({ children }: { children: ReactNode }) {
  return createElement(QueryClientProvider, { client }, children);
}

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  useScope.setState({ scope: PROJECT_A });
});

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
  release = null;
});

describe("a scheduler control answered after the reader changed project", () => {
  it("writes the reading into the project it was dispatched to, not the one on screen", async () => {
    // Project B already holds a reading of its own. If the late answer for A
    // lands here, B's panel reports A's scheduler.
    const bReading = { outcome: "ok" as const, data: schedulerBody(false) };
    client.setQueryData(statusKeyFor(PROJECT_B), bReading);

    stubHeldControl(() => jsonResponse(200, schedulerBody(true)));
    const { result, rerender } = renderHook(() => useSchedulerControl(), {
      wrapper,
    });

    act(() => result.current.mutate(true));
    await waitFor(() => expect(release).not.toBeNull());

    // The reader switches project while the pause is still in flight.
    act(() => useScope.setState({ scope: PROJECT_B }));
    rerender();

    act(() => release?.());
    await waitFor(() => expect(result.current.isPending).toBe(false));

    // B is untouched, still holding its own reading.
    expect(client.getQueryData(statusKeyFor(PROJECT_B))).toEqual(bReading);
    // A received the answer to A's request.
    const landed = client.getQueryData(statusKeyFor(PROJECT_A)) as
      { outcome: string; data: { paused: boolean } } | undefined;
    expect(landed?.outcome).toBe("ok");
    expect(landed?.data.paused).toBe(true);
  });

  it("invalidates the dispatched project after a failure, leaving the new one alone", async () => {
    // The other half of the same defect: a failed control re-reads, and the
    // re-read must be of the project that was asked, not the one now on
    // screen. `staleTime: Infinity` makes an invalidation observable as the
    // entry becoming stale — nothing else would mark it so.
    client.setQueryData(statusKeyFor(PROJECT_A), {
      outcome: "ok",
      data: schedulerBody(false),
    });
    client.setQueryData(statusKeyFor(PROJECT_B), {
      outcome: "ok",
      data: schedulerBody(false),
    });
    for (const scope of [PROJECT_A, PROJECT_B]) {
      const entry = client
        .getQueryCache()
        .find({ queryKey: statusKeyFor(scope) });
      expect(entry?.isStale()).toBe(false);
    }

    stubHeldControl(() =>
      jsonResponse(500, { detail: "scheduler unavailable" }),
    );
    const { result, rerender } = renderHook(() => useSchedulerControl(), {
      wrapper,
    });

    act(() => result.current.mutate(true));
    await waitFor(() => expect(release).not.toBeNull());

    act(() => useScope.setState({ scope: PROJECT_B }));
    rerender();

    act(() => release?.());
    await waitFor(() => expect(result.current.isPending).toBe(false));

    expect(
      client
        .getQueryCache()
        .find({ queryKey: statusKeyFor(PROJECT_A) })
        ?.isStale(),
    ).toBe(true);
    expect(
      client
        .getQueryCache()
        .find({ queryKey: statusKeyFor(PROJECT_B) })
        ?.isStale(),
    ).toBe(false);
  });
});

/**
 * The default scope, which is where the two key constructions differed.
 *
 * Under `all`, the scheduler route still carries the all-projects scope even
 * though its URL is not rewritten. The read and writer must therefore share
 * the `all` entry and must not leave behind the obsolete unscoped entry.
 *
 * Asserted through the read hook rather than against a key literal: what has to
 * hold is that the writer and the page's own read address the same entry, and a
 * test that names the key itself can agree with a writer that is wrong.
 */
describe("a scheduler control under the all-projects default", () => {
  it("writes into the entry the page's own status read occupies", async () => {
    useScope.setState({ scope: ALL_PROJECTS });
    stubHeldControl(() => jsonResponse(200, schedulerBody(true)));

    const { result } = renderHook(
      () => ({
        control: useSchedulerControl(),
        read: usePayload(
          automationSchedulerKey,
          schedulerStatusUrl,
          AutomationSchedulerStatusV1Schema,
        ),
      }),
      { wrapper },
    );

    act(() => result.current.control.mutate(true));
    await waitFor(() => expect(release).not.toBeNull());
    act(() => release?.());
    await waitFor(() => expect(result.current.control.isPending).toBe(false));

    // The reading the daemon took after the change, visible to the read that
    // renders the badge.
    await waitFor(() => {
      const read = result.current.read.data;
      expect(read?.outcome).toBe("ok");
      expect(read?.outcome === "ok" ? read.data.paused : null).toBe(true);
    });
    // And no orphan: the writer did not also populate the old unscoped entry.
    expect(
      client.getQueryData([...automationSchedulerKey, UNSCOPED_CACHE_KEY]),
    ).toBeUndefined();
  });
});
