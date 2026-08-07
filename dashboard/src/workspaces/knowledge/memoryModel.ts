/**
 * The readings the memory panels draw, computed once and away from JSX.
 *
 * Every function here turns one wire payload into the thing a panel actually
 * states, and each of them exists because the naive rendering of that payload
 * would have said something untrue:
 *
 *   - a projection whose `method` is `none` is not a map of anything, and a
 *     scatter drawn from it looks exactly like one that is;
 *   - a similarity payload carries three different denominators (vectored
 *     facts, scored pairs, returned pairs) and reporting any one of them as
 *     "the store" misstates the other two;
 *   - an oplog row's detail is a three-way domain state, and an optional
 *     `summary` collapses a privacy redaction into a missing record;
 *   - a trust audit's repair progress reports nothing at all in two of its four
 *     states, where `0 processed` is a lie and blank is an omission.
 *
 * Pure and separately tested for the reason the Work workspace splits its
 * models out: these are the sentences the product makes, and a sentence proved
 * only through a rendered DOM is a sentence proved once.
 */
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type {
  CurationRunRecord,
  CurationRunsPayload,
  OplogDetail,
  OplogPayload,
  ProjectionPayload,
  SimilarityPayload,
  TrustDetailAvailability,
  TrustHistoryPayload,
} from '../../data/query/memory.ts';

/* ---- trust history ------------------------------------------------------- */

export interface TrustHistoryReading {
  /** Events the audit returned, newest-last as the store appended them. */
  readonly count: number;
  readonly helpful: number;
  readonly unhelpful: number;
  /** Trust before the first recorded event; `null` when there are none. */
  readonly opening: number | null;
  /** Trust after the last recorded event; `null` when there are none. */
  readonly closing: number | null;
  /** `closing - opening`, or `null` when the audit recorded nothing to net. */
  readonly net: number | null;
  /** How many events carry each detail availability, zeroes included, so the
   * panel can state "3 of 11 redacted" rather than only listing the survivors. */
  readonly availability: Readonly<Record<TrustDetailAvailability, number>>;
  /** What the panel says about how complete this audit is. */
  readonly repair: string;
}

/**
 * The repair sentence.
 *
 * Four states, four different claims about completeness, and only two of them
 * carry figures. `unknown` is the one that must never render as a clean audit:
 * a store that cannot say whether its feedback history was repaired has not
 * said the history is whole.
 */
function repairSentence(repair: TrustHistoryPayload['repair']): string {
  const processed = repair.processed;
  const remaining = repair.remaining;
  switch (repair.state) {
    case 'not_required':
      return 'this store never needed a feedback-history repair, so every event below is original';
    case 'complete':
      return processed == null
        ? 'the feedback-history repair finished; it reported no processed count'
        : `the feedback-history repair finished after ${processed.toLocaleString()} rows`;
    case 'incomplete':
      return remaining == null
        ? 'the feedback-history repair is unfinished and did not report how much is left — events may be missing'
        : `the feedback-history repair still has ${remaining.toLocaleString()} rows to go — events may be missing`;
    case 'unknown':
      return 'this store cannot say whether its feedback history was ever repaired, so the audit below may be incomplete';
    default: {
      const exhaustive: never = repair.state;
      return exhaustive;
    }
  }
}

export function trustHistoryReading(payload: TrustHistoryPayload): TrustHistoryReading {
  const events = payload.trust_history;
  const availability: Record<TrustDetailAvailability, number> = {
    available: 0,
    legacy_redacted: 0,
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
    repair: repairSentence(payload.repair),
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
    case 'legacy_redacted':
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
  /** `true` only when the daemon actually decomposed the phase vectors. */
  readonly projected: boolean;
  /** What the panel says the axes mean — or that they mean nothing. */
  readonly note: string;
  readonly points: ProjectionPayload['points'];
  /** Drawing extents, `null` when there is nothing to draw. */
  readonly extent: { x: [number, number]; y: [number, number] } | null;
  /** Categories present, ranked by population, for the legend. */
  readonly categories: readonly { category: string; count: number }[];
  /** The phase-vector width the projection ran over; `0` means no vectors. */
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
      ? `principal components of ${points.length.toLocaleString()} phase vectors of width ${payload.dim.toLocaleString()} — the axes are the two directions of greatest variance, and carry no unit`
      : points.length === 0
        ? 'no fact in this store carries a phase vector, so there is nothing to project'
        : `too few comparable phase vectors to decompose (${points.length.toLocaleString()} of width ${payload.dim.toLocaleString()}) — the positions below are placeholders, not a projection`,
    points,
    extent,
    categories,
    dim: payload.dim,
  };
}

/* ---- similarity ---------------------------------------------------------- */

export interface SimilarityReading {
  /** Facts that carried a phase vector — never the store's fact total. */
  readonly vectored: number;
  /** Pairs scored above the computation's own floor, before this request's. */
  readonly scored: number;
  /** Pairs this request actually returned, after floor and cap. */
  readonly returned: number;
  /** `true` when the return was truncated by the cap rather than the floor. */
  readonly capped: boolean;
  readonly average: number | null;
  readonly min: number | null;
  readonly max: number | null;
  /** The three denominators as one sentence, so no figure is read as another. */
  readonly denominators: string;
}

