import type {
  AuthorizedWorkProductScopeV1,
  WorkAttemptStateV1,
  WorkGraphReadV1,
  WorkGraphTimelineCoverageV1,
  WorkGraphVersionEntryV1,
  WorkRuntimeProjectionV1,
} from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from './workApi.ts';
import type { WorkChannel } from './workChannel.ts';

/**
 * The work-product graph read: what `operation.work.views` said, reduced to the
 * one graph version the projections' channels are taken from.
 *
 * `WorkGraphReadV1` is the third read behind the four plan 11c Work
 * projections. One immutable graph version and the whole
 * `WorkProductProjectionBundleV1` derived from that same version: declared
 * effort and the effort-weighted critical path, the gating edge set, the
 * declared causal candidates, the per-task timeline instants, the workload
 * figures, and the live runtime attempt projection with its own coverage.
 * `workViewsModel.ts` explains how each projection binds these channels beside
 * the snapshot- and attempt-fed ones.
 */

/**
 * What `operation.work.views` said, or the reason it said nothing.
 *
 * Shaped like `WorkAttemptReading` on purpose: a read is pending, refused, or
 * answered, and there is no fourth case and no empty default. The one
 * difference is that this read has no typed `absent` — the daemon conceals
 * absence and denial behind one 404, which `workRefusal` reports as `denied`,
 * so an absence arrives here as a refusal wearing that state rather than as an
 * answer.
 */
export type WorkGraphReading =
  | { readonly state: 'pending' }
  | { readonly state: 'refused'; readonly chip: DomainStateKind; readonly detail: string }
  | { readonly state: 'read'; readonly page: WorkGraphPage };

/**
 * One answered graph read, reduced to the version its channels are taken from.
 *
 * `WorkGraphReadV1` is tagged by the mode it was asked in. `current` and
 * `as_of` carry exactly one `snapshot` entry; `evolution` and `forensic` carry
 * a timeline of them plus the coverage that timeline was read under. Every
 * channel below is a property of ONE graph version, so a timeline read is
 * reduced to its newest entry and the rest of the timeline is reported as
 * `entries` and `coverage` rather than folded into a channel — an average over
 * versions would be a number no version holds.
 *
 * `entry` is null exactly when a timeline came back with no entries in it. That
 * is a SUCCESS: a complete coverage of zero returned entries means the read
 * reached the authority and the authority had no version in the window. The
 * channels are absent under it, but absent with `complete_zero_findings`, never
 * with a failure state.
 */
export interface WorkGraphPage {
  readonly mode: WorkGraphReadV1['mode'];
  readonly scope: AuthorizedWorkProductScopeV1;
  readonly entry: WorkGraphVersionEntryV1 | null;
  /** How many versions the read carried: one for the two snapshot modes, the
   * length of the timeline for the other two. */
  readonly entries: number;
  /** The timeline's own coverage, or null for a snapshot mode — which returns
   * one version by construction and has no window to be partial over. */
  readonly coverage: WorkGraphTimelineCoverageV1 | null;
}

/** The newest entry of a timeline, by the instant it was valid at. The
 * authority's ordering is not restated as an assumption: the entry is chosen by
 * comparing the versions themselves. */
function newestEntry(
  entries: readonly WorkGraphVersionEntryV1[],
): WorkGraphVersionEntryV1 | null {
  let newest: WorkGraphVersionEntryV1 | null = null;
  for (const entry of entries) {
    if (newest === null || entry.valid_at > newest.valid_at) newest = entry;
  }
  return newest;
}

/**
 * The graph read as a reading.
 *
 * `undefined` is the request still being in flight — or never issued, because a
 * disabled query has no data — which is distinct from every answer the daemon
 * can give.
 */
export function workGraphReading(
  result: WorkResult<WorkGraphReadV1> | undefined,
): WorkGraphReading {
  if (result === undefined) return { state: 'pending' };
  if (result.outcome === 'refused') {
    return { state: 'refused', chip: result.state, detail: result.detail };
  }
  const read = result.value;
  switch (read.mode) {
    case 'current':
    case 'as_of':
      return {
        state: 'read',
        page: {
          mode: read.mode,
          scope: read.authorized_scope,
          entry: read.snapshot,
          entries: 1,
          coverage: null,
        },
      };
    case 'evolution':
    case 'forensic':
      return {
        state: 'read',
        page: {
          mode: read.mode,
          scope: read.authorized_scope,
          entry: newestEntry(read.timeline.entries),
          entries: read.timeline.entries.length,
          coverage: read.timeline.coverage,
        },
      };
    default: {
      const unhandled: never = read;
      return unhandled;
    }
  }
}

