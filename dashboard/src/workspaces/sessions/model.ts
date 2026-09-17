/**
 * Sessions — the pure model under the workspace.
 *
 * Everything here is a deterministic transform of wire records the daemon
 * already served: URL state, index paging, the typed extent of a session row,
 * the join from a selection to its index row, and the fixed presentation of the
 * daemon's Git-evidence classes on the evidence-grade ladder. Nothing fetches,
 * nothing invents a timestamp, and no absence is coerced into a zero.
 */
import {
  assertNever,
  type LcmTimelineBucketV1,
  type LcmTimelineUndatedV1,
  type LoomBranchSpanV1,
  type LoomCommitV1,
  type LoomEditedFileV1,
  type LoomSessionRowV1,
  type LoomSourceStatusV1,
  type LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import type { EvidenceGrade } from '../../ui/EvidenceGrade.tsx';

/* ------------------------------------------------------------------------ *
 * Selection identity
 * ------------------------------------------------------------------------ */

/**
 * A selected session, provider-qualified when the selecting record carried a
 * provider. The retained session store keys sessions by `(provider,
 * session_id)`; a transcript search hit carries the provider as `source`,
 * which the contract leaves nullable — so a selection may know the id and not
 * the provider, and the join below says so rather than guessing one.
 */
export interface SessionSelection {
  readonly provider: string | null;
  readonly sessionId: string;
}

export function selectionKey(selection: SessionSelection): string {
  return `${selection.provider ?? ''}\u0000${selection.sessionId}`;
}

export function sameSelection(
  left: SessionSelection | null,
  right: SessionSelection | null,
): boolean {
  if (left === null || right === null) return left === right;
  return left.sessionId === right.sessionId && left.provider === right.provider;
}

/* ------------------------------------------------------------------------ *
 * URL state
 * ------------------------------------------------------------------------ */

export type TimelineBucket = 'day' | 'hour';

/** Rows the index reads per page. Every value is a real `limit` the route
 * accepts (`loom_api.rs` clamps to 1..=500). */
export const ROWS_PER_PAGE_OPTIONS = [25, 50, 100] as const;
export type RowsPerPage = (typeof ROWS_PER_PAGE_OPTIONS)[number];

/**
 * How many most-recent dated buckets the timeline read keeps. These are the
 * route's own `limit` (`lcm_api.rs` clamps to 1..=2000); they are NOT calendar
 * windows, because the store returns only buckets that hold messages, so
 * thirty buckets can span far more than thirty days. The UI words them as
 * bucket counts for exactly that reason.
 */
export const TIMELINE_WINDOW_OPTIONS = [30, 90, 400, 2000] as const;
export type TimelineWindow = (typeof TIMELINE_WINDOW_OPTIONS)[number];
/** The route's own default when no `limit` is sent. */
export const DEFAULT_TIMELINE_WINDOW: TimelineWindow = 400;

export interface SessionsViewState {
  readonly selection: SessionSelection | null;
  /** 1-based index page. */
  readonly page: number;
  readonly rows: RowsPerPage;
  readonly bucket: TimelineBucket;
  readonly window: TimelineWindow;
}

const PARAM = {
  sessionId: 'sessionId',
  provider: 'sessionProvider',
  page: 'sessionsPage',
  rows: 'sessionsRows',
  bucket: 'sessionsBucket',
  window: 'sessionsWindow',
} as const;

export const DEFAULT_VIEW_STATE: SessionsViewState = {
  selection: null,
  page: 1,
  rows: 25,
  bucket: 'day',
  window: DEFAULT_TIMELINE_WINDOW,
};

function oneOf<T extends number>(raw: string | null, options: readonly T[], fallback: T): T {
  if (raw === null) return fallback;
  const parsed = Number(raw);
  return (options as readonly number[]).includes(parsed) ? (parsed as T) : fallback;
}

export function readViewState(params: URLSearchParams): SessionsViewState {
  const sessionId = params.get(PARAM.sessionId);
  const provider = params.get(PARAM.provider);
  const rawPage = Number(params.get(PARAM.page) ?? '1');
  const rawBucket = params.get(PARAM.bucket);
  return {
    selection:
      sessionId !== null && sessionId !== ''
        ? { sessionId, provider: provider === null || provider === '' ? null : provider }
        : null,
    page: Number.isInteger(rawPage) && rawPage >= 1 ? rawPage : 1,
    rows: oneOf(params.get(PARAM.rows), ROWS_PER_PAGE_OPTIONS, DEFAULT_VIEW_STATE.rows),
    bucket: rawBucket === 'hour' ? 'hour' : 'day',
    window: oneOf(params.get(PARAM.window), TIMELINE_WINDOW_OPTIONS, DEFAULT_TIMELINE_WINDOW),
  };
}

/**
 * Writes the view state onto a copy of the given params, leaving every param
 * this workspace does not own (scope, other workspaces) untouched. Defaults
 * are written as absence so a pristine view has a pristine URL.
 */
export function writeViewState(
  params: URLSearchParams,
  state: SessionsViewState,
): URLSearchParams {
  const next = new URLSearchParams(params);
  const set = (key: string, value: string | null) => {
    if (value === null) next.delete(key);
    else next.set(key, value);
  };
  set(PARAM.sessionId, state.selection?.sessionId ?? null);
  set(PARAM.provider, state.selection?.provider ?? null);
  set(PARAM.page, state.page === 1 ? null : String(state.page));
  set(PARAM.rows, state.rows === DEFAULT_VIEW_STATE.rows ? null : String(state.rows));
  set(PARAM.bucket, state.bucket === 'day' ? null : state.bucket);
  set(PARAM.window, state.window === DEFAULT_TIMELINE_WINDOW ? null : String(state.window));
  return next;
}

/* ------------------------------------------------------------------------ *
 * Index paging
 * ------------------------------------------------------------------------ */

export interface PageBounds {
  /** 1-based ordinal of the first loaded row, or null when the page is empty. */
  readonly first: number | null;
  readonly last: number | null;
  /** Page count against the store total, or null when the total is unknown. */
  readonly pageCount: number | null;
}

export function pageBounds(page: number, rows: number, loaded: number, total: number | null): PageBounds {
  const offset = (page - 1) * rows;
  const first = loaded > 0 ? offset + 1 : null;
  const last = loaded > 0 ? offset + loaded : null;
  const pageCount = total === null ? null : Math.max(1, Math.ceil(total / rows));
  return { first, last, pageCount };
}

export function pageOffset(page: number, rows: number): number {
  return (page - 1) * rows;
}

/* ------------------------------------------------------------------------ *
 * Session extent — typed from the recorded fields, never from the clock
 * ------------------------------------------------------------------------ */

export type SessionExtent =
  /** The row carries no start; it cannot be placed on the time field. */
  | { kind: 'undated' }
  /** The provider recorded an end. */
  | { kind: 'ended'; start: number; end: number }
  /** No recorded end; the last dated message bounds what is known. */
  | { kind: 'open'; start: number; last: number }
  /** No recorded end and no dated message after the start. */
  | { kind: 'open_unobserved'; start: number };

export function sessionExtent(row: LoomSessionRowV1): SessionExtent {
  if (row.started_at == null) return { kind: 'undated' };
  if (row.ended_at != null) return { kind: 'ended', start: row.started_at, end: row.ended_at };
  if (row.last_message_at != null) {
    return { kind: 'open', start: row.started_at, last: row.last_message_at };
  }
  return { kind: 'open_unobserved', start: row.started_at };
}

/** Recorded model identities on a row, deduplicated, with the null
 * (unrecorded) entries counted separately so "unrecorded" is a statement
 * about the store rather than a blank. */
export function recordedModels(row: LoomSessionRowV1): { models: string[]; unrecorded: number } {
  const models = new Set<string>();
  let unrecorded = 0;
  for (const entry of row.models) {
    if (entry.model === null || entry.model === '') unrecorded += 1;
    else models.add(entry.model);
  }
  return { models: [...models], unrecorded };
}

/* ------------------------------------------------------------------------ *
 * Selection → index row join
 * ------------------------------------------------------------------------ */

export type IndexJoin =
  | { kind: 'exact'; row: LoomSessionRowV1 }
  /** More than one loaded row answers to the id; a selection without a
   * provider cannot pick between providers and does not. */
  | { kind: 'ambiguous'; rows: readonly LoomSessionRowV1[] }
  /** The loaded index page holds no such row. Not "the session does not
   * exist" — the page is one window over the store. */
  | { kind: 'absent' };

export function joinIndexRow(
  rows: readonly LoomSessionRowV1[],
  selection: SessionSelection,
): IndexJoin {
  const candidates = rows.filter(
    (row) =>
      row.session_id === selection.sessionId &&
      (selection.provider === null || row.provider === selection.provider),
  );
  if (candidates.length === 1) return { kind: 'exact', row: candidates[0]! };
  if (candidates.length > 1) return { kind: 'ambiguous', rows: candidates };
  return { kind: 'absent' };
}

/* ------------------------------------------------------------------------ *
 * Git relations for one provider-qualified session
 * ------------------------------------------------------------------------ */

export interface SessionRelations {
  readonly commits: readonly LoomCommitV1[];
  readonly editedFiles: readonly LoomEditedFileV1[];
  readonly branchSpans: readonly LoomBranchSpanV1[];
  readonly commitStatus: LoomSourceStatusV1 | null;
  readonly fileStatus: LoomSourceStatusV1 | null;
  readonly branchStatus: LoomSourceStatusV1 | null;
}

/** The daemon's own source ids (`loom_api.rs`). */
export const SOURCE_STATUS_ID = {
  commit: 'session_commit',
  file: 'session_file',
  branch: 'branch_worktree',
} as const;

export function relationsFor(
  payload: LoomTemporalPayloadV1,
  provider: string,
  sessionId: string,
): SessionRelations {
  const owns = (record: { provider: string; session_id: string }) =>
    record.provider === provider && record.session_id === sessionId;
  const status = (id: string) => payload.source_statuses.find((s) => s.id === id) ?? null;
  return {
    commits: payload.commits.filter(owns),
    editedFiles: payload.edited_files.filter(owns),
    branchSpans: payload.branch_spans.filter(owns),
    commitStatus: status(SOURCE_STATUS_ID.commit),
    fileStatus: status(SOURCE_STATUS_ID.file),
    branchStatus: status(SOURCE_STATUS_ID.branch),
  };
}

/* ------------------------------------------------------------------------ *
 * Evidence grades
 * ------------------------------------------------------------------------ */

/** The daemon's closed commit-evidence vocabulary
 * (`tracedecay_sessions::runtime::git_correlation::CommitEvidence`). */
export type CommitEvidenceClass =
  | 'tool_result'
  | 'host_event'
  | 'head_observation'
  | 'reflog_overlap'
  | 'time_overlap';

const COMMIT_EVIDENCE: Record<CommitEvidenceClass, { grade: EvidenceGrade; sourceClass: string }> = {
  tool_result: { grade: 'EXACT', sourceClass: 'TOOL RESULT' },
  host_event: { grade: 'EXACT', sourceClass: 'HOST EVENT' },
  head_observation: { grade: 'EXACT', sourceClass: 'OBSERVED HEAD' },
  reflog_overlap: { grade: 'INFERRED', sourceClass: 'REFLOG OVERLAP' },
  time_overlap: { grade: 'INFERRED', sourceClass: 'TIME OVERLAP' },
};

/**
 * The fixed presentation of one commit attribution on the ladder.
 *
 * The daemon states how it knows (`evidence`) and what it claims
 * (`relation`); this maps the *how* onto the ladder and never upgrades it: a
 * commit recorded by the tool that made it, or by the host, or seen as HEAD
 * during the session is a direct source fact; a reflog or time overlap is a
 * correlation and stays `INFERRED` whatever its `confidence` says. An evidence
 * class this build does not know is returned ungraded rather than guessed.
 */
export function commitEvidence(
  commit: Pick<LoomCommitV1, 'evidence'>,
): { grade: EvidenceGrade; sourceClass: string } | null {
  const evidence = commit.evidence as CommitEvidenceClass;
  switch (evidence) {
    case 'tool_result':
    case 'host_event':
    case 'head_observation':
    case 'reflog_overlap':
    case 'time_overlap':
      return COMMIT_EVIDENCE[evidence];
    default: {
      // Not `assertNever`: the wire field is an open string and an unknown
      // class is a contract drift to report, not a crash.
      const unknown: never = evidence;
      void unknown;
      return null;
    }
  }
}

/* ------------------------------------------------------------------------ *
 * Timeline buckets
 * ------------------------------------------------------------------------ */

const SECONDS_PER_DAY = 86_400;
const SECONDS_PER_HOUR = 3_600;

/**
 * The server's bucket key for an epoch-second instant
 * (`lcm_api/aggregates.rs::utc_bucket`): UTC, `YYYY-MM-DD` for days and
 * `YYYY-MM-DDTHH:00` for hours. Reproduced here so an index row can be placed
 * on the loaded time field without a second timestamp being invented for it.
 */
export function bucketKeyFor(epochSeconds: number, bucket: TimelineBucket): string {
  const date = new Date(Math.floor(epochSeconds / SECONDS_PER_DAY) * SECONDS_PER_DAY * 1000);
  const day = date.toISOString().slice(0, 10);
  switch (bucket) {
    case 'day':
      return day;
    case 'hour': {
      const seconds = ((epochSeconds % SECONDS_PER_DAY) + SECONDS_PER_DAY) % SECONDS_PER_DAY;
      const hour = Math.floor(seconds / SECONDS_PER_HOUR);
      return `${day}T${String(hour).padStart(2, '0')}:00`;
    }
    default:
      return assertNever(bucket);
  }
}

/** Parses a server bucket key back to its UTC start instant, or null when the
 * key is not one this build understands. */
export function bucketStart(key: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}):00)?$/.exec(key);
  if (!match) return null;
  const [, year, month, day, hour] = match;
  const millis = Date.UTC(
    Number(year),
    Number(month) - 1,
    Number(day),
    hour === undefined ? 0 : Number(hour),
  );
  return Number.isFinite(millis) ? millis / 1000 : null;
}

export interface ProvenanceTally {
  /** Messages the store counted with a named tokenizer. */
  readonly known: number;
  /** Messages whose token count the store disclaims. */
  readonly unknown: number;
  /** Dated messages in the loaded buckets. */
  readonly dated: number;
  /** Messages the store holds without a timestamp; separate from the field. */
  readonly undated: number;
}

export function provenanceTally(
  buckets: readonly LcmTimelineBucketV1[],
  undated: LcmTimelineUndatedV1,
): ProvenanceTally {
  let known = undated.known_message_count;
  let unknown = undated.unknown_message_count;
  let dated = 0;
  for (const bucket of buckets) {
    known += bucket.known_message_count;
    unknown += bucket.unknown_message_count;
    dated += bucket.count;
  }
  return { known, unknown, dated, undated: undated.count };
}