export function similarityReading(payload: SimilarityPayload): SimilarityReading {
  const distribution = payload.score_distribution;
  const returned = payload.pairs.length;
  const capped = returned >= payload.limit && payload.total_pairs > returned;
  return {
    vectored: payload.count,
    scored: payload.total_pairs,
    returned,
    capped,
    average: distribution.average_score,
    min: distribution.min_score,
    max: distribution.max_score,
    denominators:
      payload.count < 2
        ? `${payload.count.toLocaleString()} vectored fact${payload.count === 1 ? '' : 's'} — a pair needs two, so nothing was scored`
        : `${returned.toLocaleString()} shown of ${payload.total_pairs.toLocaleString()} scored pairs over ${payload.count.toLocaleString()} vectored facts, at or above ${payload.min_similarity.toFixed(2)}`,
  };
}

/* ---- curation runs ------------------------------------------------------- */

export interface CurationRunTaskSummary {
  readonly task: string;
  readonly runs: number;
  readonly accepted: number;
  readonly rejected: number;
  readonly skipped: number;
  readonly failed: number;
}

export interface CurationRunsReading {
  readonly records: readonly CurationRunRecord[];
  readonly tasks: readonly CurationRunTaskSummary[];
  /** Runs whose `status` is not `completed`; these are the ones with an error. */
  readonly failed: number;
  /** The ledger's own load failure, when it had one. Read before `records`:
   * an unreadable ledger and a project that has never run both answer `[]`. */
  readonly ledgerError: string | null;
}

/** A run counts as failed when the ledger did not record it as completed. The
 * check is on the recorded status, never on the presence of an `error` string —
 * a completed run may carry a warning, and a failed one may carry none. */
function runFailed(record: CurationRunRecord): boolean {
  return record.status !== 'completed';
}

export function curationRunsReading(payload: CurationRunsPayload): CurationRunsReading {
  const byTask = new Map<string, { accepted: number; rejected: number; skipped: number; runs: number; failed: number }>();
  let failed = 0;
  for (const record of payload.records) {
    const entry = byTask.get(record.task) ?? {
      accepted: 0,
      rejected: 0,
      skipped: 0,
      runs: 0,
      failed: 0,
    };
    entry.runs += 1;
    entry.accepted += record.accepted_count;
    entry.rejected += record.rejected_count;
    entry.skipped += record.skipped_count;
    if (runFailed(record)) {
      entry.failed += 1;
      failed += 1;
    }
    byTask.set(record.task, entry);
  }
  return {
    records: payload.records,
    tasks: [...byTask]
      .map(([task, entry]) => ({ task, ...entry }))
      .sort((a, b) => b.runs - a.runs || a.task.localeCompare(b.task)),
    failed,
    ledgerError: payload.error === '' ? null : payload.error,
  };
}

/** The chip a run's recorded status renders as. Unrecognised statuses stay
 * `unknown` rather than being folded into `error`: the ledger's vocabulary is
 * the daemon's, and inventing a verdict for a word this build has not seen is
 * exactly the fabrication the taxonomy exists to prevent. */
export function runStatusState(status: string): DomainStateKind {
  switch (status) {
    case 'completed':
      return 'ready';
    case 'failed':
      return 'error';
    case 'cancelled':
      return 'cancelled';
    case 'timed_out':
      return 'timed_out';
    case 'skipped':
      return 'unsupported';
    default:
      return 'unknown';
  }
}

/* ---- oplog --------------------------------------------------------------- */

/**
 * An oplog row's detail, resolved to what the reader is shown.
 *
 * The three arms are the three `ProjectMemoryDashboardOplogDetailsV1` variants,
 * and they stay apart all the way to the screen. A `redacted` row HAS a detail
 * that this reader is not permitted to see; an `unknown` row's detail state was
 * never recorded. Rendering both as "no detail" would claim the second is the
 * first, and claim of the first that nothing was ever written.
 */
export type OplogDetailReading =
  | { kind: 'summary'; summary: string }
  | { kind: 'state'; state: DomainStateKind; sentence: string };

export function oplogDetailReading(detail: OplogDetail): OplogDetailReading {
  if ('summary' in detail && typeof detail.summary === 'string') {
    return { kind: 'summary', summary: detail.summary };
  }
  if ('redacted' in detail) {
    return {
      kind: 'state',
      state: 'redacted',
      sentence: 'detail withheld by the privacy gate',
    };
  }
  return {
    kind: 'state',
    state: 'unknown',
    sentence: 'this row predates detail recording, so its detail state is unknown',
  };
}

export interface OplogReading {
  readonly events: OplogPayload['events'];
  /** Operations by name, ranked, for the summary rail. */
  readonly operations: readonly { op: string; count: number }[];
  readonly redacted: number;
  readonly unknownDetail: number;
  /** The store's own read failure, when it had one. */
  readonly storeError: string | null;
}

export function oplogReading(payload: OplogPayload): OplogReading {
  const counts = new Map<string, number>();
  let redacted = 0;
  let unknownDetail = 0;
  for (const event of payload.events) {
    counts.set(event.op, (counts.get(event.op) ?? 0) + 1);
    const detail = oplogDetailReading(event.detail);
    if (detail.kind === 'state' && detail.state === 'redacted') redacted += 1;
    if (detail.kind === 'state' && detail.state === 'unknown') unknownDetail += 1;
  }
  return {
    events: payload.events,
    operations: [...counts]
      .map(([op, count]) => ({ op, count }))
      .sort((a, b) => b.count - a.count || a.op.localeCompare(b.op)),
    redacted,
    unknownDetail,
    storeError: payload.error === '' ? null : payload.error,
  };
}
