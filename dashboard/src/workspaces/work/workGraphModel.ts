import type {
  AuthorizedWorkProductScopeV1,
  WorkAttemptStateV1,
  WorkGraphReadV1,
  WorkGraphTimelineCoverageV1,
  WorkGraphVersionEntryV1,
  WorkRuntimeAttemptProjectionV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from './workApi.ts';
import type { WorkChannel } from './workChannel.ts';

/**
 * The work-product graph read: what `operation.work.views` said, reduced to the
 * one graph version the projections' channels are taken from.
 *
 * `WorkGraphReadV1` is one immutable graph version and the whole
 * `WorkProductProjectionBundleV1` derived from that same version: declared
 * effort and the effort-weighted critical path, the gating edge set, and the
 * live runtime attempt projection with its own coverage. `workViewsModel.ts`
 * binds the DAG's channels to it.
 */

/**
 * What `operation.work.views` said, or the reason it said nothing.
 *
 * A read is pending, refused, or
 * answered, and there is no fourth case and no empty default. An authorized
 * selection with no graph yet is an answer, the daemon's `absent` mode, and
 * arrives as a read whose page carries no entry; only a real denial arrives as
 * a refusal.
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
 * `entries` and `coverage` rather than folded into a channel, an average over
 * versions would be a number no version holds.
 *
 * `entry` is null exactly when a timeline came back with no entries in it, or
 * the read answered `absent` because no graph exists yet. Both are SUCCESSES:
 * the read reached the authority and the authority had no version to serve.
 * The channels are absent under them, but absent with `complete_zero_findings`,
 * never with a failure state.
 */
interface WorkGraphPage {
  readonly mode: WorkGraphReadV1['mode'];
  readonly scope: AuthorizedWorkProductScopeV1;
  readonly entry: WorkGraphVersionEntryV1 | null;
  /** How many versions the read carried: one for the two snapshot modes, the
   * length of the timeline for the other two. */
  readonly entries: number;
  /** The timeline's own coverage, or null for a snapshot mode, which returns
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
 * `undefined` is the request still being in flight, or never issued, because a
 * disabled query has no data, which is distinct from every answer the daemon
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
    case 'absent':
      return {
        state: 'read',
        page: {
          mode: read.mode,
          scope: read.authorized_scope,
          entry: null,
          entries: 0,
          coverage: null,
        },
      };
    default: {
      const unhandled: never = read;
      return unhandled;
    }
  }
}

/** The graph version every channel below is derived from, or `null` when this
 * read has not produced one. */
export function graphEntryOf(reading: WorkGraphReading): WorkGraphVersionEntryV1 | null {
  return reading.state === 'read' ? reading.page.entry : null;
}

/** Runtime attempts from the same immutable graph version as the product
 * projections. Unavailable coverage remains visible through `runtimeReading`;
 * callers receive no synthetic attempts for it. */
export function graphRuntimeAttempts(
  reading: WorkGraphReading,
): readonly WorkRuntimeAttemptProjectionV1[] {
  const entry = graphEntryOf(reading);
  return entry === null || entry.runtime.coverage.coverage === 'unavailable'
    ? []
    : entry.runtime.attempts;
}

export function terminalWorkAttempt(state: WorkAttemptStateV1): boolean {
  switch (state) {
    case 'cancelled':
    case 'failed':
    case 'succeeded':
    case 'timed_out':
      return true;
    case 'cancellation_acknowledged':
    case 'cancellation_escalated':
    case 'cancellation_requested':
    case 'leased':
    case 'recovery_required':
    case 'running':
      return false;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/**
 * Why a channel the graph read would have supplied has no value.
 *
 * Distinct from a schema absence: the
 * contract carries these measurements, so reporting them as
 * `unsupported_schema` would tell a reader the build cannot do something it
 * can. Each case is the state the read actually returned, in that state's own
 * words, including the one that is not a failure at all.
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
      // and the authority held no version to serve, which is a fact about the
      // graph or the window rather than a failure of the read.
      return {
        available: false,
        state: 'complete_zero_findings',
        detail:
          reading.page.mode === 'absent'
            ? `no Work graph exists yet, so there is no version for ${measure} to be a property of; create a task to start one`
            : `the work-product graph read returned no graph version at all, so there is no version for ${measure} to be a property of, this is the authority reporting an empty window, not a read that failed`,
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

export function criticalPathReading(entry: WorkGraphVersionEntryV1): WorkCriticalPathReading {
  const path = entry.projections.critical_path;
  return { taskIds: path.task_ids, totalEffort: path.total_effort };
}
