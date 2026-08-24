/**
 * The absence claims — Explorer. A coordinator run that answered with no rows
 * has to say which kind of nothing it found.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: these three scenarios need payload builders
 * nothing else uses, and the builders are most of their weight. A coordinator
 * run carries a plan revision, a merge revision, an ordering policy and a
 * per-source unit accounting, and all three scenarios differ only in the
 * coverage numbers under one source.
 *
 * Not to be confused with `axe-explorer.ts`, a separate harness over the same
 * route, which explains there why /explorer is absent from this gate's Plan 11
 * matrix subset.
 */
import { resolveFixture } from '../stories/fixtures/data.ts';
import { expectAbsent, expectVisibleText, searchFor, type Scenario } from './axe-harness.ts';

const EXPLORER_QUERIES = '/api/explorer/queries';

/** One Explorer source's coverage, defaulting to a fully accounted denominator. */
function sourceCoverage(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    completeness: 'complete',
    eligible: 0,
    examined: 0,
    matched: 0,
    excluded: 0,
    omitted: 0,
    unknown: 0,
    denominator: 0,
    unit: 'rows',
    omission_reasons: [],
    ...over,
  };
}

function explorerSource(
  id: 'code_graph' | 'sessions' | 'knowledge' | 'semantic',
  label: string,
  coverage: Record<string, unknown>,
): Record<string, unknown> {
  return {
    source_id: id,
    source_label: label,
    phase: 'completed',
    outcome: 'ready',
    completed_units: 0,
    total_units: coverage['denominator'],
    coverage,
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
  };
}

/** The live semantic source: not activated, a typed absence carrying the
 * complete accounting of an empty domain. */
function semanticAbsentSource(): Record<string, unknown> {
  return {
    ...explorerSource('semantic', 'Semantic', sourceCoverage({ unit: 'indexed vectors' })),
    outcome: 'absent',
    error_code: 'semantic_not_activated',
    message: 'semantic search is not activated for this project',
    page: null,
  };
}

/**
 * A coordinator run that answered with no rows, for the term the scenario
 * searches.
 *
 * `finality` is `complete` in every case here on purpose: the point of these
 * scenarios is that the coordinator's summary scalar says "canonical", and the
 * surface must still read the per-source unit accounting underneath it before
 * repeating that as a global-absence claim.
 */
function explorerEmptyRun(query: string, sources: unknown[]): Record<string, unknown> {
  const base = structuredClone(
    resolveFixture('/api/storage/findings', '') as { payload: unknown; [k: string]: unknown },
  );
  return {
    ...base,
    domain_state: 'ready',
    payload: {
      // Keyed per query: the status poll looks up by `run_id` alone, so a shared
      // id makes the client serve a previous run, discard it as belonging to
      // another query, and wait forever.
      run_id: `ui-truth-${Buffer.from(query).toString('hex').slice(0, 12)}`,
      request: { query, limit: 50, offset: 0 },
      request_revision: 'explorer-query-request-v1',
      plan_revision: 'explorer-query-plan-v1',
      merge_revision: 'source-local-no-merge-v1',
      required_source_ids: ['code_graph', 'sessions', 'knowledge', 'semantic'],
      ordering_policy: 'source_local_no_cross_source_merge',
      explanation: 'Search each required source and preserve its own order and coverage.',
      submitted_at_micros: 1,
      completed_at_micros: 4_100,
      elapsed_micros: 4_099,
      state: 'completed',
      finality: 'complete',
      sources,
    },
  };
}

export const EXPLORER_SCENARIOS: readonly Scenario[] = [
  {
    id: 'explorer-absence-confirmed',
    route: '/explorer',
    drive: (page) => searchFor(page, 'confirmed-absent-token'),
    proves: 'a genuinely empty index can still report itself as empty',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('confirmed-absent-token', [
          explorerSource('code_graph', 'Code graph', sourceCoverage({ unit: 'symbols' })),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource('knowledge', 'Knowledge', sourceCoverage({ unit: 'facts' })),
          semanticAbsentSource(),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(page, 'No source matched', 'the confirmed-absence heading');
      await expectVisibleText(page, 'examined its full denominator', 'the confirmed-absence reason');
      await expectVisibleText(page, 'measured', 'measured evidence pattern');
    },
  },
  {
    id: 'explorer-absence-all-unknown',
    route: '/explorer',
    drive: (page) => searchFor(page, 'all-unknown-token'),
    proves:
      'THE DEFECT PROOF — a source whose every unit is unknown cannot yield a known-coverage claim',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('all-unknown-token', [
          explorerSource('code_graph', 'Code graph', sourceCoverage({ unit: 'symbols' })),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource(
            'knowledge',
            'Knowledge',
            sourceCoverage({
              eligible: 5,
              examined: 5,
              unknown: 5,
              denominator: 5,
              unit: 'facts',
              omission_reasons: ['every unit resolved to unknown status'],
            }),
          ),
          semanticAbsentSource(),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'could not determine the status of any of its 5 facts',
        'the all-unknown refusal reason',
      );
      await expectAbsent(
        page,
        'text=completed with known coverage',
        'no known-coverage claim when every unit is unknown',
      );
      await expectAbsent(page, 'text=No source matched', 'no confirmed-absence heading');
      await expectVisibleText(page, 'unknown', 'unknown evidence pattern');
    },
  },
  {
    id: 'explorer-absence-examined-nothing',
    route: '/explorer',
    drive: (page) => searchFor(page, 'examined-nothing-token'),
    proves: 'THE DEFECT PROOF — a source that examined nothing cannot yield a completed claim',
    overrides: {
      [EXPLORER_QUERIES]: {
        status: 200,
        body: explorerEmptyRun('examined-nothing-token', [
          explorerSource(
            'code_graph',
            'Code graph',
            sourceCoverage({
              eligible: 400,
              examined: 0,
              omitted: 400,
              denominator: 400,
              unit: 'symbols',
              omission_reasons: ['result cap reached before any unit was examined'],
            }),
          ),
          explorerSource('sessions', 'Sessions', sourceCoverage()),
          explorerSource('knowledge', 'Knowledge', sourceCoverage({ unit: 'facts' })),
          semanticAbsentSource(),
        ]),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'examined none of its 400 symbols',
        'the examined-nothing refusal reason',
      );
      await expectAbsent(
        page,
        'text=completed with known coverage',
        'no known-coverage claim when nothing was examined',
      );
    },
  },
];
