import { describe, expect, it } from "vitest";

import type { SseEventEnvelope } from "./types.ts";
import { invalidationKeysForBatch, targetedInvalidationKeys } from "./useEvents.tsx";

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
  it("maps typed invalidations to canonical query roots", () => {
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

  it("invalidates all canonical queries after a revision gap", () => {
    expect(typeof invalidationKeysForBatch).toBe("function");
    expect(
      invalidationKeysForBatch({
        events: [],
        refetch: true,
        stale: false,
      }),
    ).toEqual([[]]);
  });

  it("keeps the targeted keys of a canonical batch reachable on their own", () => {
    // A canonical batch whose events still name a narrower root: while a
    // whole-projection refresh is already in flight, this is the set the render
    // layer issues so those events are not silently dropped on the floor.
    const batch = {
      events: [event("project_registry_changed"), event("heartbeat")],
      refetch: true,
      stale: true,
    };
    expect(invalidationKeysForBatch(batch)).toEqual([[]]);
    expect(targetedInvalidationKeys(batch)).toEqual([["projects"]]);
  });
});
