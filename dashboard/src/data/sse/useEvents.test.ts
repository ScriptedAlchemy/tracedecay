import { describe, expect, it } from "vitest";

import type { SseEventEnvelope } from "./types.ts";
import { invalidationKeysForBatch, targetedInvalidationKeys } from "./useEvents.tsx";

const WORK_READ_PARTS = [
  "snapshot",
  "delta",
  "list-attempts",
  "topology",
  "topology-metrics",
  "views",
  "retrieve-evidence",
  "attempt-status",
  "hydrate-artifacts",
  "run-control",
  "placement-status",
] as const;

function workKeys(scope: string): ReadonlyArray<ReadonlyArray<string>> {
  return WORK_READ_PARTS.map((part) => ["work", part, scope]);
}

function event(
  family: string,
  projectId?: string,
): SseEventEnvelope<Record<string, unknown>> {
  return {
    stream: { stream_id: family, generation: 1 },
    event_id: `${family}:1`,
    revision: { event_revision: 1, entity_revision: 1 },
    scope: projectId === undefined ? "scope" : JSON.stringify({ project_id: projectId }),
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

  /**
   * Every Work read part, not just the projection pair. The attempt list and
   * canonical topology page are scoped like the snapshot they are drawn beside,
   * so either surviving a project switch would put one project's execution
   * structure under another project's snapshot. Adding a read part to
   * `workScopeInvalidationKeys` is expected to widen these lists — a part
   * missing from them is a stale read, not a saved refetch.
   */
  it("targets exact Work keys and the default alias for active-project activity", () => {
    expect(
      targetedInvalidationKeys(
        {
          events: [event("task_activity", "project.alpha")],
          refetch: false,
          stale: false,
        },
        "project.alpha",
      ),
    ).toEqual([...workKeys("project:project.alpha"), ...workKeys("all")]);
  });

  it("targets a selected project without refreshing the active-project alias", () => {
    expect(
      targetedInvalidationKeys(
        {
          events: [event("task_activity", "project.beta")],
          refetch: false,
          stale: false,
        },
        "project.alpha",
      ),
    ).toEqual(workKeys("project:project.beta"));
  });

  it("keeps unrelated project invalidations project-exact", () => {
    expect(
      targetedInvalidationKeys(
        {
          events: [
            event("task_activity", "project.alpha"),
            event("task_activity", "project.beta"),
            event("task_activity", "project.alpha"),
            event("heartbeat", "project.gamma"),
          ],
          refetch: false,
          stale: false,
        },
        "project.gamma",
      ),
    ).toEqual([...workKeys("project:project.alpha"), ...workKeys("project:project.beta")]);
  });

  it("does not invent a Work target for task activity without a project", () => {
    expect(
      targetedInvalidationKeys(
        {
          events: [event("task_activity")],
          refetch: false,
          stale: false,
        },
        "project.alpha",
      ),
    ).toEqual([]);
  });
});
