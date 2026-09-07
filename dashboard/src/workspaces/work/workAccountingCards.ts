import type { WorkChannel } from './workChannel.ts';
import type { WorkAttemptReading } from './workAttemptModel.ts';
import { graphEntryOf, graphPageOf, type WorkGraphReading } from './workGraphModel.ts';
import { attemptChannelGap } from './workViewsModel.ts';
import {
  ANCHOR_CAP,
  EFFECT_STATES,
  RESTART_REASONS,
  attemptProvenance,
  coverageSentence,
  type AttemptCensus,
} from './workAccountingCensus.ts';
import {
  OBSERVED,
  PREDICTED,
  absentProvenance,
  accountingDimensionTitle,
  metricsGap,
  type WorkAccountingCard,
  type WorkAccountingContradiction,
  type WorkAccountingDimension,
  type WorkAccountingFigure,
  type WorkAccountingMatrix,
  type WorkAccountingRow,
  type WorkAttemptPageV1,
} from './workAccountingModel.ts';

/**
 * The five cards this build can actually source, and the one matrix card.
 *
 * Three dimensions have a mounted read behind them (concurrency and blocked
 * effort off the work-product graph; reruns and duplicate effects off the
 * attempt page), and the conflict matrices are built here too because their
 * shape — every predicted/observed cell kept separate — is the point of the
 * card rather than a rendering detail. The other seven dimensions never reach
 * this module; the assembler builds them straight from `unavailableCard`.
 *
 * Every card built here obeys the same two rules the plan imposes. A figure is
 * a count off a decoded contract field, never a rate, ratio or score, because
 * Plan 26 prohibits dashboard-side formulas outright. And an adjacent measure
 * never stands in for a mandated one: the blocked-time card prints blocked
 * EFFORT under its own label and leaves blocked TIME absent, rather than
 * letting a reader take the readable number for the mandated one.
 */

// --- Cards -------------------------------------------------------------------

const CONCURRENCY_MANDATE =
  'requested/accepted/admitted/active/useful concurrency and fanout';

