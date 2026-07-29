/**
 * SSE monotone event reducer (framework-free).
 *
 * Implements the plan's reducer contract exactly
 * (docs/plans/tracedecay-v2/11-dashboard-frontend.md):
 *
 *   - dedupe by stream/event/revision identity;
 *   - reject stale-generation events;
 *   - retain already-observed receipts (survive reload/restart);
 *   - detect revision gaps and emit ONE canonical refetch signal;
 *   - bounded queue (5,000 events or 10 MiB) — overflow marks the projection
 *     stale and emits ONE invalidation;
 *   - expose a coalescing batch boundary; the render layer owns the <=10/s
 *     clock and calls `takeBatch()` at each tick. This module owns no timers.
 *
 * The canonical refresh the last two bullets ask for is asynchronous and slow:
 * it awaits every active query while the stream keeps arriving. So it is a
 * transaction here, not a flag — `beginReseed()` stamps it with the current
 * canonical-signal epoch, and `commitReseed()` clears the state that refresh
 * superseded only if no newer signal arrived in the meantime. A refresh that was
 * superseded, or that failed, clears nothing.
 *
 * The reducer never derives product semantics (branch stack, merge order,
 * conflict result, readiness, legal action) from payloads — it only sequences
 * envelopes.
 */
import {
  MAX_OBSERVED_IDENTITIES,
  MAX_QUEUED_EVENTS,
  MAX_QUEUED_BYTES,
  type RefetchReason,
  type ReseedOutcome,
  type ReseedToken,
  type SseBatch,
  type SseEventEnvelope,
  type SseReducerStats,
  type SseReseedPhase,
} from "./types.ts";

export interface SseReducerOptions<TPayload> {
  /** Max queued events before overflow. Defaults to 5,000. */
  maxEvents?: number;
  /** Max queued bytes before overflow. Defaults to 10 MiB. */
  maxBytes?: number;
  /**
   * Byte-size estimator for an event. Defaults to the UTF-16 length of the
   * JSON encoding (a stable upper-bound proxy). Injectable for deterministic
   * overflow tests.
   */
  sizeOf?: (event: SseEventEnvelope<TPayload>) => number;
}

/** Per-stream monotone watermark (generation + last accepted revision). */
interface StreamWatermark {
  generation: number;
  lastEventRevision: number;
}

function dedupeKey(event: SseEventEnvelope): string {
  // stream / event / revision identity.
  return `${event.stream.stream_id}\u0000${event.event_id}\u0000${event.revision.event_revision}`;
}

function defaultSizeOf(event: SseEventEnvelope): number {
  return JSON.stringify(event).length;
}

/**
 * One place maps a settled transaction to its observable phase, so the phase can
 * never disagree with the outcome — a failure cannot be reported as a commit.
 * `null` means "no transition": a stale token settled a transaction that is no
 * longer the active one, which must not disturb the live phase.
 */
function reseedPhaseFor(outcome: ReseedOutcome): SseReseedPhase | null {
  switch (outcome.status) {
    case "committed":
      return { phase: "committed", epoch: outcome.epoch };
    case "superseded":
      return {
        phase: "superseded",
        epoch: outcome.epoch,
        outstandingEpoch: outcome.outstandingEpoch,
      };
    case "failed":
      return { phase: "failed", epoch: outcome.epoch, reason: outcome.reason };
    case "stale_token":
      return null;
    default: {
      const unhandled: never = outcome;
      return unhandled;
    }
  }
}

/**
 * Create a monotone SSE reducer. Not a React hook; safe to hold in any store.
 */
