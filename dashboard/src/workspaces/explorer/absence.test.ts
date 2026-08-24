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

const SOURCE_LABELS = {
  code_graph: 'Code graph',
  sessions: 'Sessions',
  knowledge: 'Knowledge',
  semantic: 'Semantic',
} as const;

function source(id: keyof typeof SOURCE_LABELS, over: Record<string, unknown> = {}) {
  return {
    source_id: id,
    source_label: SOURCE_LABELS[id],
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

/** The live semantic source today: not activated, typed absent, with the
 * complete accounting of an empty domain its constructor carries. */
function semanticAbsent(over: Record<string, unknown> = {}) {
  return source('semantic', {
    outcome: 'absent',
    completed_units: 0,
    total_units: 0,
    coverage: coverage({
      eligible: 0,
      examined: 0,
      matched: 0,
      denominator: 0,
      unit: 'indexed vectors',
    }),
    error_code: 'semantic_not_activated',
    message: 'semantic search is not activated for this project',
    page: null,
    ...over,
  });
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
    required_source_ids: ['code_graph', 'sessions', 'knowledge', 'semantic'],
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
    // The semantic source rides along as the live typed absence: complete
    // coverage of an empty domain must not block absence the way a failed
    // read does — otherwise absence claims are permanently unearnable in the
    // default install, where semantic search is not activated.
    const verdict = absenceVerdict(
      run([source('code_graph'), source('sessions'), source('knowledge'), semanticAbsent()]),
    );
    expect(verdict.confirmed).toBe(true);
    expect(verdict.quality).toBe('measured');
  });

  it('refuses an absence claim from an absent source without complete-zero accounting', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph'),
        source('sessions'),
        source('knowledge'),
        semanticAbsent({
          coverage: coverage({
            completeness: 'unknown',
            eligible: null,
            examined: null,
            matched: null,
            excluded: null,
            omitted: null,
            unknown: null,
            denominator: null,
            unit: 'indexed vectors',
          }),
        }),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Semantic reports unknown coverage',
    );
  });

  it('blocks absence while the semantic provider is still indexing', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph'),
        source('sessions'),
        source('knowledge'),
        source('semantic', {
          outcome: 'indexing',
          completed_units: 3,
          total_units: 10,
          coverage: coverage({
            completeness: 'partial',
            eligible: 10,
            examined: 3,
            denominator: 10,
            unit: 'semantic units',
            omission_reasons: ['semantic vector projection is in progress'],
          }),
          error_code: 'semantic_indexing',
          page: null,
        }),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Semantic is still building its index',
    );
  });

  it('blocks absence when a source answered with omitted records', () => {
    const verdict = absenceVerdict(
      run([
        source('code_graph'),
        source('sessions', {
          outcome: 'partial',
          error_code: 'lcm_temporal_read_incomplete',
        }),
        source('knowledge'),
        semanticAbsent(),
      ]),
    );
    expect(verdict.confirmed).toBe(false);
    expect(verdict.confirmed === false && verdict.reason).toBe(
      'Sessions answered with omitted records',
    );
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