export function concurrencyCard(graph: WorkGraphReading): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'concurrency_and_fanout';
  const entry = graphEntryOf(graph);
  const page = graphPageOf(graph);
  const workload = entry?.projections.workload ?? null;
  const runtime = entry?.runtime ?? null;
  const runtimeCoverage = runtime?.coverage;

  // Both figures are gated on complete runtime coverage by the authority, so
  // they are withheld together and are absent together. Withheld is `partial`,
  // never zero: a graph that could not be counted is not a graph running
  // nothing.
  const withheld: WorkChannel<never> = {
    available: false,
    state: 'partial',
    detail:
      'the authority withholds this width unless the runtime projection covered every attempt, so it is a width that could not be counted rather than a width of zero',
  };
  const unread = graphAbsence(graph, 'this width');

  const runtimeGap: WorkChannel<never> | null =
    runtimeCoverage === undefined || runtimeCoverage.coverage === 'complete'
      ? null
      : runtimeCoverage.coverage === 'partial'
        ? {
            available: false,
            state: 'partial',
            detail:
              'the runtime projection omitted some attempts, so this graph cannot establish a complete execution population',
          }
        : {
            available: false,
            state: 'unavailable',
            detail:
              'the runtime projection is unavailable, so every runtime observation on this graph is unread rather than observed as zero',
          };

  function widthOf(value: number | null | undefined): WorkChannel<WorkAccountingFigure> {
    if (workload === null) return unread;
    if (runtimeGap !== null) return runtimeGap;
    if (value === null || value === undefined) return withheld;
    return { available: true, value: { value, unit: 'width' } };
  }

  const requested = widthOf(workload?.requested_concurrency);
  const active = widthOf(workload?.actual_concurrency);

  const rows: WorkAccountingRow[] = [
    { key: 'requested', label: 'Requested width', channel: requested },
    {
      key: 'accepted',
      label: 'Accepted width',
      channel: metricsGap(
        dimension,
        'the Plan 24-accepted width',
        'The workload projection carries requested and actual width and nothing between them; accepted width is a decision the graph read does not record.',
      ),
    },
    {
      key: 'admitted',
      label: 'Admitted width',
      channel: metricsGap(
        dimension,
        'the Plan 32-admitted width',
        'The attempt page counts attempts admitted, which is a count and not a width — a width is a quantity over an interval and no read here carries an interval.',
      ),
    },
    { key: 'active', label: 'Active width', channel: active },
    {
      key: 'useful',
      label: 'Useful width',
      channel: metricsGap(
        dimension,
        'useful width',
        'Useful width requires distinct admitted attempts that each advanced a committed ProgressFrontier and are not linked by an adjudicated duplicate-work relation; this build reads neither the frontier advance nor the adjudication.',
      ),
    },
    {
      key: 'fanout',
      label: 'Fan-in / fan-out buckets',
      channel: metricsGap(dimension, 'the fan-in and fan-out buckets'),
    },
  ];

  // Active width above requested width is two authority figures disagreeing.
  // Reported, never clamped: clamping would present a coherent pair the
  // authority never produced.
  const contradictions: WorkAccountingContradiction[] = [];
  if (requested.available && active.available && active.value.value > requested.value.value) {
    contradictions.push({
      key: 'over_admission',
      state: 'conflicting',
      detail: `active width ${active.value.value} exceeds requested width ${requested.value.value} under one graph version — the two figures disagree and neither is clamped to the other`,
    });
  }

  const reading: WorkChannel<string> =
    runtimeGap !== null
      ? runtimeGap
      : requested.available && active.available
      ? {
          available: true,
          value: `requested ${requested.value.value} · active ${active.value.value} — three of the mandate's five rungs are unavailable`,
        }
      : requested.available || active.available
        ? {
            available: false,
            state: 'partial',
            detail:
              'only one of the two readable widths was answered, so the pair cannot be stated as a reading; the rungs below carry whichever one arrived',
          }
        : unread;

  const coverage = runtime?.coverage;
  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: CONCURRENCY_MANDATE,
    reading,
    rows,
    matrices: null,
    contradictions,
    provenance: {
      support:
        runtime === null
          ? graphAbsence(graph, 'the supporting observation count')
          : runtimeCoverage?.coverage === 'unavailable'
            ? runtimeGap!
          : {
              available: true,
              value: {
                value: runtime.attempts.length,
                unit: 'attempts',
                note:
                  runtimeCoverage?.coverage === 'partial'
                    ? 'attempts the partial runtime projection carried — a floor, not the complete population'
                    : 'attempts the runtime projection carried under this graph version',
              },
            },
      eligible:
        entry === null
          ? graphAbsence(graph, 'the eligible denominator')
          : {
              available: true,
              value: {
                value: entry.projections.dag.task_ids.length,
                unit: 'tasks',
                note: 'tasks in the graph version the widths were counted under',
              },
            },
      censoring:
        coverage === undefined
          ? graphAbsence(graph, 'the censored and unknown counts')
          : coverage.coverage === 'unavailable'
            ? runtimeGap!
          : {
              available: true,
              value: {
                censored: coverage.coverage === 'partial' ? coverage.unavailable_attempts.length : 0,
                unknown:
                  workload === null
                    ? 0
                    : [workload.requested_concurrency, workload.actual_concurrency].filter(
                        (figure) => figure === null,
                      ).length,
                note:
                  'censored attempts are ones the runtime projection knows of and could not read; unknowns are the width figures the authority withheld',
              },
            },
      intervalCoverage:
        coverage === undefined
          ? graphAbsence(graph, 'interval coverage')
          : coverage.coverage === 'unavailable'
            ? runtimeGap!
          : {
              available: true,
              value:
                coverage.coverage === 'complete'
                  ? 'runtime projection complete over every attempt of this graph version'
                  : coverage.coverage === 'partial'
                    ? `runtime projection partial: ${coverage.unavailable_attempts.length} attempts unread, so every width here is a floor`
                    : 'runtime projection unavailable: the attempts under this graph version are unmeasured, which is not a reading of zero',
            },
      horizon:
        page === null
          ? graphAbsence(graph, 'the observation horizon')
          : {
              available: true,
              value: `${page.mode} graph read · ${page.entries} version${page.entries === 1 ? '' : 's'} · a version and not a time window`,
            },
      descriptorRevision:
        entry === null
          ? graphAbsence(graph, 'the descriptor revision')
          : {
              available: true,
              value: {
                kind: 'source_read_pin',
                value: `graph version ${entry.verified_version.graph_version} · runtime generation ${entry.runtime.generation_id}`,
              },
            },
      anchors:
        runtime === null
          ? graphAbsence(graph, 'safe drill anchors')
          : runtimeCoverage?.coverage === 'unavailable'
            ? runtimeGap!
          : {
              available: true,
              value: runtime.attempts.slice(0, ANCHOR_CAP).map((attempt) => ({
                kind: 'attempt' as const,
                id: attempt.identity.attempt_id,
                taskId: attempt.identity.task_id,
              })),
            },
    },
  };
}

