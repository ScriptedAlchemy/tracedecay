import type { WorkAttemptListV1 } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkChannel } from './workChannel.ts';

/**
 * The card contract Plan 26 attaches to every execution-topology dimension,
 * and the typed absence a dimension wears when this build cannot source it.
 *
 * This module is the vocabulary and nothing else: it names the twelve
 * dimensions, gives each one its title and the persisted event kind that would
 * feed it, declares the seven provenance facets every card must carry, and
 * builds the absence a card wears when the projection behind it is not
 * published. It reads no data and derives no figure — `workAccountingCensus.ts`
 * walks the attempt page, `workAccountingCards.ts` builds the sourced cards,
 * and `workTopologyAccounting.ts` assembles the twelve.
 *
 * `metricsGap` says `unsupported` rather than `unavailable` on purpose; the
 * reasoning is in the assembler's module doc, which explains which dimensions
 * are decoded from the mounted reads and why the rest stay stated absences.
 */

/** The one `WorkAttemptListV1` variant that carries a page. */
export type WorkAttemptPageV1 = Extract<WorkAttemptListV1, { state: 'listed' }>;

// --- The twelve dimensions ---------------------------------------------------

/** The dimensions Plan 26's `execution-topology` sentence enumerates, in the
 * order it enumerates them. Display order is the mandate's order so a reader
 * holding the plan can walk the page against it. */
export type WorkAccountingDimension =
  | 'concurrency_and_fanout'
  | 'duplicate_work'
  | 'conflict_confusion'
  | 'ready_to_integrated_latency'
  | 'integration_outcomes'
  | 'stale_stack_age'
  | 'github_stack_capability'
  | 'blocked_time'
  | 'reruns'
  | 'duplicate_effects'
  | 'operational_leaks'
  | 'delivery_fanout';

export const WORK_ACCOUNTING_DIMENSIONS: readonly WorkAccountingDimension[] = [
  'concurrency_and_fanout',
  'duplicate_work',
  'conflict_confusion',
  'ready_to_integrated_latency',
  'integration_outcomes',
  'stale_stack_age',
  'github_stack_capability',
  'blocked_time',
  'reruns',
  'duplicate_effects',
  'operational_leaks',
  'delivery_fanout',
];

