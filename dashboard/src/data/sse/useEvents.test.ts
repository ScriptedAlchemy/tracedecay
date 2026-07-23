import { describe, expect, it } from "vitest";

import type { SseBatch, SseEventEnvelope } from "./types.ts";

function event(family: string): SseEventEnvelope<Record<string, unknown>> {
  return {
    stream: { stream_id: family, generation: 1 },
    event_id: `${family}:1`,
    revision: { event_revision: 1, entity_revision: 1 },
    scope: "scope",
    observation_time: "1",
    watermark: "1",
    coverage: {},
    payload: { family },
  };
}

describe("SSE query invalidation", () => {
  it("maps typed invalidations to canonical query roots", async () => {
    const module = await import("./useEvents.tsx");
    const invalidationKeysForBatch = (
      module as unknown as {
        invalidationKeysForBatch: (
          batch: SseBatch<Record<string, unknown>>,
        ) => ReadonlyArray<ReadonlyArray<string>>;
      }
    ).invalidationKeysForBatch;

    expect(typeof invalidationKeysForBatch).toBe("function");
    expect(
      invalidationKeysForBatch({
        events: [
          event("storage_telemetry_invalidated"),
          event("project_registry_changed"),
          event("heartbeat"),
        ],
        refetch: false,
        stale: false,
      }),
    ).toEqual([
      ["storage", "telemetry"],
      ["projects"],
    ]);
  });

  it("invalidates all canonical queries after a revision gap", async () => {
    const module = await import("./useEvents.tsx");
    const invalidationKeysForBatch = (
      module as unknown as {
        invalidationKeysForBatch: (
          batch: SseBatch<Record<string, unknown>>,
        ) => ReadonlyArray<ReadonlyArray<string>>;
      }
    ).invalidationKeysForBatch;

    expect(typeof invalidationKeysForBatch).toBe("function");
    expect(
      invalidationKeysForBatch({
        events: [],
        refetch: true,
        stale: false,
      }),
    ).toEqual([[]]);
  });
});