/** The graph read's own reason, phrased for a provenance facet. Kept local so
 * this module never invents a state the read did not report. */
function graphAbsence(graph: WorkGraphReading, measure: string): WorkChannel<never> {
  switch (graph.state) {
    case 'pending':
      return {
        available: false,
        state: 'loading',
        detail: `the work-product graph read has not answered yet, so ${measure} is not drawn`,
      };
    case 'refused':
      return {
        available: false,
        state: graph.chip,
        detail: `${measure} is read from the work-product graph, and that read was refused: ${graph.detail}`,
      };
    case 'read':
      return {
        available: false,
        state: 'complete_zero_findings',
        detail: `the work-product graph read returned no graph version, so there is no version for ${measure} to be a property of`,
      };
    default: {
      const unhandled: never = graph;
      return unhandled;
    }
  }
}

const RERUN_MANDATE = 'runtime/test/CI reruns';

export function rerunCard(
  reading: WorkAttemptReading,
  page: WorkAttemptPageV1 | null,
  census: AttemptCensus | null,
): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'reruns';
  const gap = (measure: string) => attemptChannelGap(reading, measure);

  function count(value: number | undefined, note?: string): WorkChannel<WorkAccountingFigure> {
    if (census === null || value === undefined) return gap('a rerun');
    const pageFloor = page?.coverage.coverage === 'capped';
    const figure: WorkAccountingFigure =
      note === undefined && !pageFloor
        ? { value, unit: 'attempts' }
        : {
            value,
            unit: 'attempts',
            note:
              pageFloor
                ? `${note === undefined ? 'count on the capped attempt page' : note} — a floor, not a total`
                : note,
          };
    return { available: true, value: figure };
  }

  const rows: WorkAccountingRow[] = [
    {
      key: 'runtime_restarted',
      label: 'Runtime · restarted',
      channel: count(census?.recovery.restarted),
    },
    {
      key: 'runtime_resumed',
      label: 'Runtime · resumed from checkpoint',
      channel: count(census?.recovery.resumed),
    },
    {
      key: 'runtime_recovery_required',
      label: 'Runtime · recovery required',
      channel: count(
        census?.recovery.recoveryRequired,
        'a rerun the record says is owed and not yet made',
      ),
    },
    ...RESTART_REASONS.map((reason) => ({
      key: `reason_${reason}`,
      label: `Runtime cause · ${reason.replace(/_/g, ' ')}`,
      channel: count(census?.restartReasons[reason]),
    })),
    {
      key: 'test_reruns',
      label: 'Test reruns',
      channel: metricsGap(
        dimension,
        'test reruns',
        'A different population from the runtime family above, and never summed with it.',
      ),
    },
    {
      key: 'ci_reruns',
      label: 'CI reruns',
      channel: metricsGap(
        dimension,
        'CI reruns',
        'A different population from the runtime family above, and never summed with it.',
      ),
    },
  ];

  const contradictions: WorkAccountingContradiction[] = [];
  if (census !== null && census.recoveryDisagreements > 0) {
    contradictions.push({
      key: 'recovery_disagreement',
      state: 'conflicting',
      detail: `${census.recoveryDisagreements} ${census.recoveryDisagreements === 1 ? 'attempt states' : 'attempts state'} recovery_required in exactly one of the two typed fields that record it — the attempt state and the recovery record disagree, and neither is taken as the authority over the other`,
    });
  }
  if (census !== null && census.terminalWhileRunning > 0) {
    contradictions.push({
      key: 'terminal_while_running',
      state: 'conflicting',
      detail: `${census.terminalWhileRunning} ${census.terminalWhileRunning === 1 ? 'attempt carries' : 'attempts carry'} terminal evidence while still typed as in flight, so the rerun census counts a record that contradicts itself rather than picking a side`,
    });
  }

  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: RERUN_MANDATE,
    reading:
      census === null
        ? gap('the rerun census')
        : metricsGap(
            dimension,
            'the completed runtime rerun total',
            'The page carries restarted, resumed, and recovery-required separately. Recovery-required is a rerun owed, not a completed rerun, and no canonical aggregate total is published.',
          ),
    rows,
    matrices: null,
    contradictions,
    provenance:
      page === null || census === null
        ? absentProvenance(dimension)
        : attemptProvenance(
            page,
            census,
            'attempts with no terminal evidence: a rerun they may still owe is not yet observable, which is right-censoring rather than a rerun of zero',
          ),
  };
}

