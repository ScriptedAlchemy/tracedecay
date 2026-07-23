import { describe, it, expect } from "vitest";
import { createSseReducer } from "./reducer.ts";
import type { SseEventEnvelope } from "./types.ts";

interface Body {
  n: number;
}

function ev(
  overrides: Partial<{
    stream_id: string;
    generation: number;
    event_id: string;
    event_revision: number;
    entity_revision: number;
    is_receipt: boolean;
    n: number;
  }> = {},
): SseEventEnvelope<Body> {
  const event_revision = overrides.event_revision ?? 1;
  return {
    stream: { stream_id: overrides.stream_id ?? "s1", generation: overrides.generation ?? 1 },
    event_id: overrides.event_id ?? `e${event_revision}`,
    revision: {
      event_revision,
      entity_revision: overrides.entity_revision ?? event_revision,
    },
    scope: "scope-1",
    observation_time: "ot",
    watermark: "wm",
    coverage: null,
    is_receipt: overrides.is_receipt,
    payload: { n: overrides.n ?? event_revision },
  };
}

describe("SSE reducer — ordering", () => {
  it("preserves monotone acceptance order in the batch", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ event_revision: 1 }));
    r.ingest(ev({ event_revision: 2 }));
    r.ingest(ev({ event_revision: 3 }));
    const batch = r.takeBatch();
    expect(batch.events.map((e) => e.revision.event_revision)).toEqual([1, 2, 3]);
    expect(batch.refetch).toBe(false);
    expect(batch.stale).toBe(false);
  });

  it("drops an out-of-order event older than the watermark", () => {
    const r = createSseReducer<Body>();
    expect(r.ingest(ev({ event_revision: 5, event_id: "e5" }))).toBe(true);
    // A late, unseen, lower-revision event is superseded.
    expect(r.ingest(ev({ event_revision: 3, event_id: "e3" }))).toBe(false);
    const batch = r.takeBatch();
    expect(batch.events.map((e) => e.revision.event_revision)).toEqual([5]);
  });
});

describe("SSE reducer — dedupe by stream/event/revision", () => {
  it("drops a duplicate (same stream/event/revision) event", () => {
    const r = createSseReducer<Body>();
    expect(r.ingest(ev({ event_revision: 1, event_id: "e1" }))).toBe(true);
    expect(r.ingest(ev({ event_revision: 1, event_id: "e1" }))).toBe(false);
    expect(r.takeBatch().events).toHaveLength(1);
    expect(r.stats().observedEvents).toBe(1);
  });

  it("treats different streams as independent identities", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ stream_id: "a", event_revision: 1, event_id: "x" }));
    // Same event_id + revision but different stream => not a duplicate.
    expect(r.ingest(ev({ stream_id: "b", event_revision: 1, event_id: "x" }))).toBe(true);
    expect(r.takeBatch().events).toHaveLength(2);
  });
});

describe("SSE reducer — stale-generation rejection", () => {
  it("rejects events from an older generation and accepts newer ones", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ generation: 2, event_revision: 1, event_id: "e1" }));
    // Older generation => rejected.
    expect(r.ingest(ev({ generation: 1, event_revision: 2, event_id: "e2" }))).toBe(false);
    // Newer generation => reconnect, accepted; dedupe watermark preserved.
    expect(r.ingest(ev({ generation: 3, event_revision: 2, event_id: "e2" }))).toBe(true);
    const batch = r.takeBatch();
    expect(batch.events.map((e) => e.revision.event_revision)).toEqual([1, 2]);
    expect(r.stats().generation).toBe(3);
  });

  it("still detects a revision gap across a reconnect", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ generation: 1, event_revision: 1, event_id: "e1" }));
    // Reconnect (gen 2) but revision jumps 1 -> 4 => gap.
    r.ingest(ev({ generation: 2, event_revision: 4, event_id: "e4" }));
    expect(r.takeBatch().refetch).toBe(true);
  });

  it("accepts a restarted revision sequence and refetches on a newer generation", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ generation: 1, event_revision: 7, event_id: "run-1-e7" }));
    r.takeBatch();

    expect(
      r.ingest(ev({ generation: 2, event_revision: 1, event_id: "run-2-e1" })),
    ).toBe(true);
    const batch = r.takeBatch();
    expect(batch.events.map((event) => event.revision.event_revision)).toEqual([1]);
    expect(batch.refetch).toBe(true);
  });
});

describe("SSE reducer — revision gap => refetch once", () => {
  it("emits exactly one refetch signal for a gap and coalesces multiple gaps", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    r.ingest(ev({ event_revision: 5, event_id: "e5" })); // gap
    r.ingest(ev({ event_revision: 9, event_id: "e9" })); // another gap in same batch
    const batch = r.takeBatch();
    expect(batch.refetch).toBe(true);
    // Coalesced: the signal is a single boolean, not per-gap.
    expect(batch.events.map((e) => e.revision.event_revision)).toEqual([1, 5, 9]);
    // Next batch (no new gap) does not re-signal.
    r.ingest(ev({ event_revision: 10, event_id: "e10" }));
    expect(r.takeBatch().refetch).toBe(false);
  });

  it("does not signal a refetch for a contiguous sequence", () => {
    const r = createSseReducer<Body>();
    for (let i = 1; i <= 4; i++) r.ingest(ev({ event_revision: i, event_id: `e${i}` }));
    expect(r.takeBatch().refetch).toBe(false);
  });
});

