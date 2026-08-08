import { describe, expect, it } from 'vitest';

import {
  WorkAttemptListV1Schema,
  WorkGraphReadV1Schema,
  type WorkAttemptListV1,
} from '../../contracts/index.ts';
import { workAttempt as attempt, workAttemptList } from '../../test/workAttemptFixture.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../test/workGraphFixture.ts';
import type { WorkResult } from './workApi.ts';
import type { WorkChannel } from './workChannel.ts';
import { workGraphReading, type WorkGraphReading } from './workGraphModel.ts';
import {
  WORK_ACCOUNTING_DIMENSIONS,
  WORK_ACCOUNTING_FACETS,
  type WorkAccountingCard,
  type WorkAccountingDimension,
} from './workAccountingModel.ts';
import { workTopologyAccounting } from './workTopologyAccounting.ts';

/**
 * Plan 26's execution-topology accounting, over pages the generated contracts
 * accept.
 *
 * Every fixture is parsed with the generated schema before the model sees it,
 * for the same reason the other Work model tests do it: a hand-shaped object
 * the daemon could never send would let this file keep passing through a
 * contract change.
 *
 * The assertions are organised around the plan's two testable invariants
 * rather than around the code's structure — every card exposes all seven
 * facets, and nothing unsupported or under-floor is ever rendered as a zero.
 * A card that lost a facet or grew a fabricated figure fails here first.
 */

function listed(attempts: readonly unknown[]): WorkResult<WorkAttemptListV1> {
  return {
    outcome: 'value',
    value: WorkAttemptListV1Schema.parse(workAttemptList(attempts)),
  };
}

const BASE_GRAPH: WorkGraphVersionSpec = {
  tasks: [{ taskId: 'alpha', effort: 3 }, { taskId: 'beta', effort: 2 }],
  runtimeAttempts: [{ attemptId: 'a-1', taskId: 'alpha', runId: 'run-1' }],
};

function graphOf(spec: WorkGraphVersionSpec = BASE_GRAPH): WorkGraphReading {
  return workGraphReading({
    outcome: 'value',
    value: WorkGraphReadV1Schema.parse(workGraphRead(spec)),
  });
}

function cardOf(
  reading: { cards: readonly WorkAccountingCard[] },
  dimension: WorkAccountingDimension,
): WorkAccountingCard {
  const card = reading.cards.find((entry) => entry.dimension === dimension);
  if (card === undefined) throw new Error(`no card for ${dimension}`);
  return card;
}

/** The figure a channel proved, or a thrown failure. Deliberately not a
 * fallback: a test that defaulted to 0 would be the exact bug under test. */
function figure(channel: WorkChannel<{ value: number; unit: string }>): {
  value: number;
  unit: string;
} {
  if (!channel.available) throw new Error(`expected a proved figure, got ${channel.state}`);
  return channel.value;
}

function absence(channel: WorkChannel<unknown>): { state: string; detail: string } {
  if (channel.available) throw new Error('expected an absence');
  return { state: channel.state, detail: channel.detail };
}

