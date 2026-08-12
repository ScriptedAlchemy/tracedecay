/**
 * The readings the memory panels draw, computed once and away from JSX.
 *
 * Every function here turns one wire payload into the thing a panel actually
 * states, and each of them exists because the naive rendering of that payload
 * would have said something untrue:
 *
 *   - a projection whose `method` is `none` is not a map of anything, and a
 *     scatter drawn from it looks exactly like one that is;
 *   - a similarity payload carries three different denominators (query-time
 *     encoded
 *     facts, scored pairs, returned pairs) and reporting any one of them as
 *     "the store" misstates the other two;
 *   - the oplog reports only canonical operation identity and never an inferred
 *     detail channel;
 *   - trust-history detail availability remains a finite canonical state.
 *
 * Pure and separately tested for the reason the Work workspace splits its
 * models out: these are the sentences the product makes, and a sentence proved
 * only through a rendered DOM is a sentence proved once.
 */
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type {
  OplogPayload,
  ProjectionPayload,
  SimilarityPayload,
  TrustDetailAvailability,
  TrustHistoryPayload,
} from '../../data/query/memory.ts';

/* ---- trust history ------------------------------------------------------- */

/** Formats canonical UTC microseconds only at the presentation boundary. */
export function formatUtcMicros(value: number): string {
  return new Date(Math.trunc(value / 1_000)).toISOString();
}

export interface TrustHistoryReading {
  /** Events this bounded audit response returned. */
  readonly count: number;
  readonly helpful: number;
  readonly unhelpful: number;
  /** Trust before the first returned event; `null` when there are none. */
  readonly opening: number | null;
  /** Trust after the last returned event; `null` when there are none. */
  readonly closing: number | null;
  /** `closing - opening` over returned rows; `null` when none were returned. */
  readonly net: number | null;
  /** How many events carry each detail availability, zeroes included, so the
   * panel can state "3 of 11 redacted" rather than only listing the survivors. */
  readonly availability: Readonly<Record<TrustDetailAvailability, number>>;
}

export function trustHistoryReading(payload: TrustHistoryPayload): TrustHistoryReading {
  const events = payload.trust_history;
  const availability: Record<TrustDetailAvailability, number> = {
    available: 0,
    redacted: 0,
    unknown: 0,
  };
  let helpful = 0;
  let unhelpful = 0;
  for (const event of events) {
    availability[event.details_availability] += 1;
    if (event.action === 'helpful') helpful += 1;
    else unhelpful += 1;
  }
  const first = events[0];
  const last = events[events.length - 1];
  const opening = first ? first.old_trust : null;
  const closing = last ? last.new_trust : null;
  return {
    count: events.length,
    helpful,
    unhelpful,
    opening,
    closing,
    net: opening == null || closing == null ? null : closing - opening,
    availability,
  };
}

/** The state a feedback event's detail is in. `available` is not a state chip —
 * the detail is simply shown — so this is only called for the other two. */
export function trustDetailState(
  availability: TrustDetailAvailability,
): DomainStateKind | null {
  switch (availability) {
    case 'available':
      return null;
    case 'redacted':
      return 'redacted';
    case 'unknown':
      return 'unknown';
    default: {
      const exhaustive: never = availability;
      return exhaustive;
    }
  }
}

/* ---- projection ---------------------------------------------------------- */

export interface ProjectionReading {
  /** `true` only when the daemon decomposed query-time-derived phase encodings. */
  readonly projected: boolean;
  /** What the panel says the axes mean — or that they mean nothing. */
  readonly note: string;
  readonly points: ProjectionPayload['points'];
  /** Drawing extents, `null` when there is nothing to draw. */
  readonly extent: { x: [number, number]; y: [number, number] } | null;
  /** Categories present, ranked by population, for the legend. */
  readonly categories: readonly { category: string; count: number }[];
  /** The derived phase-encoding width; `0` means no fact could be encoded. */
  readonly dim: number;
}

