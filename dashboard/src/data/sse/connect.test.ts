import { afterEach, describe, expect, it, vi } from "vitest";

import { connectEvents } from "./connect.ts";

type EventListener = (event: MessageEvent<string>) => void;

class FakeEventSource {
  static readonly CLOSED = 2;
  static instances: FakeEventSource[] = [];

  readonly listeners = new Map<string, EventListener[]>();
  readyState = 1;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: EventListener | null = null;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(name: string, listener: EventListener) {
    const listeners = this.listeners.get(name) ?? [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }

  emit(name: string, data: unknown) {
    const event = { data: JSON.stringify(data) } as MessageEvent<string>;
    for (const listener of this.listeners.get(name) ?? []) listener(event);
  }

  close() {
    this.readyState = FakeEventSource.CLOSED;
  }
}

describe("dashboard SSE wire bridge", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    FakeEventSource.instances = [];
  });

  it("subscribes to named backend events and normalizes their typed envelope", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;

    source.emit("storage_telemetry", {
      stream: "storage_telemetry",
      run_id: "run-42-1700000000000000",
      event_revision: 7,
      entity_revision: 4,
      scope: {
        project_id: "project.alpha",
        storage_mode: "profile_sharded",
        store_root: "/stores/project.alpha",
      },
      observation_time_micros: 1700000000000100,
      source_watermark: {
        source: "storage_telemetry",
        watermark: "8192",
      },
      coverage: {
        completeness: "complete",
        denominator: 1,
      },
      kind: {
        family: "storage_telemetry_invalidated",
        total_bytes: 8192,
      },
    });

    const batch = connection.reducer.takeBatch();
    expect(batch.events).toHaveLength(1);
    expect(batch.events[0]).toMatchObject({
      stream: {
        stream_id: "storage_telemetry",
        generation: 1700000000000000,
      },
      event_id: "run-42-1700000000000000:storage_telemetry:7",
      revision: {
        event_revision: 7,
        entity_revision: 4,
      },
      observation_time: "1700000000000100",
      watermark: "8192",
      payload: {
        family: "storage_telemetry_invalidated",
        total_bytes: 8192,
      },
    });
    connection.close();
  });
});
