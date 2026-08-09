import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import {
  allRoutesFail,
  faultHandler,
  fixtureServer,
  type HttpFault,
} from "../../../stories/fixtures/handlers.ts";
import { AutomationsPage } from "./AutomationsPage.tsx";

/**
 * HTTP fault injection for the Automations workspace (plan 11, "MSW covers
 * HTTP/SSE faults").
 *
 * Automations is the subject for read isolation because it renders five
 * independent reads side by side, and because four of its five panels have an
 * empty state written in plain English — "no automation jobs defined", "no
 * managed skills", "no fact application outcomes are recorded", "no automation runs are
 * recorded". Those sentences are the exact
 * fabrication this project forbids: a queue nobody could read must never
 * present as a queue that was read and found empty. Every case below asserts
 * they are absent, not merely that some error appeared.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

/** The empty-success copy each panel prints for a queue it really did read. */
const EMPTY_COPY = [
  /no automation jobs defined/i,
  /no managed skills/i,
  /no fact application outcomes are recorded/i,
  /no automation runs are recorded/i,
] as const;

const FAULTS: ReadonlyArray<{
  fault: HttpFault;
  kind: string;
  detail: string | null;
}> = [
  { fault: "server_error", kind: "error", detail: "HTTP 500" },
  { fault: "not_found", kind: "error", detail: "HTTP 404" },
  // The two refusals are their own states, so neither carries a status code:
  // the chip label and its guidance already say which refusal happened.
  { fault: "unauthorized", kind: "unauthorized", detail: null },
  { fault: "forbidden", kind: "denied", detail: null },
  { fault: "network_error", kind: "offline", detail: null },
  { fault: "malformed_body", kind: "unsupported_schema", detail: null },
  { fault: "unsupported_shape", kind: "unsupported_schema", detail: null },
];

describe("AutomationsPage under HTTP transport faults", () => {
  it.each(FAULTS)(
    "reports $fault on every panel instead of five empty queues",
    async ({ fault, kind, detail }) => {
      server.use(allRoutesFail(fault));
      const { container } = renderAutomations();

      // All five reads fail the same way, so all five panels say so.
      const chips = await settledChips(container);
      expect(chips).toHaveLength(5);
      for (const chip of chips) {
        expect(chip.getAttribute("data-state")).toBe(kind);
        if (detail) expect(chip.textContent).toContain(detail);
      }

      // Not one of the panels claims to have read an empty queue.
      for (const copy of EMPTY_COPY)
        expect(screen.queryByText(copy)).toBeNull();
      // Scheduler receipts are not review counters; a failed read must not
      // leave a revision/tick reading behind as if it were current.
      expect(screen.queryByText("configuration revision")).toBeNull();
      expect(screen.queryByText("tick interval")).toBeNull();
    },
  );

  it.each(FAULTS)(
    "keeps a $fault on the jobs route inside the jobs panel",
    async ({ fault, kind, detail }) => {
      // Only the jobs read fails; its two neighbours answer from the fixtures.
      // This is the case where a fabricated empty state would be hardest to
      // spot, because the surrounding panels look perfectly healthy.
      server.use(faultHandler("*/api/automation/jobs", fault));
      renderAutomations();

      const jobs = await screen.findByRole("region", { name: "Jobs" });
      const chip = await within(jobs).findByText(
        /Error|Offline|Unauthorized|Denied|Unsupported schema/,
      );
      const chipHost = chip.closest("[data-state]");
      expect(chipHost?.getAttribute("data-state")).toBe(kind);
      if (detail) expect(chipHost?.textContent).toContain(detail);
      expect(
        within(jobs).queryByText(/no automation jobs defined/i),
      ).toBeNull();
      // The fixtures serve four jobs; none of their names may survive a failed
      // read as stale content presented as current.
      expect(within(jobs).queryByText("Memory curator")).toBeNull();

      // The neighbours are untouched and still render their real rows, so the
      // failure is scoped to the read that actually failed.
      const skills = screen.getByRole("region", { name: "Managed skills" });
      expect(within(skills).getByText("Code Slop Cleanup")).toBeTruthy();
      expect(within(skills).queryByText(/no managed skills/i)).toBeNull();
      expect(skills.querySelector("[data-state]")).toBeNull();

      const receipts = screen.getByRole("region", {
        name: "Fact application outcomes",
      });
      expect(
        within(receipts).queryByText(
          /no fact application outcomes are recorded/i,
        ),
      ).toBeNull();
      expect(receipts.querySelector("[data-state]")).toBeNull();
    },
  );
});

/** Every state chip on the page once no read is still in flight. The loading
 * state renders the same panel title as the settled one, so waiting on the
 * title would sample the page mid-fetch and pass against `loading`. */
async function settledChips(container: HTMLElement): Promise<Element[]> {
  await waitFor(() => {
    expect(container.querySelectorAll('[data-state="loading"]')).toHaveLength(
      0,
    );
    expect(container.querySelectorAll("[data-state]").length).toBeGreaterThan(
      0,
    );
  });
  return [...container.querySelectorAll("[data-state]")];
}

function renderAutomations() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <AutomationsPage />
    </QueryClientProvider>,
  );
}
