/**
 * The state of one Explorer lane, as read from the wire.
 *
 * Explorer fans one query out to three independent memories, and each of them
 * can fail, refuse, stall, or answer emptily on its own. The whole point of
 * this module is that those are not the same event and must never become the
 * same pixel: a source the coordinator reported as `unavailable` is a
 * different fact from a browser that cannot reach the daemon, which is a
 * different fact again from a source that answered and had nothing to say.
 *
 * Everything here is derived by parsing the generated contract types
 * (`ExplorerSourceProgressV1`, `ExplorerResultPageV1`, `ExplorerSourceOutcomeV1`)
 * rather than by poking at fields on an `unknown`. The one place the wire is
 * genuinely untyped — the result rows — is isolated in `narrowPageRows` below,
 * which documents the backend change that would delete it.
 */
import type { EvidenceQuality } from '../../ui/EvidencePattern.tsx';
import type { DomainStateKind } from '../../ui/StateChip';
import type {
  ExplorerQueryRunV1,
  ExplorerResultPageV1,
  ExplorerRunStateV1,
  ExplorerSourceIdV1,
  ExplorerSourceOutcomeV1,
  ExplorerSourcePhaseV1,
  ExplorerSourceProgressV1,
  DashboardDomainStateV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { LegacyResult } from '../../data/query/legacy.ts';
import { codeHits, knowledgeHits, sessionHits, type Hit, type LaneId } from './model.ts';

/** Which coordinator source answers for each lane. */
export const LANE_SOURCE_ID: Record<LaneId, ExplorerSourceIdV1> = {
  code: 'code_graph',
  sessions: 'sessions',
  knowledge: 'knowledge',
};

/**
 * One lane's condition, as a closed union.
 *
 * A union rather than a bag of booleans because these conditions are mutually
 * exclusive and the surface must handle all of them: `hits` exist only on
 * `ready`, so no caller can count rows from a lane that never returned any,
 * and every `switch` over `state` is `never`-checked, so a new wire outcome
 * fails the build instead of silently rendering as an error.
 *
 * Run-level `partial` is deliberately absent: `ExplorerSourceOutcomeV1` has no
 * partial member, and the coordinator's own `ExplorerRunStateV1` already
 * carries that reading for the run as a whole. A lane that is still working is
 * `pending`, and it carries the wire's `phase` so `queued` and `reading` stay
 * apart.
 */
export type ExplorerLaneReadModel =
  /** Still working. `phase` is the source's own; `null` when the pendingness
   * is the client's (no coordinator answer yet, or a browse read in flight). */
  | {
      readonly state: 'pending';
      readonly lane: LaneId;
      readonly phase: ExplorerSourcePhaseV1 | null;
    }
  /** The source answered. Zero `hits` here means "answered with nothing",
   * which is a different fact from every other member of this union. */
  | {
      readonly state: 'ready';
      readonly lane: LaneId;
      readonly hits: readonly Hit[];
      /** The size of the matching set the source reported, or `null` when it
       * reported none. Never 0 as a stand-in for "unreported". */
      readonly reportedTotal: number | null;
      /** Rows the source returned that the surface could not render. Kept so a
       * dropped row shows up as a stated omission instead of shrinking the
       * result set silently. */
      readonly unreadableRows: number;
    }
  /** The source itself reported it could not serve this run. */
  | {
      readonly state: 'unavailable';
      readonly lane: LaneId;
      readonly errorCode: string | null;
      readonly detail: string | null;
    }
  /** The source's read was cancelled. */
  | {
      readonly state: 'cancelled';
      readonly lane: LaneId;
      readonly errorCode: string | null;
      readonly detail: string | null;
    }
  /** The source failed. */
  | {
      readonly state: 'error';
      readonly lane: LaneId;
      readonly errorCode: string | null;
      readonly detail: string | null;
    }
  /** The client could not reach the daemon at all. A connectivity condition on
   * this side of the wire — never a statement about the source. */
  | { readonly state: 'offline'; readonly lane: LaneId }
  /** The daemon accepted no identity for this read. */
  | { readonly state: 'unauthorized'; readonly lane: LaneId }
  /** The daemon knows the identity and will not serve this scope. */
  | { readonly state: 'denied'; readonly lane: LaneId }
  /** The response did not decode against the contract. */
  | { readonly state: 'unsupported_schema'; readonly lane: LaneId }
  /** The run reached a terminal state without ever naming this source. */
  | { readonly state: 'unanswered'; readonly lane: LaneId }
  /** The transport reported a domain state that carries no lane-level reading.
   * The state is kept verbatim rather than folded into an error. */
  | {
      readonly state: 'indeterminate';
      readonly lane: LaneId;
      readonly domainState: DashboardDomainStateV1;
    };

/**
 * The one place an Explorer result row stops being `unknown`.
 *
 * `ExplorerResultPageV1.rows` generates as `z.array(z.unknown())` because
 * `ExplorerResultPageV1` in `src/dashboard/explorer_api.rs` declares
 * `rows: Vec<serde_json::Value>`: `ready_source` fills one page shape from
 * three different producers, so the wire carries graph symbols, LCM messages
 * and summary nodes, and fact rows through the same field without ever saying
 * which. There is therefore no generated type to parse a row into, and writing
 * a zod object here would be a second wire contract that `contracts:generate`
 * never produced and `contracts:check` cannot see.
 *
 * So a row is narrowed structurally and nothing further is claimed about it:
 * a row that is not a keyed object cannot be read at all, and callers report
 * how many rows they could not read rather than quietly returning fewer.
 * Individual fields are then read through the `str`/`num` accessors in
 * `model.ts`, which yield `undefined` for an absent field so a value the
 * source did not send renders as missing rather than as a zero.
 *
 * Deleting this function needs `ExplorerResultPageV1.rows` to become a tagged
 * union in Rust — one variant per `ExplorerSourceIdV1`, wrapping the row types
 * that already generate today (`GraphNodeV1` for `CodeGraph`, `LcmMessageV1`
 * and `LcmSummaryNodeV1` for `Sessions`, `MemoryFactRowV1` for `Knowledge`).
 */
function narrowPageRows(page: ExplorerResultPageV1): Record<string, unknown>[] {
  return page.rows.filter(
    (row): row is Record<string, unknown> =>
      typeof row === 'object' && row !== null && !Array.isArray(row),
  );
}

/** Row grammar for the lane, so no caller picks the wrong normaliser. */
function hitsForLane(
  lane: LaneId,
  rows: readonly Record<string, unknown>[],
  terms: readonly string[],
): Hit[] {
  switch (lane) {
    case 'code':
      return codeHits(rows, terms);
    case 'sessions':
      return sessionHits(rows, terms);
    case 'knowledge':
      return knowledgeHits(rows, terms);
    default: {
      const exhaustive: never = lane;
      return exhaustive;
    }
  }
}

/** Whether the coordinator has stopped working on this run. */
export function runIsTerminal(state: ExplorerRunStateV1): boolean {
  switch (state) {
    case 'pending':
      return false;
    case 'completed':
    case 'partial':
    case 'cancelled':
    case 'error':
      return true;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * One coordinator-owned source, converted without manufacturing rows, totals,
 * or cross-source rank.
 */
export function laneFromSourceProgress(
  lane: LaneId,
  source: ExplorerSourceProgressV1,
  terms: readonly string[],
): ExplorerLaneReadModel {
  if (source.source_id !== LANE_SOURCE_ID[lane]) {
    // The record is addressed to another source, so it says nothing about this
    // lane. Reporting it as an error would blame this lane for a coordinator
    // routing mistake.
    return { state: 'unanswered', lane };
  }
  switch (source.outcome) {
    case 'pending':
      return { state: 'pending', lane, phase: source.phase };
    case 'ready': {
      // `page` is nullable on every outcome in the Rust type, so a `ready`
      // source that arrived without one has genuinely returned nothing to
      // show — which is not the same as an error, and not the same as never
      // having answered.
      const page = source.page;
      if (page === null) {
        return { state: 'ready', lane, hits: [], reportedTotal: null, unreadableRows: 0 };
      }
      const hits = hitsForLane(lane, narrowPageRows(page), terms);
      return {
        state: 'ready',
        lane,
        hits,
        reportedTotal: page.total,
        unreadableRows: page.rows.length - hits.length,
      };
    }
    case 'unavailable':
      return { state: 'unavailable', lane, errorCode: source.error_code, detail: source.message };
    case 'cancelled':
      return { state: 'cancelled', lane, errorCode: source.error_code, detail: source.message };
    case 'error':
      return { state: 'error', lane, errorCode: source.error_code, detail: source.message };
    default: {
      const exhaustive: never = source.outcome;
      return exhaustive;
    }
  }
}

/**
 * A transport reading, kept apart from anything a source said about itself.
 *
 * Exhaustive over the whole domain-state union rather than over the three
 * states `fetchEnvelope` emits today, so widening that helper cannot silently
 * land a new reading in the wrong lane state. The states no lane reading
 * exists for are carried verbatim as `indeterminate` instead of being rounded
 * off to an error.
 */
export function laneFromTransport(
  lane: LaneId,
  domainState: DashboardDomainStateV1,
  detail: string | null,
): ExplorerLaneReadModel {
  switch (domainState) {
    case 'loading':
      return { state: 'pending', lane, phase: null };
    case 'offline':
      return { state: 'offline', lane };
    case 'unauthorized':
      return { state: 'unauthorized', lane };
    case 'denied':
      return { state: 'denied', lane };
    case 'unsupported_schema':
      return { state: 'unsupported_schema', lane };
    case 'cancelled':
      return { state: 'cancelled', lane, errorCode: null, detail };
    case 'error':
      return { state: 'error', lane, errorCode: null, detail };
    case 'complete_zero_findings':
    case 'conflicting':
    case 'locked':
    case 'partial':
    case 'ready':
    case 'redacted':
    case 'stale':
    case 'timed_out':
    case 'unknown':
    case 'unsupported':
      return { state: 'indeterminate', lane, domainState };
    default: {
      const exhaustive: never = domainState;
      return exhaustive;
    }
  }
}

/**
 * The lane's condition during a search, read off one coordinator response.
 *
 * `submittedQuery` is checked because a run that answered for an earlier query
 * is not an answer for this one — showing its rows would attribute another
 * query's results to the text on screen.
 */
export function searchLane(
  lane: LaneId,
  result: EnvelopeResult<ExplorerQueryRunV1> | undefined,
  submittedQuery: string,
  terms: readonly string[],
): ExplorerLaneReadModel {
  if (result === undefined) return { state: 'pending', lane, phase: null };
  if (result.outcome === 'transport') {
    return laneFromTransport(lane, result.state, result.detail ?? null);
  }
  const run = result.envelope.payload;
  if (run.request.query !== submittedQuery) return { state: 'pending', lane, phase: null };
  const source = run.sources.find((candidate) => candidate.source_id === LANE_SOURCE_ID[lane]);
  if (source !== undefined) return laneFromSourceProgress(lane, source, terms);
  // The coordinator has finished and never named this source. Leaving the lane
  // on `pending` would show a spinner for a read that will never arrive.
  return runIsTerminal(run.state)
    ? { state: 'unanswered', lane }
    : { state: 'pending', lane, phase: null };
}

/**
 * The lane's condition while browsing, read off one legacy overview response.
 *
 * These endpoints predate the envelope and report no matching total, so
 * `reportedTotal` is `null` rather than the row count: the overview says what
 * it is holding, not how much there is.
 */
export function browseLane<T>(
  lane: LaneId,
  result: LegacyResult<T> | undefined,
  isPending: boolean,
  rowsOf: (data: T) => readonly Record<string, unknown>[],
  terms: readonly string[],
): ExplorerLaneReadModel {
  if (isPending) return { state: 'pending', lane, phase: null };
  if (result === undefined) return { state: 'unanswered', lane };
  switch (result.outcome) {
    case 'ok': {
      const rows = rowsOf(result.data);
      const hits = hitsForLane(lane, rows, terms);
      return {
        state: 'ready',
        lane,
        hits,
        reportedTotal: null,
        unreadableRows: rows.length - hits.length,
      };
    }
    case 'offline':
      return { state: 'offline', lane };
    case 'unauthorized':
      return { state: 'unauthorized', lane };
    case 'denied':
      return { state: 'denied', lane };
    case 'unsupported_schema':
      return { state: 'unsupported_schema', lane };
    case 'error':
      return { state: 'error', lane, errorCode: null, detail: result.detail };
    // The transport carried the source's own report of not being able to
    // serve, which is the state this lane already has for the same condition
    // arriving inside a 200 envelope.
    case 'unavailable':
      return { state: 'unavailable', lane, errorCode: result.status, detail: result.reason };
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/** Rows the lane actually delivered. Empty for every state but `ready`, where
 * emptiness means the source answered with nothing. */
export function laneHits(read: ExplorerLaneReadModel): readonly Hit[] {
  return read.state === 'ready' ? read.hits : [];
}

/** Whether the source answered. Only a `ready` lane may be counted. */
export function laneAnswered(read: ExplorerLaneReadModel): boolean {
  return read.state === 'ready';
}

/** Whether the lane is still working, and so has not failed to answer. */
export function lanePending(read: ExplorerLaneReadModel): boolean {
  return read.state === 'pending';
}

/**
 * The chip for a lane condition.
 *
 * `unavailable` and `offline` are different chips: a source the coordinator
 * reported as unable to serve is not a browser that never reached the daemon,
 * and the indicator has to say which one happened without help from the prose
 * beside it. Because the chip carries that distinction, `laneStateDetail`
 * spends its clause on the reason rather than repeating the state.
 */
export function laneStateKind(read: ExplorerLaneReadModel): DomainStateKind {
  switch (read.state) {
    case 'pending':
      return 'loading';
    case 'ready':
      return 'ready';
    case 'unavailable':
      return 'unavailable';
    case 'offline':
      return 'offline';
    case 'cancelled':
      return 'cancelled';
    case 'error':
      return 'error';
    case 'unauthorized':
      return 'unauthorized';
    case 'denied':
      return 'denied';
    case 'unsupported_schema':
      return 'unsupported_schema';
    case 'unanswered':
      return 'unknown';
    case 'indeterminate':
      return 'unknown';
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}

/** The chip for the coordinator's own reading of a whole run. */
export function runStateKind(state: ExplorerRunStateV1): DomainStateKind {
  switch (state) {
    case 'pending':
      return 'loading';
    case 'completed':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'cancelled':
      return 'cancelled';
    case 'error':
      return 'error';
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/** The chip for one source's outcome as the coordinator reported it. */
export function sourceOutcomeStateKind(outcome: ExplorerSourceOutcomeV1): DomainStateKind {
  switch (outcome) {
    case 'pending':
      return 'loading';
    case 'ready':
      return 'ready';
    case 'unavailable':
      return 'unavailable';
    case 'error':
      return 'error';
    case 'cancelled':
      return 'cancelled';
    default: {
      const exhaustive: never = outcome;
      return exhaustive;
    }
  }
}

/**
 * The clause a lane's chip carries beside it.
 *
 * The chip already names the condition, so this adds only what the chip cannot:
 * an unavailable source spends the clause on the reason the source itself
 * reported, because printing "source unavailable" next to a chip reading
 * exactly that is noise. `offline` keeps naming the daemon, which is the
 * subject of the failure rather than a second word for it.
 */
export function laneStateDetail(read: ExplorerLaneReadModel): string | undefined {
  switch (read.state) {
    case 'pending':
      return read.phase ?? undefined;
    case 'ready':
      return undefined;
    case 'unavailable':
      return read.errorCode ?? read.detail ?? undefined;
    case 'offline':
      return 'daemon unreachable';
    case 'cancelled':
      return 'read cancelled';
    case 'error':
      return read.errorCode ?? read.detail ?? undefined;
    case 'unauthorized':
    case 'denied':
    case 'unsupported_schema':
      return undefined;
    case 'unanswered':
      return 'the run never named this source';
    case 'indeterminate':
      return read.domainState;
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}

/**
 * Where a lane's quantity sits on the shared evidence axis.
 *
 * `measured` is reserved for a source that answered AND reported the size of
 * the matching set — the only case in which the number on screen has a real
 * denominator behind it. Rows without a reported total are `associated`: they
 * are genuine rows, but the surface cannot say what fraction of the truth they
 * are. Every other condition is `unknown`. `predicted` is deliberately
 * unreachable: Explorer never estimates.
 */
export function laneEvidence(read: ExplorerLaneReadModel | undefined): EvidenceQuality {
  if (read === undefined) return 'unknown';
  switch (read.state) {
    case 'ready':
      return read.reportedTotal != null ? 'measured' : 'associated';
    case 'pending':
    case 'unavailable':
    case 'offline':
    case 'cancelled':
    case 'error':
    case 'unauthorized':
    case 'denied':
    case 'unsupported_schema':
    case 'unanswered':
    case 'indeterminate':
      return 'unknown';
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}
