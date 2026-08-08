import type {
  WorkGraphReadV1,
  WorkGraphVersionEntryV1,
  WorkItemV1,
} from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';

/**
 * The handoff frontier, derived from the work-product graph.
 *
 * Plan 11 asks the Agents surface for handoffs. The only handoff record any
 * route in this build serves is `WorkHandoffV1`, which hangs off every
 * `WorkItemV1` on the work-product graph and is reachable through
 * `operation.work.views` (`POST /api/work/views`). It is exactly the record the
 * plan means: one actor handing a task to another actor, at a recorded instant,
 * carrying the evidence it had reached and the questions it had not answered.
 *
 * The other thing the codebase calls a handoff is not this and must not be
 * confused with it. `crates/tracedecay-api/src/handoff.rs` mounts
 * `open_investigation_handoff` and `open_task_handoff`, which REDEEM an opaque
 * bearer token for a surface plus a receipt (`OpenTaskHandoffRequestV1` is
 * `{token, session_id}`, and its `Debug` redacts the token). They open one
 * handoff the caller already holds a token for; they cannot enumerate handoffs,
 * and none of their contracts is in this build's contract catalog at all. A
 * client written against them would be a client for a question this surface is
 * not asking.
 *
 * Everything below is a derivation over a read that either landed or did not.
 * There is no default and no zero: a refusal keeps its own state and sentence
 * all the way to the screen, so an unread frontier can never be drawn as an
 * empty one.
 */

/** One handoff, flattened onto the task it belongs to. */
export interface AgentHandoff {
  readonly handoffId: string;
  readonly taskId: string;
  readonly fromActor: string;
  readonly toActor: string;
  /** `UtcMicros`. Microseconds, because that is what the contract carries; a
   * millisecond reading here would place every handoff a thousandfold early. */
  readonly handedOffAtMicros: number;
  /** Evidence the handing-off actor had reached and is passing on. */
  readonly evidenceFrontier: readonly string[];
  /** Questions the handing-off actor did NOT answer. The load-bearing half of
   * the record: an unknown carried across a handoff is a stated gap, and a
   * surface that printed only the evidence would show a handoff as complete
   * work. */
  readonly unknowns: readonly string[];
}

/** One actor's part in the frontier: what it handed on, and what it was given. */
export interface AgentHandoffActor {
  readonly actor: string;
  readonly handedOff: number;
  readonly received: number;
}

export type AgentHandoffReading =
  /** Nothing has been asked yet, or the answer has not landed. */
  | { readonly state: 'pending' }
  /** The daemon refused, or answered something this build cannot read. */
  | { readonly state: 'refused'; readonly chip: DomainStateKind; readonly detail: string }
  /** The graph answered. `handoffs` may be empty, which is a measurement. */
  | {
      readonly state: 'read';
      readonly handoffs: readonly AgentHandoff[];
      readonly actors: readonly AgentHandoffActor[];
      /** Every task on the version that was read — the population the frontier
       * is a subset of, and the only honest denominator for it. */
      readonly tasksRead: number;
      /** Tasks carrying at least one handoff. */
      readonly tasksHandedOff: number;
      readonly unknownCount: number;
      readonly evidenceCount: number;
      readonly graphVersion: number;
      readonly observedAtMicros: number;
      /** True when the reading came from the last entry of a timeline rather
       * than from a single-version snapshot, so the caption can say which
       * version of the graph the frontier belongs to. */
      readonly fromTimeline: boolean;
    };

/**
 * The graph version a read carries.
 *
 * `current` and `as_of` answer with one entry. The two timeline modes answer
 * with a series, and this build reads the LAST of them — the newest version in
 * the window — rather than merging entries, because handoffs from two graph
 * versions summed together would be a frontier that never existed at any one
 * instant. The dashboard only ever asks `current`; the other three are handled
 * because the contract admits them, not because they are requested.
 */
export function latestGraphEntry(read: WorkGraphReadV1): {
  entry: WorkGraphVersionEntryV1;
  fromTimeline: boolean;
} | null {
  switch (read.mode) {
    case 'current':
    case 'as_of':
      return { entry: read.snapshot, fromTimeline: false };
    case 'evolution':
    case 'forensic': {
      const entry = read.timeline.entries[read.timeline.entries.length - 1];
      return entry ? { entry, fromTimeline: true } : null;
    }
    default: {
      const unhandled: never = read;
      return unhandled;
    }
  }
}

function handoffsOf(item: WorkItemV1): readonly AgentHandoff[] {
  return item.handoffs.map((handoff) => ({
    handoffId: handoff.handoff_id,
    taskId: handoff.task_id,
    fromActor: handoff.from_actor,
    toActor: handoff.to_actor,
    handedOffAtMicros: handoff.handed_off_at,
    evidenceFrontier: handoff.evidence_frontier,
    unknowns: handoff.unknowns,
  }));
}

/** Who handed what to whom, ranked by how much passed through each actor. */
export function handoffActors(
  handoffs: readonly AgentHandoff[],
): readonly AgentHandoffActor[] {
  const tally = new Map<string, { handedOff: number; received: number }>();
  const seat = (actor: string) => {
    const existing = tally.get(actor);
    if (existing) return existing;
    const fresh = { handedOff: 0, received: 0 };
    tally.set(actor, fresh);
    return fresh;
  };
  for (const handoff of handoffs) {
    seat(handoff.fromActor).handedOff += 1;
    seat(handoff.toActor).received += 1;
  }
  return [...tally.entries()]
    .map(([actor, counts]) => ({ actor, ...counts }))
    .sort(
      (a, b) =>
        b.handedOff + b.received - (a.handedOff + a.received) || a.actor.localeCompare(b.actor),
    );
}

/**
 * The frontier, from the Work views read.
 *
 * `undefined` is a read that has not landed — react-query has no data for a
 * query that is still in flight — and is reported as pending rather than as an
 * empty frontier, which is the whole point of keeping the two apart.
 */
export function readHandoffFrontier(
  result: WorkResult<WorkGraphReadV1> | undefined,
): AgentHandoffReading {
  if (result === undefined) return { state: 'pending' };
  if (result.outcome === 'refused') {
    return { state: 'refused', chip: result.state, detail: result.detail };
  }
  const latest = latestGraphEntry(result.value);
  if (latest === null) {
    return {
      state: 'refused',
      chip: 'unavailable',
      detail: 'the graph read answered with a timeline holding no version to read a frontier from',
    };
  }
  const items = latest.entry.graph.items;
  const handoffs = items
    .flatMap(handoffsOf)
    // Newest first: a frontier is read from its leading edge backwards.
    .sort((a, b) => b.handedOffAtMicros - a.handedOffAtMicros);
  return {
    state: 'read',
    handoffs,
    actors: handoffActors(handoffs),
    tasksRead: items.length,
    tasksHandedOff: items.filter((item) => item.handoffs.length > 0).length,
    unknownCount: handoffs.reduce((sum, handoff) => sum + handoff.unknowns.length, 0),
    evidenceCount: handoffs.reduce((sum, handoff) => sum + handoff.evidenceFrontier.length, 0),
    graphVersion: latest.entry.graph.version,
    observedAtMicros: latest.entry.observed_at,
    fromTimeline: latest.fromTimeline,
  };
}
