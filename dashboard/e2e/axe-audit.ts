/**
 * Every scenario the axe gate drives, and what each one is evidence for.
 *
 * `npx tsx e2e/axe-audit.ts`  (run from `dashboard/`)
 *
 * A plain navigation reaches none of the states that matter here: a governance
 * read that FAILED, a health read that could not be resolved, a coordinator that
 * claims complete coverage over units it never examined. Each scenario overrides
 * only the route under test, drives the surface into one state, ASSERTS what the
 * surface then claims, and scans it.
 *
 * The assertions are the point. An earlier harness read the Doctor dot's state
 * into a JSON file and never compared it to anything; its three dot scenarios
 * were silently exercising one state for months, because its fixture did not
 * match the shape the nav rail parses. Reading a state without asserting on it
 * is how three tests become one.
 *
 * THAT THIS GATE CAN FAIL IS NOW PROVEN BY THE RUN, not by a procedure someone
 * remembers to follow. `axe-engine-canary` plants known-inaccessible markup on
 * a real surface at every viewport and theme and requires the scan to report
 * it; see that scenario for why the check runs on every scan rather than once.
 */
import type { Page } from '@playwright/test';
import { resolveFixture } from '../stories/fixtures/data.ts';
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  openRow,
  runHarness,
  searchFor,
  type Scenario,
} from './axe-harness.ts';

const STORAGE_FINDINGS_KINDS = [
  'over_budget_store',
  'orphan_store',
  'stale_branch_dbs',
  'incident_debris_present',
  'retention_backlog',
] as const;

/**
 * The storage-findings envelope with each producer's source coverage replaced.
 *
 * Built from the checked-in fixture rather than hand-rolled, so the envelope
 * and Doctor payload around `kind_statuses` stay exactly what the audit and the
 * `endpoint-fixtures` contract gate already validate. A hand-written envelope
 * that silently failed to parse would make every scenario read `unknown` and
 * look like a passing three-state dot while proving nothing.
 */
function storageFindings(
  statuses: ReadonlyArray<{ state: string; observed_entries: number }>,
): Record<string, unknown> {
  const base = structuredClone(
    resolveFixture('/api/storage/findings', '') as {
      payload: Record<string, unknown>;
      [k: string]: unknown;
    },
  );
  base.payload['kind_statuses'] = STORAGE_FINDINGS_KINDS.map((kind, i) => ({
    kind,
    state: statuses[i]!.state,
    observed_entries: statuses[i]!.observed_entries,
    reason: `${kind} source coverage reported as ${statuses[i]!.state}`,
  }));
  return base;
}

function allProducers(state: string, observed = 0) {
  return STORAGE_FINDINGS_KINDS.map(() => ({ state, observed_entries: observed }));
}

/** The scheduler payload, with the two review queues in a chosen state. */
function scheduler(review: {
  factProposals: { state: 'measured'; count: number } | { state: 'unreadable'; reason: string };
  skills: { state: 'measured'; count: number } | { state: 'unreadable'; reason: string };
}): Record<string, unknown> {
  const wire = (r: typeof review.factProposals) =>
    r.state === 'measured'
      ? { state: 'measured', count: r.count, reason: null }
      : { state: 'unreadable', count: null, reason: r.reason };
  return {
    status: 'configured',
    paused: false,
    enabled: true,
    scheduler_tick_secs: 900,
    // Null, never zero, for a queue that could not be read — exactly what
    // `automation_scheduler_api.rs` now emits.
    pending_fact_proposals:
      review.factProposals.state === 'measured' ? review.factProposals.count : null,
    pending_skills: review.skills.state === 'measured' ? review.skills.count : null,
    pending_review: { fact_proposals: wire(review.factProposals), skills: wire(review.skills) },
    now: Math.floor(Date.now() / 1000),
    last_session_activity: Math.floor(Date.now() / 1000) - 1200,
    project_config_path: '/fast/projects/tracedecay/.tracedecay/automation.toml',
    control_path: '/fast/projects/tracedecay/.tracedecay/automation.control.json',
    tasks: [
      { task: 'memory_curator', due: false, skip_reason: 'cooldown', last_scheduler_run: null },
      { task: 'session_reflector', due: true, skip_reason: null, last_scheduler_run: null },
      { task: 'skill_writer', due: false, skip_reason: 'no_new_sessions', last_scheduler_run: null },
    ],
  };
}

