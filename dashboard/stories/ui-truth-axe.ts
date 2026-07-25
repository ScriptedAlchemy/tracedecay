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
 * Three things here exist specifically to stop this harness from passing itself:
 *
 *  1. STILLNESS IS `data-motion`, NOT `animation-duration: 0s`. `.td-enter` and
 *     `.td-stagger > *` animate with `both` fill, so a zero-DURATION animation
 *     holds its from-state — `opacity: 0` — permanently. A harness that shortens
 *     animations photographs invisible content, and Axe reports invisible
 *     content as clean. `tokens.css` says this in as many words at the
 *     `.td-enter` rule. Stillness therefore goes through the app's own switch
 *     (`data-motion="reduced"`, which sets `--anim-enter: none`) plus a
 *     belt-and-braces `animation: none`, and `assertActuallyVisible` re-reads
 *     computed opacity from the page before any scan is believed.
 *
 *  2. THE BUILT BUNDLE, NOT `rsbuild dev`. Lazy route compilation under the dev
 *     server can race and throw (`factory is undefined`), which renders the
 *     router's error boundary — accessible markup that screenshots happily and
 *     passes Axe. A crashed page must not score a clean bill of health, so this
 *     builds once and serves the static output.
 *
 *  3. ANY `pageerror` FAILS THE RUN. Same reason.
 *
 * Its own port and output directory, because the shared audit wipes
 * `audit-gallery/` and pins 5173, and peer lanes verify concurrently.
 * Screenshots and `findings.json` land in `.ui-truth-axe/` (gitignored).
 *
 * Env:
 *   UI_TRUTH_PORT   Static-server port (default 5241).
 *   UI_TRUTH_LABEL  Output subdirectory (default `current`).
 */