describe('the shape of the ledger', () => {
  it('carries Plan 26’s twelve dimensions in the order the plan enumerates them', () => {
    expect(WORK_ACCOUNTING_DIMENSIONS).toEqual([
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
    ]);

    const reading = workTopologyAccounting(listed([]), graphOf());
    expect(reading.cards.map((card) => card.dimension)).toEqual(WORK_ACCOUNTING_DIMENSIONS);
  });

  /**
   * The plan's card contract, asserted as a contract rather than card by card.
   * Every dimension, in every read state, exposes all seven facets — a facet
   * that cannot be established is present and absent, never missing.
   */
  it('exposes all seven mandated facets on every card, in every read state', () => {
    const states: readonly [string, ReturnType<typeof workTopologyAccounting>][] = [
      ['unread', workTopologyAccounting(undefined, { state: 'pending' })],
      [
        'refused',
        workTopologyAccounting(
          { outcome: 'refused', state: 'unavailable', detail: 'the Work runtime is unavailable' },
          { state: 'refused', chip: 'unavailable', detail: 'the Work runtime is unavailable' },
        ),
      ],
      ['empty page', workTopologyAccounting(listed([]), graphOf())],
      [
        'read',
        workTopologyAccounting(
          listed([attempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' })]),
          graphOf(),
        ),
      ],
    ];

    for (const [label, reading] of states) {
      expect(reading.cards, label).toHaveLength(12);
      for (const card of reading.cards) {
        for (const facet of WORK_ACCOUNTING_FACETS) {
          const channel = card.provenance[facet];
          expect(channel, `${label} · ${card.dimension} · ${facet}`).toBeDefined();
          // A facet is a channel: proved with a value, or absent with a state
          // and a sentence. There is no third shape and no empty default.
          if (channel.available) expect(channel.value).toBeDefined();
          else {
            expect(channel.state.length).toBeGreaterThan(0);
            expect(channel.detail.length).toBeGreaterThan(0);
          }
        }
      }
    }
  });

  /**
   * The no-falsified-UI invariant, asserted over the whole ledger at once.
   *
   * Nine of the twelve dimensions have no published read model. Every row of
   * every one of them must be an absence that names the event kind that would
   * feed it — not a zero, and not a silently omitted row.
   */
  it('renders every unsupported dimension as a stated absence rather than a zero', () => {
    const reading = workTopologyAccounting(
      listed([attempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' })]),
      graphOf(),
    );

    const unsupported: readonly WorkAccountingDimension[] = [
      'duplicate_work',
      'conflict_confusion',
      'ready_to_integrated_latency',
      'integration_outcomes',
      'stale_stack_age',
      'github_stack_capability',
      'operational_leaks',
      'delivery_fanout',
    ];

    for (const dimension of unsupported) {
      const card = cardOf(reading, dimension);
      const stated = absence(card.reading);
      expect(stated.state, dimension).toBe('unsupported_schema');
      expect(stated.detail, dimension).toContain('ExecutionTopologyMetricsV1');
      // The event kind a reviewer greps for, on the card itself.
      expect(stated.detail, dimension).toMatch(/work\.[a-z_]+\.[a-z_.]*v1/);
      for (const row of card.rows) {
        expect(row.channel.available, `${dimension} · ${row.key}`).toBe(false);
      }
    }

    expect(reading.measured).toBe(2);
  });
});

describe('the concurrency ladder', () => {
  it('reads the two widths the workload projection carries and states the other three', () => {
    const card = cardOf(
      workTopologyAccounting(listed([]), graphOf({ ...BASE_GRAPH, requestedConcurrency: 4, actualConcurrency: 2 })),
      'concurrency_and_fanout',
    );

    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    expect(figure(rows.get('requested')!)).toEqual({ value: 4, unit: 'width' });
    expect(figure(rows.get('active')!)).toEqual({ value: 2, unit: 'width' });

    // The three rungs no field carries. Each names why, and none is a zero.
    for (const key of ['accepted', 'admitted', 'useful', 'fanout']) {
      const stated = absence(rows.get(key)!);
      expect(stated.state, key).toBe('unsupported_schema');
    }
    expect(absence(rows.get('admitted')!).detail).toContain('a count and not a width');
    expect(absence(rows.get('useful')!).detail).toContain('ProgressFrontier');

    expect(card.reading.available).toBe(true);
    if (!card.reading.available) throw new Error('unreachable');
    expect(card.reading.value).toContain('requested 4');
    expect(card.reading.value).toContain('active 2');
  });

  /** The authority withholds both widths unless runtime coverage was complete.
   * Withheld is `partial` with a sentence — a graph that could not be counted
   * is not a graph running nothing. */
  it('renders a withheld width as partial rather than as zero', () => {
    const card = cardOf(
      workTopologyAccounting(
        listed([]),
        graphOf({ ...BASE_GRAPH, requestedConcurrency: null, actualConcurrency: null }),
      ),
      'concurrency_and_fanout',
    );
    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    for (const key of ['requested', 'active']) {
      const stated = absence(rows.get(key)!);
      expect(stated.state, key).toBe('partial');
      expect(stated.detail, key).toContain('could not be counted');
    }
    // Both withheld figures are counted as unknowns in the card's censoring.
    const censoring = card.provenance.censoring;
    if (!censoring.available) throw new Error('expected censoring');
    expect(censoring.value.unknown).toBe(2);
  });

  /** Two authority figures disagreeing is reported, never clamped: clamping
   * would present a coherent pair the authority never produced. */
  it('reports active width above requested width as a contradiction and clamps neither', () => {
    const card = cardOf(
      workTopologyAccounting(
        listed([]),
        graphOf({ ...BASE_GRAPH, requestedConcurrency: 1, actualConcurrency: 5 }),
      ),
      'concurrency_and_fanout',
    );
    expect(card.contradictions.map((entry) => entry.key)).toEqual(['over_admission']);
    expect(card.contradictions[0]?.state).toBe('conflicting');

    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    expect(figure(rows.get('requested')!).value).toBe(1);
    expect(figure(rows.get('active')!).value).toBe(5);
  });

  it('carries the runtime coverage as its interval coverage and the graph version as its pin', () => {
    const card = cardOf(workTopologyAccounting(listed([]), graphOf()), 'concurrency_and_fanout');
    const coverage = card.provenance.intervalCoverage;
    if (!coverage.available) throw new Error('expected interval coverage');
    expect(coverage.value).toContain('complete');

    const revision = card.provenance.descriptorRevision;
    if (!revision.available) throw new Error('expected a revision');
    // Typed as the weaker thing it is: a source pin, never a metric descriptor.
    expect(revision.value.kind).toBe('source_read_pin');
    expect(revision.value.value).toContain('graph version 4');

    expect(figure(card.provenance.eligible)).toEqual({
      value: 2,
      unit: 'tasks',
      note: expect.stringContaining('graph version'),
    });
  });

  it('states the graph read’s own reason when it has not answered', () => {
    const card = cardOf(
      workTopologyAccounting(listed([]), {
        state: 'refused',
        chip: 'denied',
        detail: 'no authorized work-product scope',
      }),
      'concurrency_and_fanout',
    );
    const stated = absence(card.reading);
    expect(stated.state).toBe('denied');
    expect(stated.detail).toContain('no authorized work-product scope');
    for (const facet of WORK_ACCOUNTING_FACETS) {
      expect(card.provenance[facet].available, facet).toBe(false);
    }
  });
});

describe('the rerun census', () => {
  const PAGE = listed([
    attempt({
      taskId: 'alpha',
      runId: 'run-1',
      attemptId: 'a-1',
      recovery: { reason: 'lease_lost', source_attempt_id: 'a-0', state: 'restarted' },
    }),
    attempt({
      taskId: 'alpha',
      runId: 'run-1',
      attemptId: 'a-2',
      recovery: { checkpoint: null, source_attempt_id: 'a-1', state: 'resumed' },
    }),
    attempt({
      taskId: 'beta',
      runId: 'run-2',
      attemptId: 'b-1',
      state: 'recovery_required',
      recovery: {
        reason: 'provider_unavailable',
        source_attempt_id: null,
        state: 'recovery_required',
      },
      terminal: null,
    }),
    attempt({ taskId: 'gamma', runId: 'run-3', attemptId: 'c-1' }),
  ]);

  it('buckets the runtime family by its recorded restart cause', () => {
    const card = cardOf(workTopologyAccounting(PAGE, graphOf()), 'reruns');
    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));

    expect(figure(rows.get('runtime_restarted')!).value).toBe(1);
    expect(figure(rows.get('runtime_resumed')!).value).toBe(1);
    expect(figure(rows.get('runtime_recovery_required')!).value).toBe(1);
    expect(figure(rows.get('reason_lease_lost')!).value).toBe(1);
    expect(figure(rows.get('reason_provider_unavailable')!).value).toBe(1);
    // A measured zero: this page holds no attempt restarted for this cause.
    expect(figure(rows.get('reason_process_lost')!).value).toBe(0);

    if (!card.reading.available) throw new Error('expected a reading');
    expect(card.reading.value).toContain('3 runtime reruns');
  });

  /** Three populations, never summed. Test and CI reruns are absent and the
   * headline says so rather than letting a reader read the runtime figure as
   * the total. */
  it('keeps the test and CI rerun families absent and out of the runtime figure', () => {
    const card = cardOf(workTopologyAccounting(PAGE, graphOf()), 'reruns');
    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    for (const key of ['test_reruns', 'ci_reruns']) {
      const stated = absence(rows.get(key)!);
      expect(stated.state, key).toBe('unsupported_schema');
      expect(stated.detail, key).toContain('never summed');
    }
    if (!card.reading.available) throw new Error('expected a reading');
    expect(card.reading.value).toContain('never folded in');
  });

  /** Right-censoring: an attempt still running may owe a rerun nobody can see
   * yet, which is a different fact from a rerun that did not happen. */
  it('counts unterminated attempts as censored rather than as reruns of zero', () => {
    const card = cardOf(workTopologyAccounting(PAGE, graphOf()), 'reruns');
    const censoring = card.provenance.censoring;
    if (!censoring.available) throw new Error('expected censoring');
    expect(censoring.value.censored).toBe(1);
    expect(censoring.value.note).toContain('right-censoring');
  });

  it('reports the two typed fields disagreeing rather than picking a side', () => {
    const disagreement = listed([
      // Typed `recovery_required` as a state, `fresh` as a recovery record.
      attempt({
        taskId: 'alpha',
        runId: 'run-1',
        attemptId: 'a-1',
        state: 'recovery_required',
        terminal: null,
      }),
      // Terminal evidence while still typed as in flight.
      attempt({ taskId: 'beta', runId: 'run-2', attemptId: 'b-1', state: 'running' }),
    ]);
    const card = cardOf(workTopologyAccounting(disagreement, graphOf()), 'reruns');
    expect(card.contradictions.map((entry) => entry.key)).toEqual([
      'recovery_disagreement',
      'terminal_while_running',
    ]);
    for (const contradiction of card.contradictions) {
      expect(contradiction.state).toBe('conflicting');
    }
    // The census still reports what it read; the contradiction sits beside it.
    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    expect(figure(rows.get('runtime_recovery_required')!).value).toBe(0);
  });

  it('carries the daemon’s refusal rather than a census of zero', () => {
    const card = cardOf(
      workTopologyAccounting(
        { outcome: 'refused', state: 'unavailable', detail: 'the Work runtime is unavailable' },
        graphOf(),
      ),
      'reruns',
    );
    const stated = absence(card.reading);
    expect(stated.state).toBe('unavailable');
    expect(stated.detail).toContain('the Work runtime is unavailable');
    for (const row of card.rows.slice(0, 3)) {
      expect(row.channel.available, row.key).toBe(false);
    }
  });

  it('reports the capped page’s eligible denominator as the authorized set', () => {
    const capped: WorkResult<WorkAttemptListV1> = {
      outcome: 'value',
      value: WorkAttemptListV1Schema.parse(
        workAttemptList([attempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' })], {
          coverage: 'capped',
          returned: 1,
          remaining: 41,
          resume: {
            generation: 'generation-7',
            start_after: { attempt_id: 'a-1', run_id: 'run-1', task_id: 'alpha' },
          },
        }),
      ),
    };
    const card = cardOf(workTopologyAccounting(capped, graphOf()), 'reruns');
    expect(figure(card.provenance.support).value).toBe(1);
    expect(figure(card.provenance.eligible).value).toBe(42);
    const coverage = card.provenance.intervalCoverage;
    if (!coverage.available) throw new Error('expected coverage');
    expect(coverage.value).toContain('floor');
  });
});

describe('duplicate effects', () => {
  /**
   * The asymmetry this card exists to demonstrate: the reading is unavailable
   * and the eligible denominator is real. A card that withheld the denominator
   * along with the reading would understate how much of the measurement is
   * already in reach.
   */
  it('states an unavailable reading beside a real eligible denominator', () => {
    const card = cardOf(
      workTopologyAccounting(
        listed([
          attempt({
            taskId: 'alpha',
            runId: 'run-1',
            attemptId: 'a-1',
            effectState: 'compound_non_repeatable',
          }),
          attempt({
            taskId: 'beta',
            runId: 'run-2',
            attemptId: 'b-1',
            effectState: 'compound_non_repeatable',
          }),
          attempt({ taskId: 'gamma', runId: 'run-3', attemptId: 'c-1', effectState: 'intercepted' }),
          attempt({ taskId: 'delta', runId: 'run-4', attemptId: 'd-1' }),
        ]),
        graphOf(),
      ),
      'duplicate_effects',
    );

    expect(absence(card.reading).state).toBe('unsupported_schema');
    expect(figure(card.provenance.eligible)).toEqual({
      value: 2,
      unit: 'attempts',
      note: expect.stringContaining('compound non-repeatable'),
    });

    // Support is zero OBSERVATIONS, said as such, and never as a duplicate
    // count of zero.
    const support = figure(card.provenance.support);
    expect(support).toEqual({
      value: 0,
      unit: 'cases',
      note: expect.stringContaining('not a duplicate-effect count of zero'),
    });

    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));
    expect(figure(rows.get('effect_compound_non_repeatable')!).value).toBe(2);
    expect(figure(rows.get('effect_intercepted')!).value).toBe(1);
    expect(figure(rows.get('effect_observational')!).value).toBe(1);
    expect(absence(rows.get('adjudicated_duplicates')!).state).toBe('unsupported_schema');
  });
});