export function projectionReading(payload: ProjectionPayload): ProjectionReading {
  const points = payload.points;
  const projected = payload.method === 'pca' && points.length >= 2;
  const counts = new Map<string, number>();
  for (const point of points) {
    counts.set(point.category, (counts.get(point.category) ?? 0) + 1);
  }
  const categories = [...counts]
    .map(([category, count]) => ({ category, count }))
    .sort((a, b) => b.count - a.count || a.category.localeCompare(b.category));
  let extent: ProjectionReading['extent'] = null;
  if (points.length > 0) {
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    for (const point of points) {
      minX = Math.min(minX, point.x);
      maxX = Math.max(maxX, point.x);
      minY = Math.min(minY, point.y);
      maxY = Math.max(maxY, point.y);
    }
    extent = { x: [minX, maxX], y: [minY, maxY] };
  }
  return {
    projected,
    note: projected
      ? `principal components of ${points.length.toLocaleString()} query-time-derived phase encodings returned by a request bounded to ${payload.limit.toLocaleString()} facts, of width ${payload.dim.toLocaleString()} — the axes are the two directions of greatest variance, and carry no unit`
      : points.length === 0
        ? payload.coverage.completeness === 'complete'
          ? `the complete eligible set returned no phase encodings, so there is nothing to project`
          : `this ${payload.coverage.completeness} request, bounded to ${payload.limit.toLocaleString()} facts, returned no phase encodings; whole-store coverage is unknown`
        : `too few comparable query-time-derived phase encodings to decompose (${points.length.toLocaleString()} of width ${payload.dim.toLocaleString()}) — the positions below are placeholders, not a projection`,
    points,
    extent,
    categories,
    dim: payload.dim,
  };
}

/* ---- similarity ---------------------------------------------------------- */

export interface SimilarityReading {
  /** Facts successfully encoded on read — never the store's fact total. */
  readonly encoded: number;
  /** Pairs scored above the computation's own floor, before this request's. */
  readonly scored: number;
  /** Pairs this request actually returned, after floor and cap. */
  readonly returned: number;
  /**
   * Threshold-match coverage at the request cap. `false` proves the response
   * ended before the cap; `null` means the response filled the cap and the
   * endpoint cannot distinguish an exact fit from a truncated result.
   */
  readonly capped: boolean | null;
  readonly average: number | null;
  readonly min: number | null;
  readonly max: number | null;
  /** The three denominators as one sentence, so no figure is read as another. */
  readonly denominators: string;
}

export function similarityReading(payload: SimilarityPayload): SimilarityReading {
  const distribution = payload.score_distribution;
  const returned = payload.pairs.length;
  const capped = returned < payload.limit ? false : null;
  return {
    encoded: payload.count,
    scored: payload.total_pairs,
    returned,
    capped,
    average: distribution.average_score,
    min: distribution.min_score,
    max: distribution.max_score,
    denominators:
      payload.count < 2
        ? `${payload.count.toLocaleString()} query-time encoded fact${payload.count === 1 ? '' : 's'} — a pair needs two, so nothing was scored`
        : `${returned.toLocaleString()} pairs shown at or above ${payload.min_similarity.toFixed(2)}; ${payload.total_pairs.toLocaleString()} finite pairs scored globally over ${payload.count.toLocaleString()} query-time encoded facts`,
  };
}

/* ---- oplog --------------------------------------------------------------- */

export interface OplogReading {
  readonly events: OplogPayload['events'];
  /** Operations by name, ranked, for the summary rail. */
  readonly operations: readonly { op: string; count: number }[];
  /** The store's own read failure, when it had one. */
  readonly storeError: string | null;
}

export function oplogReading(payload: OplogPayload): OplogReading {
  const counts = new Map<string, number>();
  for (const event of payload.events) {
    counts.set(event.op, (counts.get(event.op) ?? 0) + 1);
  }
  return {
    events: payload.events,
    operations: [...counts]
      .map(([op, count]) => ({ op, count }))
      .sort((a, b) => b.count - a.count || a.op.localeCompare(b.op)),
    storeError: payload.error === '' ? null : payload.error,
  };
}