import { spawnSync } from 'node:child_process';
import { createReadStream, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import path from 'node:path';
import { chromium, type Browser, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from './fixtures/route.ts';
import { resolveFixture } from './fixtures/data.ts';

const ROOT = process.cwd();
const LABEL = process.env['UI_TRUTH_LABEL'] ?? 'current';
const OUT_DIR = path.join(ROOT, '.ui-truth-axe', LABEL);
const DIST = path.join(ROOT, 'app-dist');
const THEMES = ['light', 'dark'] as const;
const WIDTHS = [320, 768, 1440] as const;
const VIEWPORT_HEIGHT = 900;
const PORT = Number(process.env['UI_TRUTH_PORT'] ?? 5241);
/**
 * Reproduce the defect this harness was fixed for, to prove it was real:
 * `UI_TRUTH_TRAP=1` reverts to the old stillness (shorten the animation instead
 * of removing it, no `data-motion`, no reduced-motion emulation). Expected
 * result is that `assertActuallyVisible` fails on faded regions — i.e. the old
 * method was photographing blank content and Axe was scoring it clean.
 */
const TRAP_MODE = process.env['UI_TRUTH_TRAP'] === '1';

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

const SCENARIOS: readonly Scenario[] = [
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
      await expectVisibleText(
        page,
        'Awaiting-review counts are unknown, not zero.',
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

function expectEqual(actual: string | undefined, expected: string, what: string): void {
  if (actual !== expected) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function expectContains(actual: string, needle: string, what: string): void {
  if (!actual.includes(needle)) {
    throw new Error(
      `${what}: expected to contain ${JSON.stringify(needle)}, got ${JSON.stringify(actual)}`,
    );
  }
}

/** Present in the DOM *and* actually visible. Presence alone is what let the
 * zero-duration-animation trap pass: `opacity: 0` text is still `textContent`. */
async function expectVisibleText(page: Page, text: string, what: string): Promise<void> {
  const locator = page.getByText(text, { exact: false }).first();
  if ((await locator.count()) === 0) throw new Error(`${what}: ${JSON.stringify(text)} not present`);
  if (!(await locator.isVisible())) {
    throw new Error(`${what}: ${JSON.stringify(text)} is present but NOT VISIBLE`);
  }
  const opacity = await locator.evaluate((el) => {
    let node: Element | null = el;
    let min = 1;
    while (node) {
      min = Math.min(min, Number.parseFloat(getComputedStyle(node).opacity || '1'));
      node = node.parentElement;
    }
    return min;
  });
  if (opacity < 0.99) {
    throw new Error(`${what}: ${JSON.stringify(text)} rendered at opacity ${opacity}`);
  }
}

async function expectAbsent(page: Page, selector: string, what: string): Promise<void> {
  const n = await page.locator(selector).count();
  if (n !== 0) throw new Error(`${what}: expected ${selector} to be absent, found ${n}`);
}

/**
 * The guard against photographing blank regions: every entrance-animated region
 * on the page must have settled to full opacity, and the main region must have
 * real painted size. Runs before every scan, at every viewport and theme.
 */
async function assertActuallyVisible(page: Page, tag: string): Promise<void> {
  const report = await page.evaluate(() => {
    const main = document.querySelector('main#td-main');
    const rect = main?.getBoundingClientRect();
    const animated = Array.from(document.querySelectorAll('.td-enter, .td-stagger > *'));
    let faded = 0;
    let worst = 1;
    for (const el of animated) {
      const o = Number.parseFloat(getComputedStyle(el).opacity || '1');
      if (o < 0.99) {
        faded += 1;
        worst = Math.min(worst, o);
      }
    }
    return {
      mainW: rect?.width ?? 0,
      mainH: rect?.height ?? 0,
      textLen: (main?.textContent ?? '').trim().length,
      animatedCount: animated.length,
      faded,
      worst,
      motion: document.documentElement.dataset['motion'] ?? 'unset',
    };
  });
  if (report.mainW < 100 || report.mainH < 100) {
    throw new Error(`${tag}: main region has no painted size (${report.mainW}x${report.mainH})`);
  }
  if (report.textLen < 40) {
    throw new Error(`${tag}: main region rendered almost no text (${report.textLen} chars)`);
  }
  if (report.faded > 0) {
    throw new Error(
      `${tag}: ${report.faded}/${report.animatedCount} entrance regions still at opacity ` +
        `${report.worst} — the capture would be blank (data-motion=${report.motion})`,
    );
  }
}

/**
 * Stillness by REMOVING animation, never by shortening it.
 *
 * `.td-enter` and `.td-stagger > *` fill `both`, so `animation-duration: 0s`
 * pins the from-state (`opacity: 0`) forever and the capture is blank while Axe
 * still calls it clean. `data-motion="reduced"` is the app's own switch and
 * resolves `--anim-enter: none`; the stylesheet is belt-and-braces for anything
 * animated outside the token system.
 *
 * Passed as source text, not a function: tsx compiles callbacks with esbuild's
 * `keepNames`, whose `__name` helper does not exist inside the page.
 */
const STILLNESS_INIT = `(function () {
  var apply = function () {
    if (document.documentElement) {
      document.documentElement.setAttribute('data-motion', 'reduced');
    }
    if (document.head) {
      var style = document.createElement('style');
      style.textContent =
        '*,*::before,*::after{animation:none!important;transition:none!important;}';
      document.head.appendChild(style);
    }
  };
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', apply);
  } else {
    apply();
  }
})();`;

/** The pre-fix stillness, kept only so `UI_TRUTH_TRAP=1` can demonstrate that
 * it renders entrance regions permanently invisible. Never the default. */
const TRAP_INIT = `(function () {
  var apply = function () {
    if (!document.head) return;
    var style = document.createElement('style');
    style.textContent =
      '*,*::before,*::after{animation-duration:0s!important;animation-delay:0s!important;transition-duration:0s!important;transition-delay:0s!important;}';
    document.head.appendChild(style);
  };
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', apply);
  } else {
    apply();
  }
})();`;

const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.map': 'application/json; charset=utf-8',
  '.ico': 'image/x-icon',
};

/** Static server over the built bundle, with SPA fallback so client routes
 * (`/automations`, `/brain`) resolve to `index.html`. */
function serveDist(): Server {
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    let file = path.join(DIST, decodeURIComponent(url.pathname));
    if (!file.startsWith(DIST)) {
      res.writeHead(403).end();
      return;
    }
    if (!existsSync(file) || statSync(file).isDirectory()) {
      file = path.join(DIST, 'index.html');
    }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] ?? 'application/octet-stream' });
    createReadStream(file).pipe(res);
  });
  server.listen(PORT, '127.0.0.1');
  return server;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
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
  nodes: string[];
  help: string;
}