const DUPLICATE_EFFECT_MANDATE = 'duplicate effects';

export function duplicateEffectCard(
  reading: WorkAttemptReading,
  pageRead: WorkAttemptPageV1 | null,
  census: AttemptCensus | null,
): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'duplicate_effects';
  const gap = (measure: string) => attemptChannelGap(reading, measure);

  const rows: WorkAccountingRow[] = EFFECT_STATES.map((state) => {
    let channel: WorkChannel<WorkAccountingFigure>;
    if (census === null) {
      channel = gap('an admitted effect class');
    } else {
      const figure: WorkAccountingFigure =
        state === 'compound_non_repeatable'
          ? {
              value: census.effects[state],
              unit: 'attempts',
              note:
                pageRead?.coverage.coverage === 'capped'
                  ? 'the only class in which an effect could be duplicated — count on a capped page, a floor not a total'
                  : 'the only class in which an effect could be duplicated',
            }
          : {
              value: census.effects[state],
              unit: 'attempts',
              note:
                pageRead?.coverage.coverage === 'capped'
                  ? 'count on a capped attempt page — a floor, not a total'
                  : undefined,
            };
      channel = { available: true, value: figure };
    }
    return {
      key: `effect_${state}`,
      label: `Admitted under ${state.replace(/_/g, ' ')}`,
      channel,
    };
  });

  rows.push({
    key: 'adjudicated_duplicates',
    label: 'Adjudicated duplicate effects',
    channel: metricsGap(
      dimension,
      'the adjudicated duplicate-effect count',
      'The effect CLASS above is read from each execution envelope; whether one effect was committed twice is an adjudication, and no read here makes it.',
    ),
  });

  // The interesting asymmetry, and the reason this card exists: the reading is
  // unavailable while the eligible denominator is real. A card that hid the
  // denominator because the reading was missing would understate exactly how
  // much of this measurement is already in reach.
  const eligible: WorkChannel<WorkAccountingFigure> =
    census === null
      ? metricsGap(dimension, 'the eligible denominator')
      : pageRead?.coverage.coverage === 'capped'
        ? {
            available: false,
            state: 'partial',
            detail:
              'the capped page establishes only a lower-bound effect census, not a full eligible denominator for duplicate-effect adjudication',
          }
      : {
          available: true,
          value: {
            value: census.effects.compound_non_repeatable,
            unit: 'attempts',
            note: 'attempts admitted under a compound non-repeatable effect — the eligible set a duplicate-effect adjudication would run over',
          },
        };

  const base = absentProvenance(dimension);
  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: DUPLICATE_EFFECT_MANDATE,
    reading: metricsGap(
      dimension,
      'the duplicate-effect count',
      'The eligible denominator beside it IS readable, and is stated rather than withheld along with the reading.',
    ),
    rows,
    matrices: null,
    contradictions: [],
    provenance:
      pageRead === null || census === null
        ? { ...base, eligible }
        : {
            ...base,
            support: metricsGap(
              dimension,
              'adjudication support',
              'No duplicate-effect adjudication read is mounted, so the observation support is unknown rather than a case count of zero.',
            ),
            eligible,
            intervalCoverage: { available: true, value: coverageSentence(pageRead) },
            horizon: {
              available: true,
              value: `one attempt page under topology generation ${pageRead.topology.generation} · the eligible set only`,
            },
            descriptorRevision: {
              available: true,
              value: {
                kind: 'source_read_pin',
                value: `topology generation ${pageRead.topology.generation}`,
              },
            },
            anchors: { available: true, value: census.anchors },
          },
  };
}

