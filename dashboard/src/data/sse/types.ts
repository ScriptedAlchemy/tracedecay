/**
 * SSE monotone event reducer — typed interfaces.
 *
 * Framework-free (no React, no timers here). These types model the event
 * envelope every dashboard SSE stream emits, per
 * docs/plans/tracedecay-v2/11-dashboard-frontend.md:
 *
 *   "Every event carries stream/run identity, event and entity revision,
 *    scope, observation time, source watermark, and coverage. The monotone
 *    event reducer deduplicates by stream/event/revision, rejects stale
 *    generations, retains receipts already observed, and triggers one
 *    canonical refetch on a revision gap."
 *
 *   "...bound the queue to 5,000 events or 10 MiB. Overflow marks the
 *    projection stale and performs one canonical invalidation/refetch."
 *
 * The reducer owns the *batch boundary* (coalescing hook point) but not the
 * render clock: the render layer throttles to <=10 renders/s and calls
 * `takeBatch()` at each tick.
 */

/** Bounded-queue defaults from the plan's performance envelope. */
export const MAX_QUEUED_EVENTS = 5_000;
export const MAX_QUEUED_BYTES = 10 * 1024 * 1024; // 10 MiB

/** Stable identity of a stream connection generation. */
export interface StreamIdentity {
  /** Opaque stream ID (a workspace/projection SSE channel). */
  stream_id: string;
  /**
   * Monotone connection generation. A reconnect increments this; events from
   * an older generation are stale and rejected.
   */
  generation: number;
}

/** Monotone revision pair carried by every event. */
export interface EventRevision {
  /** Per-stream event sequence revision. Strictly increases with no gaps. */
  event_revision: number;
  /** Revision of the entity/projection the event mutates. */
  entity_revision: number;
}

/**
 * A single SSE event envelope. `payload` is opaque to the reducer — it never
 * derives product semantics (branch stack, merge order, readiness, legal
 * action) from it.
 */
export interface SseEventEnvelope<TPayload = unknown> {
  stream: StreamIdentity;
  /** Opaque per-event identity, unique within a stream. */
  event_id: string;
  revision: EventRevision;
  /** Opaque exact scope identity (never a title/path/branch). */
  scope: string;
  /** Observation time (server clock, opaque string). */
  observation_time: string;
  /** Source watermark. */
  watermark: string;
  /** Coverage descriptor (opaque to the reducer). */
  coverage: unknown;
  /**
   * Whether this event is a receipt for an already-observed operation. The
   * reducer retains receipts even when their event_revision would otherwise be
   * treated as already-seen, so a crash/restart never loses a receipt.
   */
  is_receipt?: boolean;
  payload: TPayload;
}

/**
 * The reducer's coalesced output for one batch boundary. The render layer
 * consumes this at its own <=10/s cadence.
 */
export interface SseBatch<TPayload = unknown> {
  /** Newly accepted events in monotone order since the last batch. */
  events: Array<SseEventEnvelope<TPayload>>;
  /**
   * True when the reducer emitted a single canonical refetch signal in this
   * batch (revision gap or overflow). The render/query layer performs exactly
   * one invalidation/refetch; the reducer never dispatches it itself.
   */
  refetch: boolean;
  /**
   * True when the projection is marked stale (overflow). Stale is sticky until
   * a refetch reseeds the reducer via {@link reset}.
   */
  stale: boolean;
}

/** Reason a refetch was requested (diagnostics only; not product semantics). */
export type RefetchReason = "revision_gap" | "overflow";

/** Snapshot of reducer state for tests/telemetry. */
export interface SseReducerStats {
  observedEvents: number;
  queuedEvents: number;
  queuedBytes: number;
  stale: boolean;
  lastEventRevision: number | null;
  generation: number | null;
}
