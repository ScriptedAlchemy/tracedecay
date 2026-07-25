/**
 * Targeted Playwright + axe gate for the truthfulness fixes in Automations,
 * Brain, and the app-shell Doctor dot.
 *
 * `npx tsx stories/ui-truth-axe.ts`
 *
 * The shared `stories/audit.ts` walks every surface in its default fixture
 * state. That is the wrong instrument for these fixes, because what each fix
 * changes is a state the default fixtures never produce: a governance read that
 * FAILED, and a health read that could not be resolved. So this harness drives
 * one scenario at a time, overriding only the route under test, and asserts the
 * rendered result as well as scanning it.
 *
 * Every scenario is captured at 320/768/1440 in light and dark, and axe must
 * report zero violations in all six. Screenshots and a machine-readable
 * `findings.json` land in `.ui-truth-axe/` (gitignored).
 *
 * Env:
 *   UI_TRUTH_PORT   Dev-server port to spawn on (default 5241).
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { chromium, type Browser, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from './fixtures/route.ts';
import { resolveFixture } from './fixtures/data.ts';

const ROOT = process.cwd();
const OUT_DIR = path.join(ROOT, '.ui-truth-axe');
const THEMES = ['light', 'dark'] as const;
const WIDTHS = [320, 768, 1440] as const;
const VIEWPORT_HEIGHT = 900;
const PORT = Number(process.env['UI_TRUTH_PORT'] ?? 5241);

type Theme = (typeof THEMES)[number];

/** A JSON body, or an explicit HTTP failure to simulate a broken read. */
type Override = { status: number; body: unknown };

interface Scenario {
  readonly id: string;
  readonly route: string;
  /** What this scenario is evidence FOR, recorded in findings.json. */
  readonly proves: string;
  readonly overrides: Readonly<Record<string, Override>>;
  /**
   * Asserted once per scenario (at 1440/light) against the rendered DOM.
   * Throwing fails the run: a clean axe scan of a falsified reading is not a
   * pass.
   */
  readonly assert?: (page: Page) => Promise<void>;
}

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

