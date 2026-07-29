/**
 * The two governance reads behind /automations: the scheduler's review queues,
 * and the storage-findings producers the nav rail's Doctor dot folds into one
 * state.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: both halves need payload builders nothing else
 * uses. They sit together because they are the same failure in two places — a
 * read that did not happen, printed as a measurement that did. The scheduler
 * prints it as a `0` where a queue could not be counted; the dot prints it as a
 * clean bill of health where no producer ever looked.
 *
 * The scenarios are composed apart in `axe-audit.ts`, in the order they have
 * always run: the scheduler set, then Brain and Explorer, then the dot.
 */
import type { Page } from '@playwright/test';
import { resolveFixture } from '../stories/fixtures/data.ts';
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';

const SCHEDULER = '/api/automation/scheduler/status';
const FINDINGS = '/api/storage/findings';

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

export const AUTOMATION_SCHEDULER_SCENARIOS: readonly Scenario[] = [
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
];

/**
 * The nav rail's Doctor dot, in each state the storage-findings read can put it
 * in. Each one asserts the dot's own attribute and the label it announces;
 * `axe-audit.ts` records what became of the dot scenarios that only read them.
 */
export const STORAGE_FINDINGS_SCENARIOS: readonly Scenario[] = [
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