async function runAxe(page: Page): Promise<Violation[]> {
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze();
  return results.violations.map((v) => ({
    id: v.id,
    impact: v.impact ?? 'unknown',
    help: v.help,
    nodes: v.nodes.map((n) => `${n.target.join(' ')} :: ${n.html.slice(0, 160)}`),
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

  console.log('[ui-truth] building the bundle (the dev server can serve a crashed route) ...');
  const build = spawnSync('npx', ['rsbuild', 'build'], {
    cwd: ROOT,
    env: { ...process.env, NO_COLOR: '1' },
    encoding: 'utf8',
  });
  if (build.status !== 0) {
    console.error(build.stdout ?? '', build.stderr ?? '');
    throw new Error(`rsbuild build failed with status ${String(build.status)}`);
  }
  if (!existsSync(path.join(DIST, 'index.html'))) {
    throw new Error(`build produced no ${path.join(DIST, 'index.html')}`);
  }
  const server = serveDist();
  const baseURL = `http://127.0.0.1:${PORT}`;
  console.log(`[ui-truth] serving ${DIST} at ${baseURL}`);

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
  const pageErrors: string[] = [];

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
      // cache must not leak between states. `reducedMotion` drives the app's
      // media path; the init script drives its attribute path.
      const context = await browser.newContext({
        deviceScaleFactor: 1,
        ...(TRAP_MODE ? {} : { reducedMotion: 'reduce' as const }),
      });
      const page = await context.newPage();
      page.on('pageerror', (err) => {
        const message = `${scenario.id}: ${err.message}`;
        pageErrors.push(message);
        console.error(`[ui-truth] PAGEERROR ${message}`);
      });
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
      // Stillness by REMOVING animation, never by shortening it: `.td-enter`
      // fills `both`, so a 0s duration pins `opacity: 0` and the capture is
      // blank. `data-motion="reduced"` is the app's own switch and sets
      // `--anim-enter: none`; the stylesheet is the belt-and-braces.
      // `addInitScript` runs before the document exists, so everything here is
      // deferred and null-guarded; touching `documentElement` at top level
      // throws, and this harness fails the run on any page error.
      await page.addInitScript({ content: TRAP_MODE ? TRAP_INIT : STILLNESS_INIT });

      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: VIEWPORT_HEIGHT });
        for (const theme of THEMES) {
          const file = `${scenario.id}__${theme}__${width}.png`;
          const tag = `${scenario.id}/${theme}/${width}`;
          try {
            await page.goto(`${baseURL}${scenario.route}`, { waitUntil: 'domcontentloaded' });
            await setTheme(page, theme);
            await page.waitForSelector('main#td-main', { timeout: 20_000 });
            await sleep(900);
            await assertActuallyVisible(page, tag);
            await page.screenshot({ path: path.join(OUT_DIR, file), fullPage: true });
            const violations = await runAxe(page);
            totalViolations += violations.length;
            record.shots.push({ theme, width, file, violations });
            console.log(
              `[ui-truth] ${file}  axe=${violations.length}` +
                (violations.length ? ` (${violations.map((v) => v.id).join(', ')})` : ''),
            );
            for (const v of violations) {
              console.log(`             - ${v.id} [${v.impact}] x${v.nodes.length} — ${v.help}`);
              for (const n of v.nodes.slice(0, 3)) console.log(`                 ${n}`);
            }
            // Assert the rendered state once, at the showcase viewport.
            if (width === 1440 && theme === 'light' && scenario.assert) {
              await scenario.assert(page);
            }
          } catch (err) {
            const message = String(err);
            record.assertion = message;
            assertionFailures += 1;
            console.error(`[ui-truth] FAILED ${tag}: ${message}`);
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
        reducedMotion: 'reduce',
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
      await page.addInitScript({ content: STILLNESS_INIT });
      for (const theme of THEMES) {
        await page.goto(`${baseURL}${scenario.route}`, { waitUntil: 'domcontentloaded' });
        await setTheme(page, theme);
        await page.waitForSelector('[data-doctor-health]', { timeout: 20_000 });
        await sleep(600);
        const row = page.locator('nav[aria-label="Workspaces"] a[href="/observatory"]');
        await row.screenshot({
          path: path.join(OUT_DIR, `closeup-${scenario.id}__${theme}.png`),
        });
      }
      await context.close();
    }
  } finally {
    if (browser) await browser.close();
    server.close();
  }

  const byViewport: Record<string, number> = {};
  const byRule: Record<string, number> = {};
  for (const r of records) {
    for (const s of r.shots) {
      const key = `${s.theme}__${s.width}`;
      byViewport[key] = (byViewport[key] ?? 0) + s.violations.length;
      for (const v of s.violations) byRule[v.id] = (byRule[v.id] ?? 0) + 1;
    }
  }
  const findings = {
    generatedAt: new Date().toISOString(),
    label: LABEL,
    servedFrom: DIST,
    widths: WIDTHS,
    themes: THEMES,
    axeTags: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
    scenarioCount: SCENARIOS.length,
    shotCount: records.reduce((n, r) => n + r.shots.length, 0),
    totalViolations,
    assertionFailures,
    pageErrors,
    violationsByViewport: byViewport,
    violationsByRule: byRule,
    scenarios: records,
  };
  writeFileSync(path.join(OUT_DIR, 'findings.json'), `${JSON.stringify(findings, null, 2)}\n`);

  console.log('');
  console.log('[ui-truth] ===== summary =====');
  console.log(`[ui-truth] scenarios=${findings.scenarioCount} shots=${findings.shotCount}`);
  console.log(
    `[ui-truth] axe violations=${totalViolations} byViewport=${JSON.stringify(byViewport)}`,
  );
  console.log(`[ui-truth] byRule=${JSON.stringify(byRule)}`);
  console.log(`[ui-truth] assertion/visibility failures=${assertionFailures}`);
  console.log(`[ui-truth] page errors=${pageErrors.length}`);
  if (totalViolations > 0 || assertionFailures > 0 || pageErrors.length > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error('[ui-truth] fatal:', err);
  process.exit(1);
});