const SCHEDULER = '/api/automation/scheduler/status';
const FINDINGS = '/api/storage/findings';
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
  id: 'code_graph' | 'sessions' | 'knowledge',
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
      required_source_ids: ['code_graph', 'sessions', 'knowledge'],
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

/** The pending-review tiles, as rendered text keyed by label. */
async function reviewTiles(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => {
    const out: Record<string, string> = {};
    for (const legend of Array.from(document.querySelectorAll('.td-legend'))) {
      const label = (legend.textContent ?? '').trim();
      if (label !== 'pending proposals' && label !== 'pending skills') continue;
      const cell = legend.parentElement?.querySelector('[data-cell="numeric"]');
      out[label] = (cell?.textContent ?? '').trim();
    }
    return out;
  });
}

async function doctorDotState(page: Page): Promise<{ state: string; label: string }> {
  const dot = page.locator('[data-doctor-health]').first();
  await dot.waitFor({ state: 'attached', timeout: 15_000 });
  return {
    state: (await dot.getAttribute('data-doctor-health')) ?? '',
    label: (await dot.getAttribute('aria-label')) ?? '',
  };
}

/**
 * Two rules, chosen because they are detected by different means: `image-alt`
 * is a missing-attribute check, `button-name` runs axe's accessible-name
 * computation. A scan that reports both is exercising more than one code path
 * inside the engine.
 */
const SEEDED_DEFECTS = ['image-alt', 'button-name'] as const;

/**
 * Plant the known-inaccessible markup the canary scan must find.
 *
 * Injected into the live page rather than served as a static fixture so the
 * proof runs through the *same* path every real scenario uses — this build's
 * bundle, this context, this stillness init, this `AxeBuilder` tag set. A
 * separate hand-written HTML page would prove that axe works somewhere, which
 * is not the question; the question is whether these scans would have caught
 * anything.
 */
async function seedKnownDefects(page: Page): Promise<void> {
  const planted = await page.evaluate(() => {
    const main = document.querySelector('main#td-main');
    if (!main) return -1;
    const host = document.createElement('div');
    host.setAttribute('data-axe-canary', '');
    // A 1x1 transparent GIF inlined as a data URI: the scan must never depend
    // on a network fetch, and axe only needs the element, not the pixels.
    const img = document.createElement('img');
    img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
    img.style.cssText = 'width:24px;height:24px';
    host.append(img);
    // No text, no aria-label, no title: nothing for the accessible-name
    // computation to find.
    const button = document.createElement('button');
    button.type = 'button';
    button.style.cssText = 'width:24px;height:24px';
    host.append(button);
    main.prepend(host);
    return host.childElementCount;
  });
  if (planted !== SEEDED_DEFECTS.length) {
    throw new Error(
      `the canary planted ${planted} elements, expected ${SEEDED_DEFECTS.length}` +
        (planted === -1 ? ' (main#td-main was not in the page)' : ''),
    );
  }
}