describe("SSE reducer — overflow => stale + single invalidation", () => {
  it("marks stale and emits one invalidation on event-count overflow", () => {
    const r = createSseReducer<Body>({ maxEvents: 3, sizeOf: () => 1 });
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    r.ingest(ev({ event_revision: 2, event_id: "e2" }));
    r.ingest(ev({ event_revision: 3, event_id: "e3" }));
    // 4th exceeds maxEvents=3.
    expect(r.ingest(ev({ event_revision: 4, event_id: "e4" }))).toBe(false);
    const batch = r.takeBatch();
    expect(batch.stale).toBe(true);
    expect(batch.refetch).toBe(true);
    expect(batch.events).toHaveLength(3);
  });

  it("marks stale on byte overflow", () => {
    const r = createSseReducer<Body>({ maxBytes: 10, sizeOf: () => 4 });
    r.ingest(ev({ event_revision: 1, event_id: "e1" })); // 4
    r.ingest(ev({ event_revision: 2, event_id: "e2" })); // 8
    expect(r.ingest(ev({ event_revision: 3, event_id: "e3" }))).toBe(false); // 12 > 10
    expect(r.takeBatch().stale).toBe(true);
  });

  it("emits the invalidation only ONCE while stale", () => {
    const r = createSseReducer<Body>({ maxEvents: 1, sizeOf: () => 1 });
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    r.ingest(ev({ event_revision: 2, event_id: "e2" })); // overflow -> stale + refetch
    const first = r.takeBatch();
    expect(first.refetch).toBe(true);
    expect(first.stale).toBe(true);
    // Further events while stale do not emit another refetch.
    r.ingest(ev({ event_revision: 3, event_id: "e3" }));
    const second = r.takeBatch();
    expect(second.refetch).toBe(false);
    expect(second.stale).toBe(true); // sticky
    expect(second.events).toHaveLength(0);
  });

  it("clears stale after reset (post-refetch reseed)", () => {
    const r = createSseReducer<Body>({ maxEvents: 1, sizeOf: () => 1 });
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    r.ingest(ev({ event_revision: 2, event_id: "e2" }));
    expect(r.takeBatch().stale).toBe(true);
    r.reset();
    expect(r.stats().stale).toBe(false);
    // Fresh baseline after the canonical refetch is accepted.
    expect(r.ingest(ev({ generation: 2, event_revision: 101, event_id: "e101" }))).toBe(true);
    expect(r.takeBatch().events).toHaveLength(1);
  });
});

describe("SSE reducer — receipt retention", () => {
  it("retains a receipt and keeps it across reset (survives reload/restart)", () => {
    const r = createSseReducer<Body>();
    r.ingest(ev({ event_revision: 1, event_id: "r1", is_receipt: true, n: 7 }));
    expect(r.getRetainedReceipts().map((e) => e.event_id)).toEqual(["r1"]);
    r.takeBatch();
    // Reset (post-refetch reseed) must preserve retained receipts.
    r.reset();
    expect(r.getRetainedReceipts().map((e) => e.event_id)).toEqual(["r1"]);
  });

  it("retains an already-observed duplicate receipt without re-queuing it", () => {
    const r = createSseReducer<Body>();
    expect(r.ingest(ev({ event_revision: 1, event_id: "r1", is_receipt: true }))).toBe(true);
    r.takeBatch();
    // Duplicate receipt: retained, not re-delivered.
    expect(r.ingest(ev({ event_revision: 1, event_id: "r1", is_receipt: true }))).toBe(false);
    expect(r.takeBatch().events).toHaveLength(0);
    expect(r.getRetainedReceipts()).toHaveLength(1);
  });

  it("retains a receipt that arrives while stale", () => {
    const r = createSseReducer<Body>({ maxEvents: 1, sizeOf: () => 1 });
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    r.ingest(ev({ event_revision: 2, event_id: "e2" })); // overflow -> stale
    r.takeBatch();
    r.ingest(ev({ event_revision: 3, event_id: "r3", is_receipt: true }));
    expect(r.getRetainedReceipts().map((e) => e.event_id)).toEqual(["r3"]);
  });
});

describe("SSE reducer — housekeeping", () => {
  it("reports pending state for the coalescing scheduler", () => {
    const r = createSseReducer<Body>();
    expect(r.hasPending()).toBe(false);
    r.ingest(ev({ event_revision: 1, event_id: "e1" }));
    expect(r.hasPending()).toBe(true);
    r.takeBatch();
    expect(r.hasPending()).toBe(false);
  });
});