export function accountingDimensionTitle(dimension: WorkAccountingDimension): string {
  switch (dimension) {
    case 'concurrency_and_fanout':
      return 'Concurrency and fanout';
    case 'duplicate_work':
      return 'Adjudicated duplicate work';
    case 'conflict_confusion':
      return 'Conflict confusion matrices';
    case 'ready_to_integrated_latency':
      return 'Ready-to-integrated latency';
    case 'integration_outcomes':
      return 'Observed integration outcomes';
    case 'stale_stack_age':
      return 'Stale-stack age';
    case 'github_stack_capability':
      return 'GitHub stack capability';
    case 'blocked_time':
      return 'Blocked time';
    case 'reruns':
      return 'Reruns';
    case 'duplicate_effects':
      return 'Duplicate effects';
    case 'operational_leaks':
      return 'Operational leaks';
    case 'delivery_fanout':
      return 'Delivery fanout';
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}

/**
 * The persisted event kind that feeds each dimension.
 *
 * Copied from Plan 26's execution-topology event family and from
 * `EXECUTION_TOPOLOGY_EVENT_KINDS_V1` in the Rust projector, which spell them
 * identically. Naming the kind rather than the route is deliberate: an
 * absence here is a descriptor this ledger does not decode, not a transport
 * that is down, and a reviewer who greps the kind lands on the projector
 * rather than on a router.
 */
export function accountingEventKind(dimension: WorkAccountingDimension): string {
  switch (dimension) {
    case 'concurrency_and_fanout':
      return 'work.execution_topology.sampled.v1';
    case 'duplicate_work':
    case 'duplicate_effects':
      return 'work.duplicate_effort.observed.v1';
    case 'conflict_confusion':
      return 'work.conflict_prediction.observed.v1 / work.conflict_outcome.linked.v1';
    case 'ready_to_integrated_latency':
    case 'integration_outcomes':
      return 'work.integration.transition.observed.v1';
    case 'stale_stack_age':
      return 'work.stack_drift.observed.v1';
    case 'github_stack_capability':
      return 'work.github_stack_capability.observed.v1';
    case 'blocked_time':
      return 'work.blocked_interval.observed.v1';
    case 'reruns':
      return 'work.rerun.observed.v1';
    case 'operational_leaks':
      return 'work.execution_leak.observed.v1';
    case 'delivery_fanout':
      return 'work.delivery_fanout.observed.v1';
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}

// --- Figures, rows, cells ----------------------------------------------------

/** A quantity and the unit it was measured in. There is no bare number in this
 * module: `3` as a concurrency width and `3` as an attempt count are different
 * facts, and a card that printed both without units would invite the reader to
 * compare them. */
export interface WorkAccountingFigure {
  readonly value: number;
  readonly unit: 'width' | 'attempts' | 'effort' | 'tasks' | 'cases';
  /** A sentence the row prints beside the figure when the figure needs one —
   * most often that it is a floor rather than a total. */
  readonly note?: string;
}

/** One line of a card: a named measurement, proved or explained. */
export interface WorkAccountingRow {
  readonly key: string;
  readonly label: string;
  readonly channel: WorkChannel<WorkAccountingFigure>;
}

/**
 * One cell of a conflict confusion matrix.
 *
 * Cells are carried individually and never reduced. Plan 26 keeps mechanical
 * and semantic prediction separate and keeps every cell of each matrix
 * separate; an accuracy scalar computed over them would be a dashboard formula
 * twice over — a metric the plan prohibits the UI from deriving, and a
 * collapse the plan prohibits outright.
 */
export interface WorkAccountingMatrixCell {
  readonly predicted: 'conflict' | 'no_conflict' | 'abstained' | 'unknown';
  readonly observed: 'conflict' | 'no_conflict' | 'unknown';
  readonly channel: WorkChannel<WorkAccountingFigure>;
}

export interface WorkAccountingMatrix {
  readonly kind: 'mechanical' | 'semantic';
  readonly cells: readonly WorkAccountingMatrixCell[];
}

export const PREDICTED: readonly WorkAccountingMatrixCell['predicted'][] = [
  'conflict',
  'no_conflict',
  'abstained',
  'unknown',
];

export const OBSERVED: readonly WorkAccountingMatrixCell['observed'][] = [
  'conflict',
  'no_conflict',
  'unknown',
];

// --- The seven provenance facets ---------------------------------------------

/** Right-censored and unknown observations, kept apart. An observation still
 * running is not an observation with an unknown outcome, and folding them
 * would hide the one a longer horizon would fix. */
export interface WorkAccountingCensoring {
  readonly censored: number;
  readonly unknown: number;
  readonly note: string;
}

/**
 * What a measurement is pinned to.
 *
 * `metric_descriptor` is what Plan 26 means by descriptor revision — the
 * `EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1` a stored value carries so it can
 * never be compared against a differently derived one. No card can carry that
 * today, because the descriptor is not published to this build.
 *
 * `source_read_pin` is the weaker thing the mounted reads DO carry: the graph
 * version or topology generation the reading was taken under. It is typed
 * separately rather than written into the same slot, so a reader is never
 * shown a version identity under the word "descriptor revision".
 */
export interface WorkAccountingRevision {
  readonly kind: 'metric_descriptor' | 'source_read_pin';
  readonly value: string;
}

/** A local, non-exportable anchor a reader can drill from an aggregate to.
 * Plan 26 allows exactly these: opaque local ids reached after authorization,
 * never a path, ref, commit, actor, or title. */
export interface WorkAccountingAnchor {
  readonly kind: 'task' | 'run' | 'attempt';
  readonly id: string;
  /** The task the anchor selects, when selecting one is meaningful. */
  readonly taskId: string | null;
}

/** The seven facets Plan 26 requires on every card. Each is a channel, so a
 * facet that cannot be established renders as its own stated absence rather
 * than as a blank or a zero. */
export interface WorkAccountingProvenance {
  readonly support: WorkChannel<WorkAccountingFigure>;
  readonly eligible: WorkChannel<WorkAccountingFigure>;
  readonly censoring: WorkChannel<WorkAccountingCensoring>;
  readonly intervalCoverage: WorkChannel<string>;
  readonly horizon: WorkChannel<string>;
  readonly descriptorRevision: WorkChannel<WorkAccountingRevision>;
  readonly anchors: WorkChannel<readonly WorkAccountingAnchor[]>;
}

export const WORK_ACCOUNTING_FACETS = [
  'support',
  'eligible',
  'censoring',
  'intervalCoverage',
  'horizon',
  'descriptorRevision',
  'anchors',
] as const satisfies readonly (keyof WorkAccountingProvenance)[];

/** A reading the card is obliged to print but that contradicts itself. Never
 * clamped and never dropped: two typed fields of one contract disagreeing is a
 * fact about the record, and rounding it away would make the record look
 * cleaner than it is. */
export interface WorkAccountingContradiction {
  readonly key: string;
  readonly state: DomainStateKind;
  readonly detail: string;
}

export interface WorkAccountingCard {
  readonly dimension: WorkAccountingDimension;
  readonly title: string;
  /** What Plan 26 asks this card for, in the plan's own words, so the absence
   * beneath it can be read against the requirement rather than guessed at. */
  readonly mandate: string;
  /** The card's headline reading, or the reason there is none. */
  readonly reading: WorkChannel<string>;
  readonly rows: readonly WorkAccountingRow[];
  /** Present only on the conflict card. Null elsewhere rather than empty, so a
   * view cannot render an empty grid as a matrix of zeroes. */
  readonly matrices: readonly WorkAccountingMatrix[] | null;
  readonly contradictions: readonly WorkAccountingContradiction[];
  readonly provenance: WorkAccountingProvenance;
}

export interface WorkTopologyAccountingReading {
  readonly cards: readonly WorkAccountingCard[];
  /** How many of the twelve rendered a headline reading. Printed rather than
   * inferred, because a page of twelve absences and a page of twelve readings
   * are the same shape. */
  readonly measured: number;
}

// --- Absences ----------------------------------------------------------------

/**
 * The absence a dimension wears when this ledger takes no measurement for it.
 *
 * `unsupported` rather than `unavailable`: `ExecutionTopologyMetricsV1` is
 * published and mounted at `operation.work.topology_metrics`, and this ledger
 * decodes its integration and stack families, but it does not decode a
 * descriptor for this measure. Saying `unavailable` would tell a reader a
 * reachable source refused, which is a different and
 * fixable-in-a-different-place thing.
 */
export function metricsGap(
  dimension: WorkAccountingDimension,
  measure: string,
  extra?: string,
): WorkChannel<never> {
  const kind = accountingEventKind(dimension);
  const tail = extra === undefined ? '' : ` ${extra}`;
  return {
    available: false,
    state: 'unsupported',
    detail: `${measure} belongs to the ${kind} event family Plan 26 projects through ExecutionTopologyMetricsV1; this ledger does not decode a descriptor for it, so it is a measurement not taken here rather than one measured as zero.${tail}`,
  };
}

/** Every facet unavailable, for a dimension nothing in this build can source.
 * Callers override individual facets where a real one exists. */
export function absentProvenance(dimension: WorkAccountingDimension): WorkAccountingProvenance {
  return {
    support: metricsGap(dimension, 'the supporting observation count'),
    eligible: metricsGap(dimension, 'the eligible denominator'),
    censoring: metricsGap(dimension, 'the censored and unknown counts'),
    intervalCoverage: metricsGap(dimension, 'interval coverage'),
    horizon: metricsGap(dimension, 'the observation horizon'),
    descriptorRevision: metricsGap(dimension, 'the descriptor revision'),
    anchors: metricsGap(dimension, 'safe drill anchors'),
  };
}

/** A whole dimension with nothing behind it: one stated absence, repeated into
 * every slot the card contract demands. */
export function unavailableCard(
  dimension: WorkAccountingDimension,
  mandate: string,
  rows: readonly { key: string; label: string; measure: string }[],
  extra?: string,
): WorkAccountingCard {
  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate,
    reading: metricsGap(dimension, accountingDimensionTitle(dimension).toLowerCase(), extra),
    rows: rows.map((row) => ({
      key: row.key,
      label: row.label,
      channel: metricsGap(dimension, row.measure),
    })),
    matrices: null,
    contradictions: [],
    provenance: absentProvenance(dimension),
  };
}
