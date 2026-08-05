import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fixtureEnvelope } from "../../test/fixtureEnvelope.ts";
import { SessionInspector } from "./SessionInspector.tsx";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Session transcript drill-down", () => {
  it("reports canonical temporal retrieval as unavailable without rendering raw turns", async () => {
    const response = fixtureEnvelope(null, "unknown");
    response.coverage = {
      completeness: "unknown",
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: "records",
      omission_reasons: ["lcm_temporal_retrieval_not_mounted"],
    };
    renderInspector(response);

    expect(await screen.findByText("Unknown")).toBeTruthy();
    expect(
      await screen.findByText(/lcm_temporal_retrieval_not_mounted/),
    ).toBeTruthy();
    expect(screen.queryByText("assistant")).toBeNull();
    expect(screen.queryByText(/raw messages/)).toBeNull();
  });

  it("uses the daemon temporal cursor for the next transcript page", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      return new Response(
        JSON.stringify(
          fixtureEnvelope(
            sessionPayload(
              url.includes("cursor=cursor-next") ? null : "cursor-next",
            ),
          ),
        ),
        { status: 200 },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    renderWith();

    fireEvent.click(await screen.findByRole("button", { name: "Next page" }));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining("cursor=cursor-next"),
        expect.anything(),
      );
    });
  });
});

function sessionPayload(nextCursor: string | null) {
  return {
    path: "daemon://session-temporal",
    storage_scope: "project",
    exists: true,
    session_id: "claude:035c8f3c",
    limit: 100,
    counts: {
      message_count: 101,
      summary_node_count: 0,
      token_estimate_total: 10,
      summary_token_count: 0,
      source_token_count: 10,
    },
    messages: [],
    summary_nodes: [],
    has_more: nextCursor != null,
    has_more_messages: nextCursor != null,
    has_more_summary_nodes: false,
    next_cursor: nextCursor,
  };
}

function renderInspector(payload: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <SessionInspector sessionId="claude:035c8f3c" onClose={() => {}} />
    </QueryClientProvider>,
  );
}
