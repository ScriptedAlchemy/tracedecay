/**
 * Every scenario the axe gate drives, and what each one is evidence for.
 *
 * `npx tsx e2e/axe-audit.ts`  (run from `dashboard/`)
 *
 * This file is the composition root: the order the scenarios run in, and
 * nothing else. Each group sits in a sibling module beside the payload builders
 * only it uses — `axe-automations.ts` (the scheduler queues and the Doctor
 * dot), `axe-brain.ts`, `axe-explorer-absence.ts`, `axe-observatory.ts`,
 * `axe-costs.ts`, `axe-code-freshness.ts`, `axe-sessions.ts`, `axe-work.ts`,
 * and `axe-canary.ts` for every canary. Settings, Knowledge, Delivery, Loom and
 * Agents live in `axe-workspaces.ts`, which explains why they are separate.
 * What two groups share is a module they both import: `axe-envelopes.ts` for
 * the envelope a generated payload arrives in, `axe-measurements.ts` for
 * reading a metric plate.
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
 * remembers to follow. The `canary` factory plants known-inaccessible markup on
 * a real surface at every viewport and theme and requires the scan to report
 * it; see it for why the check runs on every scan, and once per audited ROUTE,
 * rather than once per run.
 *
 * Where the scenarios spend their attention, and why:
 *
 *   the states nobody looks at. An `unsupported` panel, a metric whose value
 *   does not exist, a page of a transcript that carried none of the session's
 *   summary nodes, a mount that is `ready` and separately `unauthorized` — these
 *   render least often and are reviewed least carefully, so they are where
 *   accessibility regressions survive. Most of them are unreachable by
 *   navigation, which is why each one overrides its route.
 *
 *   the reading, not the markup. Axe cannot tell whether a unit reached the
 *   accessibility tree beside the figure it scales, whether a group header's
 *   tally agrees with its own plates, or whether activating a pager left focus
 *   anywhere at all. Those are asserted directly — see
 *   `assertMeasurementIsSelfDescribing`, `assertMetricPlateTruth`, and
 *   `sessions-transcript-paged`.
 */
import { runHarness, type Scenario } from './axe-harness.ts';
import { AUTOMATION_SCHEDULER_SCENARIOS, STORAGE_FINDINGS_SCENARIOS } from './axe-automations.ts';
import { BRAIN_SCENARIOS } from './axe-brain.ts';
import { MATRIX_CANARIES, SHOWCASE_CANARIES } from './axe-canary.ts';
import { CODE_FRESHNESS_SCENARIOS } from './axe-code-freshness.ts';
import { COSTS_SCENARIOS } from './axe-costs.ts';
import { EXPLORER_SCENARIOS } from './axe-explorer-absence.ts';
import { OBSERVATORY_SCENARIOS } from './axe-observatory.ts';
import { SESSIONS_SCENARIOS } from './axe-sessions.ts';
import { WORK_SCENARIOS } from './axe-work.ts';
import { WORKSPACE_SCENARIOS } from './axe-workspaces.ts';

const SCENARIOS: readonly Scenario[] = [
  ...MATRIX_CANARIES,
  ...AUTOMATION_SCHEDULER_SCENARIOS,
  ...BRAIN_SCENARIOS,
  ...EXPLORER_SCENARIOS,
  ...STORAGE_FINDINGS_SCENARIOS,
  ...OBSERVATORY_SCENARIOS,
  ...COSTS_SCENARIOS,
  ...CODE_FRESHNESS_SCENARIOS,
  ...SESSIONS_SCENARIOS,
  ...WORK_SCENARIOS,

  // The five workspaces this gate did not visit. Their scenarios live in
  // `axe-workspaces.ts`; their canaries stay with the other five in
  // `axe-canary.ts`, so the per-route liveness rule is legible in one place.
  ...WORKSPACE_SCENARIOS,
  ...SHOWCASE_CANARIES,
];

runHarness(SCENARIOS).catch((err: unknown) => {
  console.error('[axe] fatal:', err);
  process.exit(1);
});