describe('blocked time', () => {
  it('keeps blocked effort as a distinct measure and leaves blocked time absent', () => {
    const card = cardOf(
      workTopologyAccounting(listed([]), graphOf({ ...BASE_GRAPH, blockedEffort: 7 })),
      'blocked_time',
    );
    const rows = new Map(card.rows.map((row) => [row.key, row.channel]));

    expect(absence(rows.get('unioned_blocked_time')!).state).toBe('unsupported_schema');
    expect(absence(rows.get('attributed_blocked_time')!).state).toBe('unsupported_schema');

    const effort = figure(rows.get('blocked_effort')!);
    expect(effort.value).toBe(7);
    // The unit is the whole point: effort is not time and is not offered as it.
    expect(effort.unit).toBe('effort');
    expect(absence(card.reading).detail).toContain('effort is not time');
  });
});

describe('the conflict confusion matrices', () => {
  it('keeps mechanical and semantic separate and every cell separate', () => {
    const card = cardOf(workTopologyAccounting(listed([]), graphOf()), 'conflict_confusion');
    expect(card.matrices).not.toBeNull();
    const matrices = card.matrices ?? [];
    expect(matrices.map((matrix) => matrix.kind)).toEqual(['mechanical', 'semantic']);

    for (const matrix of matrices) {
      // Four predicted classes against three observed classes, each carried
      // individually. Nothing is summed and no scalar is derived.
      expect(matrix.cells).toHaveLength(12);
      const seen = new Set(matrix.cells.map((cell) => `${cell.predicted}:${cell.observed}`));
      expect(seen.size).toBe(12);
      for (const cell of matrix.cells) {
        expect(cell.channel.available).toBe(false);
      }
    }

    // No accuracy scalar anywhere on the card, in any slot.
    expect(card.rows).toHaveLength(0);
    expect(absence(card.reading).detail).toContain('no accuracy, precision, or recall scalar');
  });

  it('is the only card that carries matrices', () => {
    const reading = workTopologyAccounting(listed([]), graphOf());
    const withMatrices = reading.cards.filter((card) => card.matrices !== null);
    expect(withMatrices.map((card) => card.dimension)).toEqual(['conflict_confusion']);
  });
});

