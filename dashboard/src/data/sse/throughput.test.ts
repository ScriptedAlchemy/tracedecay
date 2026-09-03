/**
 * Production queue ceilings and named sustained rates. `reducer.test.ts` uses
 * injected toy limits; this file drives the real defaults. Time is the loop
 * index (one outer iteration = one 100 ms tick), never a wall clock.
 */
import { describe, expect, it } from "vitest";

import { createSseReducer } from "./reducer.ts";
import { MAX_QUEUED_BYTES, MAX_QUEUED_EVENTS, type SseEventEnvelope } from "./types.ts";

/** The render layer's coalescing cadence: at most ten batch boundaries/second. */
const RENDER_TICKS_PER_SECOND = 10;

interface Body {
  family: string;
  filler: string;
}

/**
 * A realistically shaped envelope. The reducer's default `sizeOf` is
 * `JSON.stringify(event).length`, so these cases deliberately do NOT inject a
 * size estimator — the byte ceiling must be exercised through the same
 * accounting production uses.
 */
function envelope(revision: number, filler = ""): SseEventEnvelope<Body> {
  return {
    stream: { stream_id: "code_index_activity", generation: 1 },
    event_id: `run-1-1700000000000000:code_index_activity:${revision}`,
    revision: { event_revision: revision, entity_revision: revision },
    scope: '{"project_id":"project.alpha","storage_mode":"profile_sharded"}',
    observation_time: String(1_700_000_000_000_000 + revision),
    watermark: String(revision),
    coverage: { completeness: "complete", denominator: 1 },
    payload: { family: "code_index_activity", filler },
  };
}

describe("SSE queue ceiling — 5,000 events / 10 MiB", () => {
  it("uses the production defaults", () => {
    expect(MAX_QUEUED_EVENTS).toBe(5_000);
    expect(MAX_QUEUED_BYTES).toBe(10 * 1024 * 1024);
  });

  it("holds the queue at exactly 5,000 events when the render layer stalls", () => {
    const reducer = createSseReducer<Body>();
    const total = 10 * 1_000;
    let accepted = 0;
    let firstRejected: number | null = null;
    let peakQueued = 0;
    let peakBytes = 0;

    for (let revision = 1; revision <= total; revision += 1) {
      if (reducer.ingest(envelope(revision))) accepted += 1;
      else if (firstRejected === null) firstRejected = revision;
      const stats = reducer.stats();
      peakQueued = Math.max(peakQueued, stats.queuedEvents);
      peakBytes = Math.max(peakBytes, stats.queuedBytes);
    }

    expect(accepted).toBe(MAX_QUEUED_EVENTS);
    expect(firstRejected).toBe(MAX_QUEUED_EVENTS + 1);
    expect(peakQueued).toBe(MAX_QUEUED_EVENTS);
    expect(peakBytes).toBeLessThanOrEqual(MAX_QUEUED_BYTES);

    const overflow = reducer.takeBatch();
    expect(overflow.events).toHaveLength(MAX_QUEUED_EVENTS);
    expect(overflow.stale).toBe(true);
    expect(overflow.refetch).toBe(true);
    // 5,000 dropped events produced one invalidation, not one apiece.
    expect(reducer.takeBatch().refetch).toBe(false);
  });

  it("stops on the 10 MiB ceiling before the event ceiling for large payloads", () => {
    const filler = "x".repeat(4 * 1024);
    const reducer = createSseReducer<Body>();
    let accepted = 0;
    let firstRejected: number | null = null;

    for (let revision = 1; revision <= MAX_QUEUED_EVENTS; revision += 1) {
      if (reducer.ingest(envelope(revision, filler))) accepted += 1;
      else if (firstRejected === null) firstRejected = revision;
      expect(reducer.stats().queuedBytes).toBeLessThanOrEqual(MAX_QUEUED_BYTES);
    }

    const stats = reducer.stats();
    expect(stats.stale).toBe(true);
    // Bytes bound first: the count ceiling was never the binding constraint.
    expect(accepted).toBeLessThan(MAX_QUEUED_EVENTS);
    expect(firstRejected).not.toBeNull();
    // And the queue really was driven *to* the ceiling — the remaining headroom
    // is smaller than the event that was refused.
    const refusedSize = JSON.stringify(envelope(firstRejected!, filler)).length;
    expect(MAX_QUEUED_BYTES - stats.queuedBytes).toBeLessThan(refusedSize);
    expect(reducer.takeBatch().refetch).toBe(true);
  });

  it("resumes at full rate after the canonical refetch reseeds the reducer", () => {
    const reducer = createSseReducer<Body>();
    for (let revision = 1; revision <= MAX_QUEUED_EVENTS + 500; revision += 1) {
      reducer.ingest(envelope(revision));
    }
    expect(reducer.takeBatch().stale).toBe(true);

    reducer.commitReseed(reducer.beginReseed());

    // One second at the peak rate, drained on the 100 ms grid. The stream picks
    // up at 10,000, well past the 5,000 the overflow refused, so the first tick
    // honestly reports that jump as a gap — a reseed establishes a fresh
    // baseline for the projection, not amnesia about the revision sequence.
    let delivered = 0;
    let gapTicks = 0;
    for (let tick = 0; tick < RENDER_TICKS_PER_SECOND; tick += 1) {
      for (let i = 0; i < 100; i += 1) {
        reducer.ingest(envelope(10_000 + tick * 100 + i));
      }
      const batch = reducer.takeBatch();
      expect(batch.stale).toBe(false);
      if (batch.refetch) gapTicks += 1;
      delivered += batch.events.length;
    }
    expect(delivered).toBe(1_000);
    expect(gapTicks).toBe(1);
  });
});

