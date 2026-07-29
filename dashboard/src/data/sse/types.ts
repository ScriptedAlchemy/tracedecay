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

/**
 * Upper bound on remembered dedupe identities.
 *
 * Identity memory has to outlive the queue. A canonical refresh commits while
 * the stream keeps running, and an event redelivered after that commit must
 * still be recognized as already applied — clearing the identity set was what
 * let a redelivery be applied twice. So the growth is bounded explicitly
 * instead of by wiping: two full queue ceilings, evicted in insertion order.
 * The per-stream watermark is the primary guard (anything at or below it is
 * refused regardless of identity); this set is what catches a redelivery that
 * is still above the watermark, and those are always recent, which is exactly
 * what insertion-order eviction keeps.
 */
export const MAX_OBSERVED_IDENTITIES = 2 * MAX_QUEUED_EVENTS;

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
   * a canonical refresh actually succeeds — see {@link ReseedToken}.
   */
  stale: boolean;
}

/** Reason a refetch was requested (diagnostics only; not product semantics). */
export type RefetchReason =
  | "revision_gap"
  | "generation_change"
  | "overflow"
  /** A refresh the render layer owns rejected, so its slice is not fresh. */
  | "invalidation_failed";

/**
 * Handle on one canonical-refresh transaction.
 *
 * The refresh is asynchronous and slow — it awaits every active query — so the
 * stream keeps running underneath it. The token records the canonical-signal
 * epoch the refresh was issued against, which is what lets the reducer answer
 * the only question that matters when the refresh settles: is this refresh
 * still the newest truth, or did a gap, reconnect, or overflow arrive after it
 * was issued and supersede it?
 */
export interface ReseedToken {
  readonly epoch: number;
}

/**
 * What settling a transaction did. Success and failure are separate variants on
 * purpose: treating a rejected refresh as a successful one is what silently
 * forgave staleness the client had never actually refetched.
 */
export type ReseedOutcome =
  /** The refresh was the newest truth; the state it superseded is cleared. */
  | { readonly status: "committed"; readonly epoch: number }
  /** A newer signal arrived mid-flight: nothing is cleared, one refresh is owed. */
  | { readonly status: "superseded"; readonly epoch: number; readonly outstandingEpoch: number }
  /** The refresh rejected: nothing is cleared and the signal stays outstanding. */
  | { readonly status: "failed"; readonly epoch: number; readonly reason: string }
  /** The token no longer names the active transaction (double settle). */
  | { readonly status: "stale_token"; readonly epoch: number };

/** Observable phase of the reducer's canonical-refresh transaction. */
export type SseReseedPhase =
  | { readonly phase: "idle" }
  | { readonly phase: "in_flight"; readonly epoch: number }
  | { readonly phase: "committed"; readonly epoch: number }
  | { readonly phase: "superseded"; readonly epoch: number; readonly outstandingEpoch: number }
  | { readonly phase: "failed"; readonly epoch: number; readonly reason: string };

/** Snapshot of reducer state for tests/telemetry. */
export interface SseReducerStats {
  observedEvents: number;
  /** Dedupe identities currently remembered (bounded, see the cap above). */
  observedIdentities: number;
  queuedEvents: number;
  queuedBytes: number;
  stale: boolean;
  lastEventRevision: number | null;
  generation: number | null;
  /** Monotone count of canonical signals raised since the reducer was created. */
  canonicalEpoch: number;
  /** Epoch the newest *successful* canonical refresh superseded. */
  supersededEpoch: number;
  /** True when a canonical refresh is owed and can be started now. */
  canonicalRefreshOutstanding: boolean;
  /** Why the outstanding signal was raised; null once it has been superseded. */
  refetchReason: RefetchReason | null;
  reseed: SseReseedPhase;
}
