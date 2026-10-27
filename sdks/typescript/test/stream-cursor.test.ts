import { describe, expect, it } from "vitest";
import {
  createClient,
  TraceDecayMalformedResponseError,
  type OperationStreamEvent,
  type OperationStreamOptions,
} from "../src/client";

const receipt = {
  started_at: 10,
  ended_at: 20,
  effective_deadline: { expires_at: 30 },
  cancellation: null,
  budget: { units_consumed: 1, bytes_consumed: 2, elapsed_micros: 3 },
  termination: "completed",
};

function frame(event: string, data: Record<string, unknown>, id?: number): string {
  return `event: ${event}\n${id === undefined ? "" : `id: ${id}\n`}data: ${JSON.stringify({ event, data })}\n\n`;
}

function open(nextSequence: number, retained = 0, token = "resume.initial"): string {
  return frame("open", {
    correlation_id: "request.operation",
    frontier: { next_sequence: nextSequence, retained_from_sequence: retained, resume_token: token },
  });
}

function item(sequence: number): string {
  return frame("item", { sequence, item: { kind: "accepted" } }, sequence);
}

function terminal(sequence: number): string {
  return frame("completed", { sequence, terminal: { termination: "completed", receipt } }, sequence);
}

function gap(sequence: number, last: number, next = 8, retained = last + 1, first = sequence): string {
  return frame("resume_gap", {
    sequence,
    gap: {
      first_missing_sequence: first,
      last_missing_sequence: last,
      frontier: { next_sequence: next, retained_from_sequence: retained, resume_token: "resume.gap" },
    },
  }, sequence);
}

function clientFor(bodies: readonly string[]) {
  const requests: URL[] = [];
  const client = createClient({
    baseUrl: "http://127.0.0.1:1",
    projectId: "project.sdk",
    token: "isolated-fixture-token",
    fetch: async (input) => {
      requests.push(new URL(String(input)));
      const body = bodies[requests.length - 1];
      if (body === undefined) throw new Error("unexpected reconnect");
      return new Response(body, { headers: { "content-type": "text/event-stream" } });
    },
  });
  const collect = async (options: OperationStreamOptions = {}): Promise<OperationStreamEvent[]> => {
    const events: OperationStreamEvent[] = [];
    for await (const event of client.streamOperation("request.operation", options)) events.push(event);
    return events;
  };
  return { requests, collect };
}

describe("operation delivery cursors", () => {
  it("delivers retained events from zero despite a later publication watermark", async () => {
    const { collect, requests } = clientFor([open(3) + item(0) + item(1) + terminal(2)]);
    const events = await collect();
    expect(events.map((event) => event.id)).toEqual([null, "0", "1", "2"]);
    expect(events[0]?.data.data).toMatchObject({ frontier: { next_sequence: 3 } });
    expect(requests).toHaveLength(1);
  });

  it("keeps zero through two open-only connections and refreshes resume authority", async () => {
    const { collect, requests } = clientFor([
      open(3, 0, "resume.first"),
      open(3, 0, "resume.second"),
      open(3, 0, "resume.third") + item(0) + item(1) + terminal(2),
    ]);
    const events = await collect({ maxReconnects: 2 });
    expect(events.filter((event) => event.id !== null).map((event) => event.id)).toEqual(["0", "1", "2"]);
    expect(requests.map((url) => url.searchParams.get("next_sequence"))).toEqual([null, "0", "0"]);
    expect(requests.map((url) => url.searchParams.get("resume_token"))).toEqual([null, "resume.first", "resume.second"]);
  });

  it("keeps the last delivered position when a reconnect ends after open", async () => {
    const { collect, requests } = clientFor([
      open(3) + item(0),
      open(3),
      open(3) + item(1) + terminal(2),
    ]);
    const events = await collect({ maxReconnects: 2 });
    expect(events.filter((event) => event.id !== null).map((event) => event.id)).toEqual(["0", "1", "2"]);
    expect(requests.map((url) => url.searchParams.get("next_sequence"))).toEqual([null, "1", "1"]);
  });

  it("keeps an explicit caller cursor until a retained event is delivered", async () => {
    const { collect, requests } = clientFor([open(4), open(4) + item(2) + terminal(3)]);
    const events = await collect({ resume: { nextSequence: 2, resumeToken: "resume.caller" }, maxReconnects: 1 });
    expect(events.filter((event) => event.id !== null).map((event) => event.id)).toEqual(["2", "3"]);
    expect(requests.map((url) => url.searchParams.get("next_sequence"))).toEqual(["2", "2"]);
  });

  it("advances through the entire explicit gap without adopting its publication watermark", async () => {
    const { collect, requests } = clientFor([
      open(8, 5) + gap(0, 4),
      open(8, 5),
      open(8, 5) + item(5) + item(6) + terminal(7),
    ]);
    const events = await collect({ maxReconnects: 2 });
    expect(events.filter((event) => event.id !== null).map((event) => event.id)).toEqual(["0", "5", "6", "7"]);
    expect(requests.map((url) => url.searchParams.get("next_sequence"))).toEqual([null, "5", "5"]);
    expect(requests[1]?.searchParams.get("resume_token")).toBe("resume.gap");
  });

  it("delivers the canonical terminal replay for a cursor one past completion", async () => {
    const { collect, requests } = clientFor([open(3) + terminal(2)]);
    const events = await collect({ resume: { nextSequence: 3, resumeToken: "resume.completed" }, maxReconnects: 2 });
    expect(events.map((event) => event.event)).toEqual(["open", "completed"]);
    expect(events[1]?.id).toBe("2");
    expect(requests).toHaveLength(1);
  });

  it("refuses a first event that skips undelivered history without a gap", async () => {
    const { collect, requests } = clientFor([open(4) + item(2) + terminal(3)]);
    await expect(collect({ maxReconnects: 2 })).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(requests).toHaveLength(1);
  });

  it("does not accept a terminal older than the caller's last consumed event", async () => {
    const { collect, requests } = clientFor([open(4) + terminal(1)]);
    await expect(collect({ resume: { nextSequence: 4, resumeToken: "resume.caller" }, maxReconnects: 2 }))
      .rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(requests).toHaveLength(1);
  });

  it.each([
    ["a gap that starts outside its enclosing sequence", gap(0, 4, 8, 5, 1)],
    ["a reversed gap", gap(4, 3, 8, 5)],
    ["a gap beyond published history", gap(0, 8, 8, 5)],
    ["an inconsistent gap frontier", gap(0, 4, 8, 9)],
  ])("refuses %s without reconnecting", async (_label, invalid) => {
    const { collect, requests } = clientFor([open(8) + invalid + terminal(7)]);
    await expect(collect({ maxReconnects: 2 })).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(requests).toHaveLength(1);
  });

  it("refuses an inconsistent opening frontier before delivering it", async () => {
    const { collect, requests } = clientFor([open(3, 4) + terminal(0)]);
    await expect(collect({ maxReconnects: 2 })).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(requests).toHaveLength(1);
  });
});
