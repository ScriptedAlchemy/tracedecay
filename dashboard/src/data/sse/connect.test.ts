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

  it("subscribes only to server-emitted code-index activity", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;

    expect(source.listeners.has("code_index")).toBe(false);
    expect(source.listeners.has("code_index_activity")).toBe(true);
    connection.close();
  });

  it("projects accepted events to live pulses carrying their own scope identity", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;
    const frame = (revision: number, projectId: string) => ({
      stream: "project_registry",
      run_id: "run-1-1700000000000000",
      event_revision: revision,
      entity_revision: revision,
      scope: {
        project_id: projectId,
        storage_mode: "profile_sharded",
        store_root: `/stores/${projectId}`,
      },
      observation_time_micros: 1700000000000000 + revision,
      source_watermark: null,
      coverage: { completeness: "complete", denominator: 1 },
      kind: { family: "project_registry_changed", project_count: 3 },
    });

    source.emit("project_registry", frame(1, "project.alpha"));
    source.emit("project_registry", frame(2, "project.beta"));

    expect(connection.activityRevision()).toBe(2);
    expect(connection.activity()).toMatchObject([
      { projectId: "project.alpha", family: "project_registry_changed" },
      { projectId: "project.beta", family: "project_registry_changed" },
    ]);

    // A duplicate is one real occurrence: it must not pulse twice.
    source.emit("project_registry", frame(2, "project.beta"));
    expect(connection.activityRevision()).toBe(2);
    expect(connection.activity()).toHaveLength(2);

    // Reading pulses never disturbs the reducer's batch boundary.
    expect(connection.reducer.takeBatch().events).toHaveLength(2);
    expect(connection.activity()).toHaveLength(2);
    connection.close();
  });

  it("pulses live agent-activity families on the project that did the work", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;
    const activity = (
      eventName: string,
      streamId: string,
      family: string,
      projectId: string,
      payload: Record<string, unknown>,
    ) => ({
      eventName,
      frame: {
        stream: streamId,
        run_id: "run-9-1700000000000000",
        event_revision: 1,
        entity_revision: 1,
        // The daemon coalesces bursts, so coverage is honestly unknown.
        coverage: { completeness: "unknown" },
        scope: {
          project_id: projectId,
          storage_mode: "profile_sharded",
          store_root: "/stores/profile",
        },
        observation_time_micros: 1700000000000000,
        source_watermark: null,
        kind: { family, count: 1, ...payload },
      },
    });

    const frames = [
      activity("hook_activity", "hook_activity:project.alpha", "hook_activity", "project.alpha", {
        hook_events: 12,
      }),
      activity("tool_call", "tool_call:project.beta", "tool_call_activity", "project.beta", {
        calls: 4,
      }),
      activity(
        "session_ingest",
        "session_ingest:project.gamma",
        "session_ingest_activity",
        "project.gamma",
        { messages: 31 },
      ),
      activity(
        "code_index_activity",
        "code_index_activity:project.alpha",
        "code_index_activity",
        "project.alpha",
        { files: 7 },
      ),
      // Work task mutations. Listed last because it is the newest family, and
      // the one whose subscription this asserts: drop it from the opt-in names
      // and the frame below is silently discarded rather than failing loudly.
      activity("task_activity", "task_activity:project.alpha", "task_activity", "project.alpha", {
        tasks: 3,
      }),
    ];
    for (const { eventName, frame } of frames) source.emit(eventName, frame);

    // Each named event must be subscribed, or the burst never reaches the UI.
    expect(connection.activityRevision()).toBe(5);
    expect(connection.activity()).toMatchObject([
      { projectId: "project.alpha", family: "hook_activity" },
      { projectId: "project.beta", family: "tool_call_activity" },
      { projectId: "project.gamma", family: "session_ingest_activity" },
      { projectId: "project.alpha", family: "code_index_activity" },
      { projectId: "project.alpha", family: "task_activity" },
    ]);
    // Per-project streams keep their own revision sequence, so two projects
    // both at revision 1 are two accepted events, not a duplicate.
    expect(connection.reducer.takeBatch().events).toHaveLength(5);
    connection.close();
  });

  /**
   * The shape the activity lane actually puts on the wire.
   *
   * Every fixture above is a poll-lane frame: a `run-<n>-<micros>` run id and a
   * stream id of its own. The activity lane emits neither. It publishes on the
   * single shared `dashboard_activity` stream under the constant run id
   * `registered-observability-v1` (`src/application/event_lane.rs`), which
   * `activity_event` copies to the wire verbatim
   * (`src/dashboard/events_api.rs`), and it carries its monotone row id in
   * `event_revision`.
   *
   * Decoding it is what makes the subscription worth having. A decoder that
   * only accepts the poll shape drops every hook, session-ingest, code-index,
   * tool-call and task frame the daemon sends — and still reports the link as
   * live, because the state flips before the frame is parsed. That failure is
   * invisible to every other test here, so it is pinned against the real
   * constant rather than a plausible one.
   */
  it("accepts the activity lane's own run id and shared stream", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;

    source.emit("task_activity", {
      stream: "dashboard_activity",
      run_id: "registered-observability-v1",
      event_revision: 4711,
      entity_revision: 4711,
      coverage: { completeness: "complete", denominator: 3 },
      scope: {
        project_id: "project.alpha",
        storage_mode: "profile_sharded",
        store_root: "/stores/profile",
      },
      observation_time_micros: 1700000000000000,
      source_watermark: { source: "dashboard_activity_lane", watermark: "4711" },
      kind: { family: "task_activity", count: 3, tasks: 5, detail: null },
    });

    expect(
      connection.activity(),
      "the activity lane's own run id must decode, or every live family is dropped",
    ).toMatchObject([{ projectId: "project.alpha", family: "task_activity" }]);
    expect(connection.reducer.takeBatch().events).toHaveLength(1);
    connection.close();
  });

  it("keeps the pulse ring bounded so a long-lived tab cannot grow it", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const connection = connectEvents("/api/events");
    const source = FakeEventSource.instances[0]!;
    for (let revision = 1; revision <= 80; revision += 1) {
      source.emit("heartbeat", {
        stream: "heartbeat",
        run_id: "run-1-1700000000000000",
        event_revision: revision,
        entity_revision: null,
        scope: { project_id: null, storage_mode: "project_local", store_root: "/s" },
        observation_time_micros: 1700000000000000 + revision,
        source_watermark: null,
        coverage: { completeness: "complete", denominator: 1 },
        kind: { family: "heartbeat" },
      });
    }
    expect(connection.activityRevision()).toBe(80);
    expect(connection.activity()).toHaveLength(64);
    expect(connection.activity()[0]).toMatchObject({ projectId: null, family: "heartbeat" });
    connection.close();
  });
});