export function createSseReducer<TPayload = unknown>(
  options: SseReducerOptions<TPayload> = {},
) {
  const maxEvents = options.maxEvents ?? MAX_QUEUED_EVENTS;
  const maxBytes = options.maxBytes ?? MAX_QUEUED_BYTES;
  const sizeOf = options.sizeOf ?? (defaultSizeOf as (e: SseEventEnvelope<TPayload>) => number);

  // Generation + revision watermark are tracked per stream: each stream/run has
  // its own monotone revision sequence and reconnect generation. Dedupe,
  // overflow, staleness, and the refetch signal are projection-wide.
  const watermarks = new Map<string, StreamWatermark>();
  let latestGeneration: number | null = null;
  let latestEventRevision: number | null = null;
  let observedCount = 0;
  const observed = new Set<string>();
  const retainedReceipts = new Map<string, SseEventEnvelope<TPayload>>();

  let queue: Array<SseEventEnvelope<TPayload>> = [];
  let queuedBytes = 0;
  let stale = false;
  let refetchRequested = false;

  // The canonical signal has two halves. `refetchRequested` is edge-triggered:
  // `takeBatch()` clears it, because the render layer only needs to be told
  // once. `canonicalEpoch` is the durable half — it only ever counts up, so a
  // signal raised while a refresh is in flight is still visible after the drain
  // that cleared the boolean, and the refresh cannot claim to have covered it.
  let canonicalEpoch = 0;
  let supersededEpoch = 0;
  let failedEpoch: number | null = null;
  let activeReseed: ReseedToken | null = null;
  let reseedPhase: SseReseedPhase = { phase: "idle" };
  let refetchReason: RefetchReason | null = null;

  /**
   * Raise one canonical signal. The batch-level flag is coalesced (a second
   * signal before the next `takeBatch()` does not produce a second batch flag),
   * but the epoch advances every time, which is what makes an in-flight refresh
   * detectably out of date.
   *
   * Public so the render layer can escalate a refresh it owns and that failed:
   * a rejected targeted invalidation leaves that slice unfresh, and saying so
   * here routes it through the same transaction instead of dropping it.
   */
  function requestCanonicalRefresh(reason: RefetchReason): void {
    refetchRequested = true;
    refetchReason = reason;
    canonicalEpoch += 1;
  }

  function retainReceipt(key: string, event: SseEventEnvelope<TPayload>): void {
    if (event.is_receipt) retainedReceipts.set(key, event);
  }

  /**
   * Remember an accepted event's identity, capped. `Set` iterates in insertion
   * order, so evicting the front is FIFO: the newest identities — the only ones
   * a redelivery can still be above its stream watermark for — are the ones
   * kept. See {@link MAX_OBSERVED_IDENTITIES} for why the memory is bounded
   * this way rather than cleared.
   */
  function rememberIdentity(key: string): void {
    observed.add(key);
    while (observed.size > MAX_OBSERVED_IDENTITIES) {
      const oldest = observed.values().next();
      if (oldest.done) break;
      observed.delete(oldest.value);
    }
  }

  /**
   * Feed one event. Returns `true` when the event was newly accepted into the
   * pending batch (useful for callers that schedule a coalesced flush).
   */
  function ingest(event: SseEventEnvelope<TPayload>): boolean {
    const streamId = event.stream.stream_id;
    const incomingGen = event.stream.generation;
    const mark = watermarks.get(streamId);

    // 1. Stale-generation rejection.
    if (mark && incomingGen < mark.generation) {
      return false;
    }
    // 2. Reconnect to a newer generation. Dedupe state and the revision
    //    identities are preserved, but the per-run revision sequence restarts.
    //    Mark the canonical projection for refetch and accept the new run's
    //    first event instead of comparing it to the previous run's watermark.
    if (mark && incomingGen > mark.generation) {
      mark.generation = incomingGen;
      mark.lastEventRevision = -1;
      requestCanonicalRefresh("generation_change");
    }

    const key = dedupeKey(event);

    // Once stale (overflow), a single invalidation has already been emitted and
    // the consumer will refresh and commit the reseed; further events are moot.
    // Receipts are still retained so a reload/restart never loses them.
    if (stale) {
      if (observed.has(key)) return false;
      retainReceipt(key, event);
      return false;
    }

    // 3. Dedupe by stream/event/revision identity.
    if (observed.has(key)) {
      // Retain already-observed receipts; never re-queue (avoids double render).
      retainReceipt(key, event);
      return false;
    }

    const rev = event.revision.event_revision;
    const lastRev = mark ? mark.lastEventRevision : null;

    // Out-of-order / superseded within this stream: an unseen event whose
    // revision is not newer than the stream watermark. Drop it (retain if a
    // receipt) — the monotone sequence has moved past it.
    if (lastRev !== null && rev <= lastRev) {
      retainReceipt(key, event);
      return false;
    }

    // 4. Overflow: bounded projection queue. Mark stale + one canonical
    //    invalidation.
    const size = sizeOf(event);
    const wouldOverflow = queue.length + 1 > maxEvents || queuedBytes + size > maxBytes;
    if (wouldOverflow) {
      stale = true;
      requestCanonicalRefresh("overflow");
      retainReceipt(key, event);
      return false;
    }

    // 5. Revision-gap detection: emit one canonical refetch. We still accept the
    //    event and advance the watermark; the refetch reseeds the projection.
    if (lastRev !== null && rev > lastRev + 1) {
      requestCanonicalRefresh("revision_gap");
    }

    // 6. Accept.
    rememberIdentity(key);
    observedCount += 1;
    if (mark) {
      mark.lastEventRevision = Math.max(mark.lastEventRevision, rev);
    } else {
      watermarks.set(streamId, { generation: incomingGen, lastEventRevision: rev });
    }
    latestGeneration = latestGeneration === null ? incomingGen : Math.max(latestGeneration, incomingGen);
    latestEventRevision = latestEventRevision === null ? rev : Math.max(latestEventRevision, rev);
    queue.push(event);
    queuedBytes += size;
    retainReceipt(key, event);
    return true;
  }

  /**
   * The coalescing batch boundary. The render layer calls this at its own
   * <=10/s cadence. Returns the coalesced batch and clears the pending queue
   * and the (single) refetch signal. Draining is not the same as serving: the
   * cleared flag is only the notification, and the need it announced stays in
   * `canonicalEpoch` until a refresh actually supersedes it. `stale` is sticky
   * until {@link commitReseed} succeeds.
   */
  function takeBatch(): SseBatch<TPayload> {
    const batch: SseBatch<TPayload> = {
      events: queue,
      refetch: refetchRequested,
      stale,
    };
    queue = [];
    queuedBytes = 0;
    refetchRequested = false;
    return batch;
  }

  /**
   * True when a batch is pending or a refetch/stale signal has yet to be
   * announced to the render layer. This is the tick filter, not the freshness
   * authority: a signal that was already announced but not yet served lives in
   * {@link canonicalRefreshOutstanding}.
   */
  function hasPending(): boolean {
    return queue.length > 0 || refetchRequested || stale;
  }

  /**
   * True when a canonical refresh is owed and can be started now: some signal
   * has been raised that no successful refresh has superseded, nothing is
   * already in flight, and the newest signal is not one whose refresh already
   * failed. That last clause is what keeps a failure from becoming a storm —
   * retrying the identical refresh on every 100 ms tick would hammer the daemon
   * exactly when it is least able to answer. The failure stays visible in
   * {@link stats} instead, and any newer signal is attempted normally.
   */
  function canonicalRefreshOutstanding(): boolean {
    return (
      activeReseed === null && canonicalEpoch > supersededEpoch && canonicalEpoch !== failedEpoch
    );
  }

  /**
   * Open a canonical-refresh transaction, stamped with the epoch of the signal
   * it is answering. Call it immediately before issuing the refresh so nothing
   * can slip in between.
   */
  function beginReseed(): ReseedToken {
    const token: ReseedToken = { epoch: canonicalEpoch };
    activeReseed = token;
    reseedPhase = { phase: "in_flight", epoch: token.epoch };
    return token;
  }

  function settleReseed(token: ReseedToken, failure: string | null): ReseedOutcome {
    if (activeReseed === null || activeReseed.epoch !== token.epoch) {
      return { status: "stale_token", epoch: token.epoch };
    }
    activeReseed = null;

    if (failure !== null) {
      // Nothing is cleared: the refresh did not happen, so it superseded
      // nothing. The signal stays outstanding and `stale` stays set. Gate the
      // retry on this exact epoch, so a newer signal still gets an attempt.
      failedEpoch = token.epoch;
      return { status: "failed", epoch: token.epoch, reason: failure };
    }

    if (token.epoch !== canonicalEpoch) {
      // A gap, reconnect, or overflow landed after this refresh was issued, so
      // the data it just fetched cannot account for it. Clear nothing and leave
      // the signal outstanding: exactly one follow-up refresh is now owed.
      return { status: "superseded", epoch: token.epoch, outstandingEpoch: canonicalEpoch };
    }

    // Success, and nothing newer arrived — so clear exactly what this refresh
    // superseded, which is the staleness the signal raised and nothing else.
    // The queue holds events that arrived after the refresh was issued and are
    // therefore not in its result; per-stream watermarks are the only reason the
    // next gap is detectable at all; dedupe identities are the only reason a
    // redelivery is recognizable. Wiping any of those was the old reseed's bug.
    supersededEpoch = token.epoch;
    stale = false;
    failedEpoch = null;
    refetchReason = null;
    return { status: "committed", epoch: token.epoch };
  }

  function applyOutcome(outcome: ReseedOutcome): ReseedOutcome {
    const phase = reseedPhaseFor(outcome);
    if (phase !== null) reseedPhase = phase;
    return outcome;
  }

  /** The canonical refresh for `token` succeeded. */
  function commitReseed(token: ReseedToken): ReseedOutcome {
    return applyOutcome(settleReseed(token, null));
  }

  /** The canonical refresh for `token` failed; retain that as typed state. */
  function abortReseed(token: ReseedToken, reason: string): ReseedOutcome {
    return applyOutcome(settleReseed(token, reason));
  }

  function getRetainedReceipts(): Array<SseEventEnvelope<TPayload>> {
    return [...retainedReceipts.values()];
  }

  function stats(): SseReducerStats {
    return {
      observedEvents: observedCount,
      observedIdentities: observed.size,
      queuedEvents: queue.length,
      queuedBytes,
      stale,
      lastEventRevision: latestEventRevision,
      generation: latestGeneration,
      canonicalEpoch,
      supersededEpoch,
      canonicalRefreshOutstanding: canonicalRefreshOutstanding(),
      refetchReason,
      reseed: reseedPhase,
    };
  }

  return {
    ingest,
    takeBatch,
    hasPending,
    requestCanonicalRefresh,
    canonicalRefreshOutstanding,
    beginReseed,
    commitReseed,
    abortReseed,
    getRetainedReceipts,
    stats,
  };
}

export type SseReducer<TPayload = unknown> = ReturnType<typeof createSseReducer<TPayload>>;