describe("SSE sustained throughput — two named rates", () => {
  const rates = [
    { label: "100 events/s for ten minutes", perSecond: 100, seconds: 600 },
    { label: "1,000 events/s for ten seconds", perSecond: 1_000, seconds: 10 },
  ] as const;

  it.each(rates)(
    "sustains $label with no loss, no overflow, and one batch per render tick",
    ({ perSecond, seconds }) => {
      const reducer = createSseReducer<Body>();
      const ticks = seconds * RENDER_TICKS_PER_SECOND;
      const perTick = perSecond / RENDER_TICKS_PER_SECOND;

      let revision = 0;
      let delivered = 0;
      let batches = 0;
      let refetchSignals = 0;
      let staleBatches = 0;
      let peakQueued = 0;
      let peakBytes = 0;

      for (let tick = 0; tick < ticks; tick += 1) {
        for (let i = 0; i < perTick; i += 1) {
          revision += 1;
          reducer.ingest(envelope(revision));
        }
        const stats = reducer.stats();
        peakQueued = Math.max(peakQueued, stats.queuedEvents);
        peakBytes = Math.max(peakBytes, stats.queuedBytes);

        const batch = reducer.takeBatch();
        batches += 1;
        delivered += batch.events.length;
        if (batch.refetch) refetchSignals += 1;
        if (batch.stale) staleBatches += 1;
      }

      expect(revision).toBe(perSecond * seconds);
      expect(delivered).toBe(perSecond * seconds);
      expect(refetchSignals).toBe(0);
      expect(staleBatches).toBe(0);
      expect(peakQueued).toBe(perTick);
      expect(peakQueued).toBeLessThanOrEqual(MAX_QUEUED_EVENTS);
      expect(peakBytes).toBeLessThanOrEqual(MAX_QUEUED_BYTES);
      expect(batches / seconds).toBe(RENDER_TICKS_PER_SECOND);
    },
  );

  it("coalesces a 1,000-event second into ten batches, not a thousand", () => {
    const reducer = createSseReducer<Body>();
    const sizes: number[] = [];
    for (let tick = 0; tick < RENDER_TICKS_PER_SECOND; tick += 1) {
      for (let i = 0; i < 100; i += 1) reducer.ingest(envelope(tick * 100 + i + 1));
      sizes.push(reducer.takeBatch().events.length);
    }
    expect(sizes).toHaveLength(RENDER_TICKS_PER_SECOND);
    expect(sizes.reduce((a, b) => a + b, 0)).toBe(1_000);
    expect(sizes.every((size) => size === 100)).toBe(true);
  });
});