/** The page a reading carries, or `null` for the two states that carry none. */
export function graphPageOf(reading: WorkGraphReading): WorkGraphPage | null {
  return reading.state === 'read' ? reading.page : null;
}

/** The graph version every channel below is derived from, or `null` when this
 * read has not produced one. */
export function graphEntryOf(reading: WorkGraphReading): WorkGraphVersionEntryV1 | null {
  return reading.state === 'read' ? reading.page.entry : null;
}

/**
 * Why a channel the graph read would have supplied has no value.
 *
 * Distinct from `channelGap` for the same reason `attemptChannelGap` is: the
 * contract carries these measurements, so reporting them as
 * `unsupported_schema` would tell a reader the build cannot do something it
 * can. Each case is the state the read actually returned, in that state's own
 * words — including the one that is not a failure at all.
 */
export function graphChannelGap(
  reading: WorkGraphReading,
  measure: string,
): WorkChannel<never> {
  switch (reading.state) {
    case 'pending':
      return {
        available: false,
        state: 'loading',
        detail: `the work-product graph read has not answered yet, so ${measure} is not drawn`,
      };
    case 'refused':
      return {
        available: false,
        state: reading.chip,
        detail: `${measure} is read from the work-product graph, and that read was refused: ${reading.detail}`,
      };
    case 'read':
      // The honest success with nothing in it. The read reached the authority
      // and the authority held no version in the window, which is a fact about
      // the window rather than a failure of the read.
      return {
        available: false,
        state: 'complete_zero_findings',
        detail: `the work-product graph read returned no graph version at all, so there is no version for ${measure} to be a property of — this is the authority reporting an empty window, not a read that failed`,
      };
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

/**
 * A channel over DECLARED graph data: present whenever a version answered.
 *
 * Deliberately unlike `attemptChannel`, which treats an empty page as an
 * absence. There the rows ARE the measurement, so no rows is nothing measured.
 * Here the measurement is what the graph declares, and declaring nothing is an
 * answer: an empty gating-edge set means the plan gates nothing, an empty
 * candidate set means nobody nominated a cause. Collapsing those into absences
 * would lose the difference between "the plan says none" and "we could not
 * ask".
 */
export function graphChannel<T>(
  reading: WorkGraphReading,
  measure: string,
  value: T | null,
): WorkChannel<T> {
  if (value === null) return graphChannelGap(reading, measure);
  return { available: true, value };
}

/** The effort-weighted critical path: the chain the authority weighted, and
 * what it weighed. */
export interface WorkCriticalPathReading {
  readonly taskIds: readonly string[];
  readonly totalEffort: number;
}

/** Declared effort mass, and the runtime split of it when one could be taken. */
export interface WorkEffortMassReading {
  readonly total: number;
  /**
   * Ready, running and blocked effort are measured against live attempt state,
   * so the authority returns them only under COMPLETE runtime coverage and
   * returns nothing at all otherwise (`work_product_projection.rs` gates all
   * three on `runtime_complete`). Absent here therefore means the split could
   * not be taken — never that nothing is ready.
   */
  readonly split: WorkChannel<WorkEffortSplitReading>;
}

export interface WorkEffortSplitReading {
  readonly ready: number;
  readonly running: number;
  readonly blocked: number;
}

/** What was asked for against what is actually running. Gated on complete
 * runtime coverage by the same authority rule as the effort split. */
export interface WorkConcurrencyReading {
  readonly requested: number;
  readonly actual: number;
}

export interface WorkTimelineInstantReading {
  readonly taskId: string;
  readonly createdAt: number;
  readonly updatedAt: number;
  readonly scheduledAt: number | null;
  readonly deadline: number | null;
}

export interface WorkChurnEntry {
  readonly taskId: string;
  readonly updatedAt: number;
  /** Microseconds between the recorded update and the instant this read was
   * taken at. A measurement, not a bucket. */
  readonly age: number;
}

export interface WorkChurnReading {
  /** The instant the graph version was observed at, which is the only clock
   * "recent" can be recent against. */
  readonly observedAt: number;
  readonly window: number;
  /** Entries updated within `window` before `observedAt`, freshest first. */
  readonly recent: readonly WorkChurnEntry[];
  /** Timeline entries the window was measured over, so a small `recent` can be
   * told from a small graph. */
  readonly counted: number;
  /**
   * Entries whose recorded update is LATER than the instant this read was taken
   * at. Neither recent nor stale: the two instants disagree, which is a reading
   * about the read and not something to round away into the window.
   */
  readonly ahead: number;
}

export interface WorkRuntimeAttemptReading {
  readonly attemptId: string;
  readonly taskId: string;
  readonly runId: string;
  readonly state: WorkAttemptStateV1;
}

export interface WorkRuntimeReading {
  readonly attempts: readonly WorkRuntimeAttemptReading[];
  /** False when the projection could read only some of the attempts. Every
   * count taken off `attempts` is then a floor. */
  readonly complete: boolean;
  /** How many attempts the projection knows of and could not read. */
  readonly unavailable: number;
  readonly observedAt: number;
}

/** How far back "recent" reaches, in microseconds: one day.
 *
 * A rendering parameter rather than a measurement, and printed as one. The
 * measurement is `WorkChurnEntry.age`, which is the real distance between an
 * update and the instant the graph was read at; the window only decides which
 * of those distances the view lists. */
export const WORK_CHURN_WINDOW_MICROS = 24 * 60 * 60 * 1_000_000;

export function criticalPathReading(entry: WorkGraphVersionEntryV1): WorkCriticalPathReading {
  const path = entry.projections.critical_path;
  return { taskIds: path.task_ids, totalEffort: path.total_effort };
}

export function effortSplit(entry: WorkGraphVersionEntryV1): WorkChannel<WorkEffortSplitReading> {
  const { ready_effort: ready, running_effort: running, blocked_effort: blocked } =
    entry.projections.workload;
  if (ready === null || running === null || blocked === null) {
    return {
      available: false,
      state: 'partial',
      detail:
        'the graph version answered its total declared effort but no ready/running/blocked split: the authority takes that split against live attempt state and withholds it unless the runtime projection covered every attempt, so this is effort that could not be apportioned rather than effort that is all idle',
    };
  }
  return { available: true, value: { ready, running, blocked } };
}

export function concurrencyReading(
  entry: WorkGraphVersionEntryV1,
): WorkChannel<WorkConcurrencyReading> {
  const { requested_concurrency: requested, actual_concurrency: actual } =
    entry.projections.workload;
  if (requested === null || actual === null) {
    return {
      available: false,
      state: 'partial',
      detail:
        'neither concurrency figure was answered: both are counted against live attempt state and the authority withholds them unless the runtime projection covered every attempt, so this is concurrency that could not be counted rather than a graph running nothing',
    };
  }
  return { available: true, value: { requested, actual } };
}

export function timelineInstants(
  entry: WorkGraphVersionEntryV1,
): readonly WorkTimelineInstantReading[] {
  return entry.projections.timeline.entries
    .map((row) => ({
      taskId: row.task_id,
      createdAt: row.created_at,
      updatedAt: row.updated_at,
      scheduledAt: row.scheduled_at,
      deadline: row.deadline,
    }))
    .sort((a, b) => b.updatedAt - a.updatedAt || a.taskId.localeCompare(b.taskId));
}

export function churnReading(entry: WorkGraphVersionEntryV1, window: number): WorkChurnReading {
  const observedAt = entry.observed_at;
  const recent: WorkChurnEntry[] = [];
  let ahead = 0;
  for (const row of entry.projections.timeline.entries) {
    const age = observedAt - row.updated_at;
    if (age < 0) {
      ahead += 1;
      continue;
    }
    if (age > window) continue;
    recent.push({ taskId: row.task_id, updatedAt: row.updated_at, age });
  }
  return {
    observedAt,
    window,
    recent: recent.sort((a, b) => a.age - b.age || a.taskId.localeCompare(b.taskId)),
    counted: entry.projections.timeline.entries.length,
    ahead,
  };
}

/**
 * The live runtime projection, and the one coverage state that is not a value.
 *
 * `unavailable` coverage means the attempts could not be measured at all. It is
 * NOT zero attempts, and the two must never render alike: an empty attempt list
 * under `complete` coverage is the authority stating that nothing is running,
 * which is a reading a reader can act on.
 */
export function runtimeReading(runtime: WorkRuntimeProjectionV1): WorkChannel<WorkRuntimeReading> {
  const coverage = runtime.coverage;
  if (coverage.coverage === 'unavailable') {
    return {
      available: false,
      state: 'unavailable',
      detail:
        'the runtime projection could not be read at all, so the attempts under this graph version are unmeasured — this is not a reading of zero attempts, and nothing about what is running follows from it',
    };
  }
  return {
    available: true,
    value: {
      attempts: runtime.attempts
        .map((attempt) => ({
          attemptId: attempt.identity.attempt_id,
          taskId: attempt.identity.task_id,
          runId: attempt.identity.run_id,
          state: attempt.state,
        }))
        .sort(
          (a, b) => a.taskId.localeCompare(b.taskId) || a.attemptId.localeCompare(b.attemptId),
        ),
      complete: coverage.coverage === 'complete',
      unavailable: coverage.coverage === 'partial' ? coverage.unavailable_attempts.length : 0,
      observedAt: runtime.observed_at,
    },
  };
}