const SCENARIOS: readonly Scenario[] = [
  {
    id: 'axe-engine-canary',
    route: '/automations',
    proves:
      'THE GATE ITSELF — a known-inaccessible element on a real surface is reported, so the zeros elsewhere in this run are measurements',
    overrides: {},
    drive: seedKnownDefects,
    // Checked on every scan, not once: the seeding is re-applied after each
    // navigation, so each viewport and theme is an independent confirmation
    // that the scan running at that size is live. It also means a breakage
    // that only shows up at one width cannot hide behind five good scans.
    expectViolations: SEEDED_DEFECTS,
    assert: async (page) => {
      // The planted nodes must really be on the page and really be visible. An
      // element the browser never laid out is one axe skips, which would turn
      // the whole canary into a check that cannot fail.
      const canary = page.locator('[data-axe-canary]');
      const hosts = await canary.count();
      if (hosts !== 1) throw new Error(`expected one canary host in the page, found ${hosts}`);
      if (!(await canary.isVisible())) {
        throw new Error('the canary markup is present but not visible, so axe would skip it');
      }
    },
  },
  {
    id: 'automations-measured',
    route: '/automations',
    proves: 'a real count still renders as a measured figure',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 5 },
          skills: { state: 'measured', count: 2 },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '5', 'measured proposals tile');
      expectEqual(tiles['pending skills'], '2', 'measured skills tile');
      await expectVisibleText(page, 'measured', 'measured evidence pattern');
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'no unknown banner on a measured read',
      );
    },
  },
  {
    id: 'automations-confirmed-empty',
    route: '/automations',
    proves: 'a queue that was read and is genuinely empty may still say zero',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 0 },
          skills: { state: 'measured', count: 0 },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '0', 'confirmed-empty proposals tile');
      expectEqual(tiles['pending skills'], '0', 'confirmed-empty skills tile');
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'a confirmed empty queue is not reported as unknown',
      );
    },
  },
  {
    id: 'automations-unreadable',
    route: '/automations',
    proves: 'THE DEFECT 1 PROOF — both governance queues failed to read, and neither renders 0',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: {
            state: 'unreadable',
            reason: 'the project fact authority could not be read: database is locked',
          },
          skills: {
            state: 'unreadable',
            reason: 'the managed skill store could not be read: permission denied',
          },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      // The whole defect, asserted directly: a failed read must not print a 0.
      expectEqual(tiles['pending proposals'], '—', 'unreadable proposals tile');
      expectEqual(tiles['pending skills'], '—', 'unreadable skills tile');
      if (tiles['pending proposals'] === '0' || tiles['pending skills'] === '0') {
        throw new Error('FALSIFIED: a failed governance read rendered as a measured zero');
      }
      await expectVisibleText(page, 'unknown', 'unknown evidence pattern');
      await expectVisibleText(
        page,
        'Awaiting-review counts are unknown, not zero.',
        'the unknown-not-zero sentence',
      );
      await expectVisibleText(page, 'database is locked', 'the fact-authority failure reason');
      await expectVisibleText(page, 'permission denied', 'the skill-store failure reason');
    },
  },
  {
    id: 'automations-mixed',
    route: '/automations',
    proves: 'one unreadable queue never suppresses the other queue’s real count',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: scheduler({
          factProposals: { state: 'measured', count: 3 },
          skills: {
            state: 'unreadable',
            reason: 'the user profile root could not be resolved: no home directory',
          },
        }),
      },
    },
    assert: async (page) => {
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '3', 'measured proposals beside an unreadable queue');
      expectEqual(tiles['pending skills'], '—', 'unreadable skills tile');
      await expectVisibleText(page, 'no home directory', 'the profile-root failure reason');
    },
  },
  {
    id: 'automations-uncontracted-payload',
    route: '/automations',
    proves:
      'a scheduler payload missing the pending_review union reads as unsupported schema, never as counts',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        // The flat `pending_*` mirrors without the discriminated union. The
        // bundle ships inside the binary that answers this route, so this is
        // not a version skew the surface may paper over with the mirrors: it
        // is a payload that does not satisfy the generated contract.
        body: {
          status: 'configured',
          paused: false,
          enabled: true,
          scheduler_tick_secs: 900,
          pending_fact_proposals: null,
          pending_skills: null,
          now: Math.floor(Date.now() / 1000),
          last_session_activity: null,
          project_config_path: '/x/automation.toml',
          control_path: '/x/automation.control.json',
          tasks: [],
        },
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'The daemon answered with a shape this build does not understand.',
        'the unsupported-schema sentence',
      );
      // No tile at all is the point: an uncontracted payload must not be
      // mined for a number, and must not print a zero in place of one.
      const tiles = await reviewTiles(page);
      if ('pending proposals' in tiles || 'pending skills' in tiles) {
        throw new Error(
          `FALSIFIED: an uncontracted scheduler payload still rendered review tiles: ${JSON.stringify(tiles)}`,
        );
      }
      await expectAbsent(
        page,
        'text=Awaiting-review counts are unknown',
        'no partial scheduler panel behind an unsupported-schema state',
      );
    },
  },
  {
    id: 'brain',
    route: '/brain',
    proves: 'Brain definition lists are well-formed (definition-list / dlitem)',
    overrides: {},
    assert: async (page) => {
      // Structural check independent of axe: every dt/dd sits directly in a dl
      // or in a div wrapper whose parent is the dl, and each group's dt
      // precedes its dd in DOM order.
      const bad = await page.evaluate(() => {
        const problems: string[] = [];
        for (const dl of Array.from(document.querySelectorAll('dl'))) {
          for (const child of Array.from(dl.children)) {
            const tag = child.tagName;
            if (tag !== 'DT' && tag !== 'DD' && tag !== 'DIV') {
              problems.push(`dl has a ${tag} child`);
            }
            if (tag === 'DIV') {
              const kids = Array.from(child.children)
                .map((k) => k.tagName)
                .filter((t) => t !== 'SPAN');
              const dtAt = kids.indexOf('DT');
              const ddAt = kids.indexOf('DD');
              if (dtAt >= 0 && ddAt >= 0 && dtAt > ddAt) {
                problems.push(`dd precedes dt in a dl group: ${kids.join(',')}`);
              }
            }
          }
        }
        return problems;
      });
      if (bad.length > 0) throw new Error(`malformed definition lists: ${bad.join(' | ')}`);
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('Brain rendered no definition lists at all');
    },
  },
  {
    id: 'brain-scoped',
    route: '/brain',
    proves: 'the per-project Brain reached from the registry is also well-formed',
    overrides: {},
    // Scoping to a project swaps in a different set of definition lists, which
    // is why the registry view passing does not vouch for this one.
    drive: (page) => openRow(page, /tracedecay/i),
    assert: async (page) => {
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('scoped Brain rendered no definition lists at all');
    },
  },
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
  {
    id: 'navrail-healthy',
    route: '/automations',
    proves: 'DEFECT 2 state 1 — every producer looked and found nothing: verified healthy',
    overrides: { [FINDINGS]: { status: 200, body: storageFindings(allProducers('real', 0)) } },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'healthy', 'doctor dot state');
      expectContains(dot.label, 'measured healthy', 'doctor dot label');
    },
  },
  {
    id: 'navrail-attention',
    route: '/automations',
    proves: 'DEFECT 2 state 2 — a producer observed real findings: attention needed',
    overrides: {
      [FINDINGS]: {
        status: 200,
        body: storageFindings([
          { state: 'real', observed_entries: 3 },
          ...allProducers('real', 0).slice(1),
        ]),
      },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'attention', 'doctor dot state');
      expectContains(dot.label, 'need attention', 'doctor dot label');
    },
  },
  {
    id: 'navrail-unknown-transport',
    route: '/automations',
    proves: 'DEFECT 2 state 3 — the storage-findings read is broken: health unknown, not all-clear',
    overrides: {
      [FINDINGS]: { status: 500, body: { detail: 'storage findings reader unavailable' } },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'unknown', 'doctor dot state');
      expectContains(dot.label, 'health unknown', 'doctor dot label');
    },
  },
  {
    id: 'navrail-unknown-nocoverage',
    route: '/automations',
    proves: 'a producer that never ran is unknown, not a clean bill of health',
    overrides: {
      [FINDINGS]: { status: 200, body: storageFindings(allProducers('unsupported', 0)) },
    },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'unknown', 'doctor dot state');
    },
  },
];

runHarness(SCENARIOS).catch((err: unknown) => {
  console.error('[axe] fatal:', err);
  process.exit(1);
});