const BLOCKED_MANDATE = 'unioned and cause-attributed blocked time';

export function blockedTimeCard(graph: WorkGraphReading): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'blocked_time';
  const entry = graphEntryOf(graph);
  const blockedEffort = entry?.projections.workload.blocked_effort ?? null;

  const rows: WorkAccountingRow[] = [
    {
      key: 'unioned_blocked_time',
      label: 'Unioned blocked time',
      channel: metricsGap(dimension, 'unioned blocked time'),
    },
    {
      key: 'attributed_blocked_time',
      label: 'Cause-attributed blocked time',
      channel: metricsGap(dimension, 'cause-attributed blocked time'),
    },
    {
      key: 'blocked_effort',
      label: 'Declared blocked effort — a different measure',
      channel:
        entry === null
          ? graphAbsence(graph, 'declared blocked effort')
          : blockedEffort === null
            ? {
                available: false,
                state: 'partial',
                detail:
                  'the authority withheld the ready/running/blocked split because the runtime projection did not cover every attempt, so blocked effort could not be apportioned',
              }
            : {
                available: true,
                value: {
                  value: blockedEffort,
                  unit: 'effort',
                  note: 'declared effort sitting behind a gate — NOT time, and never substituted for the two rows above',
                },
              },
    },
  ];

  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: BLOCKED_MANDATE,
    reading: metricsGap(
      dimension,
      'blocked time',
      'Blocked EFFORT is readable and is printed below under its own label; effort is not time and does not stand in for it.',
    ),
    rows,
    matrices: null,
    contradictions: [],
    provenance: absentProvenance(dimension),
  };
}

export function conflictCard(): WorkAccountingCard {
  const dimension: WorkAccountingDimension = 'conflict_confusion';
  const matrices: WorkAccountingMatrix[] = (['mechanical', 'semantic'] as const).map((kind) => ({
    kind,
    cells: PREDICTED.flatMap((predicted) =>
      OBSERVED.map((observed) => ({
        predicted,
        observed,
        channel: metricsGap(
          dimension,
          `the ${kind} cell predicted ${predicted.replace(/_/g, ' ')} / observed ${observed.replace(/_/g, ' ')}`,
        ),
      })),
    ),
  }));

  return {
    dimension,
    title: accountingDimensionTitle(dimension),
    mandate: 'mechanical/semantic conflict confusion matrices',
    reading: metricsGap(
      dimension,
      'the conflict confusion matrices',
      'Every cell is carried separately below and stays separate: no accuracy, precision, or recall scalar is derived here, because the plan forbids collapsing the matrix and forbids the dashboard deriving a metric at all.',
    ),
    rows: [],
    matrices,
    contradictions: [],
    provenance: absentProvenance(dimension),
  };
}
