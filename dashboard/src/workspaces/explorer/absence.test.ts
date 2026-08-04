import { describe, expect, it } from 'vitest';
import { absenceVerdict } from './absence.ts';
import { ExplorerQueryRunV1Schema } from '../../contracts/generated.ts';

/**
 * Coverage fields a source may report. Defaults describe a source that examined
 * its whole denominator and accounted for every unit — the only shape that may
 * earn a confirmed absence.
 */
function coverage(over: Record<string, unknown> = {}) {
  return {
    completeness: 'complete',
    eligible: 5,
    examined: 5,
    matched: 0,
    excluded: 0,
    omitted: 0,
    unknown: 0,
    denominator: 5,
    unit: 'symbols',
    omission_reasons: [],
    ...over,
  };
}

function source(id: 'code_graph' | 'sessions' | 'knowledge', over: Record<string, unknown> = {}) {
  return {
    source_id: id,
    source_label: id === 'code_graph' ? 'Code graph' : id === 'sessions' ? 'Sessions' : 'Knowledge',
    phase: 'completed',
    outcome: 'ready',
    completed_units: 0,
    total_units: 5,
    coverage: coverage(),
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
    ...over,
  };
}

/** Parsed through the real schema, so a fixture that the contract would reject
 * cannot quietly prove anything here. */
function run(sources: unknown[], finality: 'complete' | 'partial' = 'complete') {
  return ExplorerQueryRunV1Schema.parse({
    run_id: 'run-1',
    request: { query: 'missing', limit: 50, offset: 0 },
    request_revision: 'explorer-query-request-v1',
    plan_revision: 'explorer-query-plan-v1',
    merge_revision: 'source-local-no-merge-v1',
    required_source_ids: ['code_graph', 'sessions', 'knowledge'],
    ordering_policy: 'source_local_no_cross_source_merge',
    explanation: 'test',
    submitted_at_micros: 1,
    completed_at_micros: 2,
    elapsed_micros: 1,
    state: finality === 'complete' ? 'completed' : 'partial',
    finality,
    sources,
  });
}

describe('absenceVerdict', () => {
  it('confirms absence when every source examined its full denominator', () => {
    const verdict = absenceVerdict(
      run([source('code_graph'), source('sessions'), source('knowledge')]),
    );
    expect(verdict.confirmed).toBe(true);
    expect(verdict.quality).toBe('measured');
  });

  it('confirms absence for a genuinely empty index, where there is nothing to examine', () => {
    // Denominator zero is legitimately complete: refusing it would leave an
    // empty index permanently unable to report itself as empty.
    const empty = coverage({ eligible: 0, examined: 0, denominator: 0, matched: 0 });
    const verdict = absenceVerdict(
      run([
        source('code_graph', { coverage: empty }),
        source('sessions', { coverage: empty }),
        source('knowledge', { coverage: empty }),
      ]),
    );
    expect(verdict.confirmed).toBe(true);
  });

  // ---------------------------------------------------------------- regressions

  it('REGRESSION: refuses absence when every unit a source counted is unknown', () => {
    // `completeness: 'complete'` with a real denominator was the whole gate, so
    // this shape used to earn "completed with known coverage" while the source
    // itself said it knew the status of nothing.
    const verdict = absenceVerdict(
      run([
        source('code_graph'),
        source('sessions'),
        source('knowledge', {
          coverage: coverage({
            unknown: 5,
            matched: 0,
            unit: 'facts',
            omission_reasons: ['every unit resolved to unknown status'],
          }),
        }),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Knowledge could not determine the status of any of its 5 facts',
    );
  });

  it('REGRESSION: refuses absence when a source examined none of its units', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph', {
          coverage: coverage({
            eligible: 400,
            examined: 0,
            omitted: 400,
            denominator: 400,
            omission_reasons: ['result cap reached before any unit was examined'],
          }),
        }),
        source('sessions'),
        source('knowledge'),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Code graph examined none of its 400 symbols',
    );
  });

  it('names a partial accounting distinctly from examining nothing', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph', { coverage: coverage({ examined: 3, unknown: 1, omitted: 1 }) }),
        source('sessions'),
        source('knowledge'),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Code graph left 1 unknown and 1 omitted of its 5 symbols',
    );
  });

  it('refuses a complete claim that does not account for its denominator', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph', {
          coverage: coverage({ examined: null, unknown: null, omitted: null }),
        }),
        source('sessions'),
        source('knowledge'),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Code graph claims complete coverage without accounting for its 5 symbols',
    );
  });

  it('reports a declared-incomplete source by the completeness it declared', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph'),
        source('sessions'),
        source('knowledge', {
          total_units: null,
          coverage: coverage({
            completeness: 'unknown',
            eligible: null,
            examined: null,
            omitted: null,
            unknown: null,
            denominator: null,
            unit: 'facts',
          }),
        }),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Knowledge reports unknown coverage',
    );
  });

  it('refuses absence while the coordinator withholds canonical finality', () => {
    const verdict = absenceVerdict(
      run([source('code_graph'), source('sessions'), source('knowledge')], 'partial'),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'the coordinator has not declared canonical finality',
    );
  });

  it('refuses absence when no run has answered', () => {
    expect(absenceVerdict(undefined).confirmed).toBe(false);
  });
});
