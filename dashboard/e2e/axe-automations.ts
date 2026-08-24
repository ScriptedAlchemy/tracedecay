/** Browser audit scenarios for daemon-owned automation outcomes and the
 * storage-findings state shown in the navigation rail. */
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
  'incident_debris_present',
  'retention_backlog',
  'table_growth',
] as const;

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

export const AUTOMATION_SCHEDULER_SCENARIOS: readonly Scenario[] = [
  {
    id: 'automations-measured',
    route: '/automations',
    proves: 'scheduler receipts and automatic fact outcomes render without approval controls',
    overrides: {
      [SCHEDULER]: { status: 200, body: resolveFixture(SCHEDULER, '') },
    },
    assert: async (page) => {
      await expectVisibleText(page, 'configuration revision', 'scheduler configuration revision');
      await expectVisibleText(page, 'Fact application outcomes', 'automatic fact receipt panel');
      await expectVisibleText(page, 'applied', 'applied receipt state');
      await expectVisibleText(page, 'quarantined', 'quarantined receipt state');
      await expectAbsent(page, 'text=approve', 'no browser approval action');
      await expectAbsent(page, 'text=Apply', 'no browser apply action');
    },
  },
  {
    id: 'automations-confirmed-empty',
    route: '/automations',
    proves: 'an empty automatic receipt history is stated as empty',
    overrides: {
      '/api/automation/automatic-fact-receipts': {
        status: 200,
        body: { receipts: [], count: 0, limit: 50, error: '' },
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'no fact application outcomes are recorded',
        'empty receipt history',
      );
    },
  },
  {
    id: 'automations-uncontracted-payload',
    route: '/automations',
    proves: 'a scheduler payload missing its configuration revision reads as unsupported schema',
    overrides: {
      [SCHEDULER]: {
        status: 200,
        body: {
          status: 'configured',
          paused: false,
          enabled: true,
          scheduler_tick_secs: 900,
          now: Math.floor(Date.now() / 1000),
          last_session_activity: null,
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
      await expectAbsent(page, 'text=configuration revision', 'no scheduler data from malformed payload');
    },
  },
];

async function doctorDotState(page: Page): Promise<{ state: string; label: string }> {
  const dot = page.locator('[data-doctor-health]').first();
  await dot.waitFor({ state: 'attached', timeout: 15_000 });
  return {
    state: (await dot.getAttribute('data-doctor-health')) ?? '',
    label: (await dot.getAttribute('aria-label')) ?? '',
  };
}

export const STORAGE_FINDINGS_SCENARIOS: readonly Scenario[] = [
  {
    id: 'navrail-healthy',
    route: '/automations',
    proves: 'every storage producer looked and found nothing',
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
    proves: 'a storage producer observed real findings',
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
    proves: 'a broken storage-findings read remains unknown',
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
    proves: 'a producer that never ran is unknown, not healthy',
    overrides: { [FINDINGS]: { status: 200, body: storageFindings(allProducers('unsupported', 0)) } },
    assert: async (page) => {
      const dot = await doctorDotState(page);
      expectEqual(dot.state, 'unknown', 'doctor dot state');
    },
  },
];