/** The pending-review tiles, as rendered text keyed by label. */
async function reviewTiles(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => {
    const out: Record<string, string> = {};
    for (const legend of document.querySelectorAll('.td-legend')) {
      const label = legend.textContent?.trim() ?? '';
      if (label !== 'pending proposals' && label !== 'pending skills') continue;
      // The readout prints its legend, then the value, then the note.
      const cell = legend.parentElement?.querySelector('[data-cell="numeric"]');
      out[label] = cell?.textContent?.trim() ?? '';
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

const SCENARIOS: readonly Scenario[] = [
  {
    id: 'automations-measured',
    route: '/automations',
    proves: 'a real zero-or-more count still renders as a measured figure',
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
      await expectVisible(page, 'text=measured', 'measured evidence pattern');
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
      await expectVisible(page, 'text=unknown', 'unknown evidence pattern');
      await expectVisible(
        page,
        'text=Awaiting-review counts are unknown, not zero.',
        'the unknown-not-zero sentence',
      );
      await expectVisible(page, 'text=database is locked', 'the fact-authority failure reason');
      await expectVisible(page, 'text=permission denied', 'the skill-store failure reason');
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
      await expectVisible(page, 'text=no home directory', 'the profile-root failure reason');
    },
  },
  {
    id: 'automations-legacy-null',
    route: '/automations',
    proves:
      'a daemon too old to send pending_review, reporting null counts, reads as unknown not zero',
    overrides: {
      [SCHEDULER]: {
        status: 200,
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
      const tiles = await reviewTiles(page);
      expectEqual(tiles['pending proposals'], '—', 'legacy null proposals tile');
      expectEqual(tiles['pending skills'], '—', 'legacy null skills tile');
      await expectVisible(
        page,
        'text=Awaiting-review counts are unknown, not zero.',
        'the unknown-not-zero sentence on a legacy payload',
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
        for (const dl of document.querySelectorAll('dl')) {
          for (const child of dl.children) {
            const tag = child.tagName;
            if (tag !== 'DT' && tag !== 'DD' && tag !== 'DIV') {
              problems.push(`dl has a ${tag} child`);
            }
            if (tag === 'DIV') {
              const kids = [...child.children].map((k) => k.tagName).filter((t) => t !== 'SPAN');
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
      if (dot.state === 'healthy') {
        throw new Error('FALSIFIED: a broken health read rendered as verified healthy');
      }
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

function expectEqual(actual: string | undefined, expected: string, what: string): void {
  if (actual !== expected) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function expectContains(actual: string, needle: string, what: string): void {
  if (!actual.includes(needle)) {
    throw new Error(`${what}: expected to contain ${JSON.stringify(needle)}, got ${JSON.stringify(actual)}`);
  }
}

async function expectVisible(page: Page, selector: string, what: string): Promise<void> {
  const n = await page.locator(selector).count();
  if (n === 0) throw new Error(`${what}: expected ${selector} to be present`);
}

async function expectAbsent(page: Page, selector: string, what: string): Promise<void> {
  const n = await page.locator(selector).count();
  if (n !== 0) throw new Error(`${what}: expected ${selector} to be absent, found ${n}`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function waitForServer(baseURL: string, timeoutMs = 120_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(baseURL, { method: 'GET', signal: AbortSignal.timeout(5_000) });
      if (res.ok || res.status === 304) return;
    } catch (err) {
      lastErr = err;
    }
    await sleep(500);
  }
  throw new Error(`server at ${baseURL} not ready: ${String(lastErr)}`);
}

async function setTheme(page: Page, theme: Theme): Promise<void> {
  await page.evaluate((t) => {
    try {
      localStorage.setItem('td-theme', t);
    } catch {
      /* storage disabled — the dataset alone still themes */
    }
    document.documentElement.dataset['theme'] = t;
  }, theme);
}

interface Violation {
  id: string;
  impact: string;
  nodes: number;
  help: string;
}

async function runAxe(page: Page): Promise<Violation[]> {
  const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze();
  return results.violations.map((v) => ({
    id: v.id,
    impact: v.impact ?? 'unknown',
    nodes: v.nodes.length,
    help: v.help,
  }));
}

interface ShotRecord {
  theme: Theme;
  width: number;
  file: string;
  violations: Violation[];
  error?: string;
}

async function main(): Promise<void> {
  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });

  const child: ChildProcess = spawn('npx', ['rsbuild', 'dev', '--port', String(PORT)], {
    cwd: ROOT,
    env: { ...process.env, NO_COLOR: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true,
  });
  child.stdout?.on('data', () => {});
  child.stderr?.on('data', () => {});
  const baseURL = `http://localhost:${PORT}`;
  console.log(`[ui-truth] waiting for ${baseURL} ...`);
  await waitForServer(baseURL);

  let browser: Browser | null = null;
  const records: Array<{
    id: string;
    route: string;
    proves: string;
    assertion: 'passed' | string;
    shots: ShotRecord[];
  }> = [];
  let totalViolations = 0;
  let assertionFailures = 0;

  try {
    browser = await chromium.launch({ headless: true });
    for (const scenario of SCENARIOS) {
      const record = {
        id: scenario.id,
        route: scenario.route,
        proves: scenario.proves,
        assertion: 'passed' as 'passed' | string,
        shots: [] as ShotRecord[],
      };
      // One fresh context per scenario: the route overrides and the react-query
      // cache must not leak between states.
      const context = await browser.newContext({ deviceScaleFactor: 1 });
      const page = await context.newPage();
      await installApiFixtures(page);
      // Registered after the fixtures, so these win for the routes under test.
      for (const [pathname, override] of Object.entries(scenario.overrides)) {
        await page.route(`**${pathname}*`, async (route) => {
          await route.fulfill({
            status: override.status,
            contentType: 'application/json',
            body: JSON.stringify(override.body),
          });
        });
      }
      await page.addInitScript(() => {
        const inject = () => {
          const style = document.createElement('style');
          style.textContent =
            '*,*::before,*::after{animation-duration:0s!important;transition-duration:0s!important;}';
          document.head.appendChild(style);
        };
        if (document.head) inject();
        else document.addEventListener('DOMContentLoaded', inject);
      });

      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: VIEWPORT_HEIGHT });
        for (const theme of THEMES) {
          const file = `${scenario.id}__${theme}__${width}.png`;
          try {
            await page.goto(`${baseURL}${scenario.route}`, { waitUntil: 'domcontentloaded' });
            await setTheme(page, theme);
            await page.waitForSelector('main#td-main', { timeout: 20_000 });
            await sleep(800);
            await page.screenshot({ path: path.join(OUT_DIR, file), fullPage: true });
            const violations = await runAxe(page);
            totalViolations += violations.length;
            record.shots.push({ theme, width, file, violations });
            console.log(
              `[ui-truth] ${file}  axe=${violations.length}` +
                (violations.length ? ` (${violations.map((v) => v.id).join(', ')})` : ''),
            );
            // Assert the rendered state once, at the showcase viewport.
            if (width === 1440 && theme === 'light' && scenario.assert) {
              await scenario.assert(page);
            }
          } catch (err) {
            const message = String(err);
            if (message.includes('expected') || message.includes('FALSIFIED')) {
              record.assertion = message;
              assertionFailures += 1;
              console.error(`[ui-truth] ASSERTION FAILED ${scenario.id}: ${message}`);
            } else {
              record.shots.push({ theme, width, file, violations: [], error: message });
              console.warn(`[ui-truth] shot failed ${file}: ${message}`);
            }
          }
        }
      }
      records.push(record);
      await context.close();
    }
    // The health dot is 8px square, which is the right size on screen and
    // useless as evidence. Re-shoot just the rail row that carries it at 6x so
    // the three states can actually be compared side by side.
    for (const scenario of SCENARIOS.filter((s) => s.id.startsWith('navrail-'))) {
      const context = await browser.newContext({
        deviceScaleFactor: 6,
        viewport: { width: 1440, height: VIEWPORT_HEIGHT },
      });
      const page = await context.newPage();
      await installApiFixtures(page);
      for (const [pathname, override] of Object.entries(scenario.overrides)) {
        await page.route(`**${pathname}*`, async (route) => {
          await route.fulfill({
            status: override.status,
            contentType: 'application/json',
            body: JSON.stringify(override.body),
          });
        });
      }
      for (const theme of THEMES) {
        await page.goto(`${baseURL}${scenario.route}`, { waitUntil: 'domcontentloaded' });
        await setTheme(page, theme);
        await page.waitForSelector('[data-doctor-health]', { timeout: 20_000 });
        await sleep(400);
        const row = page.locator('nav[aria-label="Workspaces"] a[href="/observatory"]');
        await row.screenshot({
          path: path.join(OUT_DIR, `closeup-${scenario.id}__${theme}.png`),
        });
      }
      await context.close();
    }
  } finally {
    if (browser) await browser.close();
    if (child.pid) {
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch {
        /* group already gone */
      }
    }
  }

  const byViewport: Record<string, number> = {};
  for (const r of records) {
    for (const s of r.shots) {
      const key = `${s.theme}__${s.width}`;
      byViewport[key] = (byViewport[key] ?? 0) + s.violations.length;
    }
  }
  const findings = {
    generatedAt: new Date().toISOString(),
    widths: WIDTHS,
    themes: THEMES,
    scenarioCount: SCENARIOS.length,
    shotCount: records.reduce((n, r) => n + r.shots.length, 0),
    totalViolations,
    assertionFailures,
    violationsByViewport: byViewport,
    scenarios: records,
  };
  writeFileSync(path.join(OUT_DIR, 'findings.json'), `${JSON.stringify(findings, null, 2)}\n`);

  console.log('');
  console.log('[ui-truth] ===== summary =====');
  console.log(`[ui-truth] scenarios=${findings.scenarioCount} shots=${findings.shotCount}`);
  console.log(`[ui-truth] axe violations=${totalViolations} byViewport=${JSON.stringify(byViewport)}`);
  console.log(`[ui-truth] assertion failures=${assertionFailures}`);
  if (totalViolations > 0 || assertionFailures > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error('[ui-truth] fatal:', err);
  process.exit(1);
});