describe('the near misses', () => {
  /** `WorkFallbackTopology` is the provider-executable fallback and looks like
   * the thing this card wants. Naming it in the absence is what stops the next
   * reader from wiring the wrong contract into the right-sounding slot. */
  it('names the provider fallback as NOT the GitHub generic fallback', () => {
    const card = cardOf(
      workTopologyAccounting(listed([]), graphOf()),
      'github_stack_capability',
    );
    expect(absence(card.reading).detail).toContain('WorkFallbackTopology');
    expect(absence(card.reading).detail).toContain('never counted into this card');
  });

  it('refuses to read an observed integration outcome off the pinned policy', () => {
    const card = cardOf(workTopologyAccounting(listed([]), graphOf()), 'integration_outcomes');
    expect(absence(card.reading).detail).toContain('integration STRATEGY');
    for (const key of ['fast_forward', 'merge', 'cherry_pick']) {
      const row = card.rows.find((entry) => entry.key === key);
      expect(row?.channel.available, key).toBe(false);
    }
  });

  it('does not borrow the retry weave for adjudicated duplicate work', () => {
    const card = cardOf(workTopologyAccounting(listed([]), graphOf()), 'duplicate_work');
    expect(absence(card.reading).detail).toContain('retry chain and not duplicate work');
  });
});
