/**
 * The dashboard's axe + screenshot gate: build once, serve the built bundle,
 * drive each surface into the state under test, and fail the process on any
 * accessibility violation, page error, or assertion.
 *
 * This module is the ENGINE. Scenarios live in `axe-audit.ts`. They were one
 * file per lane until three copies drifted apart, and two of them ended in an
 * unconditional `process.exit(0)` — so they reported violations and still
 * exited clean for months. There is one engine now, and its exit code is the
 * gate.
 *
 * Four things here exist specifically to stop the gate from passing itself:
 *
 *  1. NON-ZERO EXIT ON ANY FAILURE. Violations, page errors, and failed
 *     assertions all set `process.exitCode = 1`. A harness nobody has watched
 *     fail is not a gate; `axe-audit.ts` documents how to make it fail on
 *     demand.
 *
 *  2. STILLNESS IS `data-motion`, NOT `animation-duration: 0s`. An animation
 *     with `both` fill and zero duration holds its from-state — `opacity: 0` —
 *     permanently, so a harness that shortens animations photographs invisible
 *     content, and Axe reports invisible content as clean. Stillness goes
 *     through the app's own switch (`data-motion="reduced"`) plus a
 *     belt-and-braces `animation: none`.
 *
 *  3. THE BUILT BUNDLE, NOT `rsbuild dev`. Lazy route compilation under the dev
 *     server can race and throw (`factory is undefined`), which renders the
 *     router's error boundary — accessible markup that screenshots happily and
 *     passes Axe. A crashed page must not score a clean bill of health.
 *
 *  4. ANY `pageerror` FAILS THE RUN. Same reason.
 *
 * Env:
 *   AXE_PORT    Static-server port (default 5241).
 *   AXE_LABEL   Output subdirectory under `.axe-audit/` (default `current`).
 *   AXE_ONLY    Substring filter over scenario ids.
 *   AXE_TRAP    `1` reverts to the pre-fix stillness, to demonstrate defect 2.
 */
import { spawnSync } from 'node:child_process';
import {
  cpSync,
  createReadStream,
  existsSync,
  mkdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createServer, type Server } from 'node:http';
import path from 'node:path';
import { chromium, type Browser, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from '../stories/fixtures/route.ts';
import { resolveFixture } from '../stories/fixtures/data.ts';
import {
  VISIBILITY_PROBE,
  assertVisibilityReport,
  type VisibilityReport,
} from './visibility.ts';

const ROOT = process.cwd();
const LABEL = process.env['AXE_LABEL'] ?? 'current';
const OUT_DIR = path.join(ROOT, '.axe-audit', LABEL);
/**
 * Where `rsbuild build` must write, and where this harness actually serves
 * from.
 *
 * `app-dist/` is tracked — `build.rs` embeds it into the binary — so a verify
 * run that simply built in place would leave rebuilt bundle files modified in a
 * shared checkout and invite them into someone's next commit. The build output
 * is copied out to a scratch directory and `app-dist/` is put back exactly as
 * it was found.
 */
const BUILD_DIST = path.join(ROOT, 'app-dist');
const DIST = path.join(OUT_DIR, 'bundle');
const THEMES = ['light', 'dark'] as const;
const WIDTHS = [320, 768, 1440] as const;
const VIEWPORT_HEIGHT = 900;
const PORT = Number(process.env['AXE_PORT'] ?? 5241);
/**
 * `AXE_TRAP=1` reverts to the old stillness (shorten the animation instead of
 * removing it, no `data-motion`, no reduced-motion emulation).
 *
 * Be precise about what this currently demonstrates. The mechanism is real —
 * `--anim-enter` fills `both` and opens at `opacity: 0`, so zeroing its
 * duration pins it invisible — but the only rules that apply it are
 * `.td-enter` and `.td-stagger > *`, and a repo-wide search finds those class
 * names nowhere outside `tokens.css`. **No component uses them, so the trap
 * fades nothing today and this flag does not currently reproduce a defect.**
 * It becomes a live reproduction the moment a component adopts either
 * primitive, which is exactly when `assertActuallyVisible` has to catch it.
 * Do not cite a clean `AXE_TRAP=1` run as evidence that the guard works —
 * `visibility.dom.test.ts` is the evidence, and it fails on a faded page.
 */
export const TRAP_MODE = process.env['AXE_TRAP'] === '1';
/** Substring filter over scenario ids, so one state can be iterated on without
 * paying for a full six-viewport sweep of all of them. */
const ONLY = process.env['AXE_ONLY'] ?? '';

type Theme = (typeof THEMES)[number];

/** A JSON body, or an explicit HTTP failure to simulate a broken read. */
type Override = { status: number; body: unknown };

export interface Scenario {
  readonly id: string;
  readonly route: string;
  /** What this scenario is evidence FOR, recorded in findings.json. */
  readonly proves: string;
  readonly overrides: Readonly<Record<string, Override>>;
  /** Drive the surface into the state under test before scanning — submit a
   * query, open a row, scope to a project. Most states a truthfulness gate
   * cares about are not reachable by navigation alone. */
  readonly drive?: (page: Page) => Promise<void>;
  /**
   * Asserted once per scenario (at 1440/light) against the rendered DOM.
   * Throwing fails the run: a clean axe scan of a falsified reading is not a
   * pass.
   */
  readonly assert?: (page: Page) => Promise<void>;
}

export function expectEqual(actual: string | undefined, expected: string, what: string): void {
  if (actual !== expected) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

export function expectContains(actual: string, needle: string, what: string): void {
  if (!actual.includes(needle)) {
    throw new Error(
      `${what}: expected to contain ${JSON.stringify(needle)}, got ${JSON.stringify(actual)}`,
    );
  }
}

/** Present in the DOM *and* actually visible. Presence alone is what let the
 * zero-duration-animation trap pass: `opacity: 0` text is still `textContent`. */
export async function expectVisibleText(page: Page, text: string, what: string): Promise<void> {
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

export async function expectAbsent(page: Page, selector: string, what: string): Promise<void> {
  const n = await page.locator(selector).count();
  if (n !== 0) throw new Error(`${what}: expected ${selector} to be absent, found ${n}`);
}

/**
 * The guard against photographing blank regions. Runs before every scan, at
 * every viewport and theme.
 *
 * The measurement and the pass/fail rule live in `visibility.ts`, without a
 * Playwright import, so `visibility.dom.test.ts` can drive the shipped probe
 * against a real DOM under `npm test` and watch it reject a faded page. The
 * previous version swept `.td-enter, .td-stagger > *` — primitives no
 * component uses — so it matched nothing and passed on every page regardless
 * of what was on screen.
 */
export async function assertActuallyVisible(page: Page, tag: string): Promise<void> {
  const report = (await page.evaluate(VISIBILITY_PROBE)) as VisibilityReport;
  assertVisibilityReport(report, tag);
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
export const STILLNESS_INIT = `(function () {
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

/** The pre-fix stillness, kept only so `AXE_TRAP=1` can demonstrate that
 * it renders entrance regions permanently invisible. Never the default. */
export const TRAP_INIT = `(function () {
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

/** Submit a query and wait for the coordinator run to settle. */
export async function searchFor(page: Page, term: string): Promise<void> {
  const box = page.getByRole('searchbox').first();
  await box.waitFor({ state: 'visible', timeout: 20_000 });
  await box.fill(term);
  await box.press('Enter');
  await sleep(1_400);
}

/**
 * Open the first row matching `name`, if the surface produced one.
 *
 * Tolerant of absence on purpose: a row that a fixture change stops producing
 * should surface as the scenario's own assertion failing on the state it
 * expected, not as an opaque timeout inside a click.
 */
export async function openRow(page: Page, name: RegExp): Promise<void> {
  const row = page.getByRole('button', { name }).first();
  if ((await row.count()) === 0) return;
  await row.click({ force: true });
  await sleep(1_100);
}

export async function setTheme(page: Page, theme: Theme): Promise<void> {
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

export async function runAxe(page: Page): Promise<Violation[]> {
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

/**
 * Build the bundle and serve it statically, returning its base URL.
 *
 * Exported so every harness reaches the same page the release does. Auditing
 * `rsbuild dev` is what let a crashed route — rendered as the router's own
 * accessible error boundary — score a clean Axe pass.
 */
export function startStaticServer(): { baseURL: string; server: Server } {
  buildBundleIntoScratch();
  const server = serveDist();
  const baseURL = `http://127.0.0.1:${PORT}`;
  console.log(`[axe] serving ${DIST} at ${baseURL}`);
  return { baseURL, server };
}

function buildBundleIntoScratch(): void {
  console.log('[axe] building the bundle (the dev server can serve a crashed route) ...');
  // Snapshot the tracked output before building over it, and put it back as
  // soon as the fresh bundle has been copied somewhere private.
  const snapshot = path.join(OUT_DIR, 'app-dist.orig');
  const hadDist = existsSync(BUILD_DIST);
  if (hadDist) cpSync(BUILD_DIST, snapshot, { recursive: true });
  try {
    const build = spawnSync('npx', ['rsbuild', 'build'], {
      cwd: ROOT,
      env: { ...process.env, NO_COLOR: '1' },
      encoding: 'utf8',
    });
    if (build.status !== 0) {
      console.error(build.stdout ?? '', build.stderr ?? '');
      throw new Error(`rsbuild build failed with status ${String(build.status)}`);
    }
    if (!existsSync(path.join(BUILD_DIST, 'index.html'))) {
      throw new Error(`build produced no ${path.join(BUILD_DIST, 'index.html')}`);
    }
    cpSync(BUILD_DIST, DIST, { recursive: true });
  } finally {
    rmSync(BUILD_DIST, { recursive: true, force: true });
    if (hadDist) {
      cpSync(snapshot, BUILD_DIST, { recursive: true });
      rmSync(snapshot, { recursive: true, force: true });
    }
  }
}

export async function runHarness(scenarios: readonly Scenario[]): Promise<void> {
  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });
  const { baseURL, server } = startStaticServer();

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
    for (const scenario of scenarios.filter((s) => s.id.includes(ONLY))) {
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
        console.error(`[axe] PAGEERROR ${message}`);
      });
      await installApiFixtures(page);
      // Registered after the fixtures, so these win for the routes under test.
      // The trailing `**` matters: a single `*` does not cross `/`, so an
      // override on `/api/explorer/queries` would catch the POST that creates a
      // run and miss the poll on `/api/explorer/queries/{run_id}` — which then
      // falls through to the generic fixture and reads back as a schema
      // mismatch rather than as the state under test.
      for (const [pathname, override] of Object.entries(scenario.overrides)) {
        await page.route(`**${pathname}**`, async (route) => {
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
            if (scenario.drive !== undefined) await scenario.drive(page);
            await assertActuallyVisible(page, tag);
            await page.screenshot({ path: path.join(OUT_DIR, file), fullPage: true });
            const violations = await runAxe(page);
            totalViolations += violations.length;
            record.shots.push({ theme, width, file, violations });
            console.log(
              `[axe] ${file}  axe=${violations.length}` +
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
            console.error(`[axe] FAILED ${tag}: ${message}`);
          }
        }
      }
      records.push(record);
      await context.close();
    }

    // The health dot is 8px square, which is the right size on screen and
    // useless as evidence. Re-shoot just the rail row that carries it at 6x so
    // the three states can actually be compared side by side.
    for (const scenario of scenarios.filter((s) => s.id.startsWith('navrail-'))) {
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
    scenarioCount: records.length,
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
  console.log('[axe] ===== summary =====');
  console.log(`[axe] scenarios=${findings.scenarioCount} shots=${findings.shotCount}`);
  console.log(
    `[axe] axe violations=${totalViolations} byViewport=${JSON.stringify(byViewport)}`,
  );
  console.log(`[axe] byRule=${JSON.stringify(byRule)}`);
  console.log(`[axe] assertion/visibility failures=${assertionFailures}`);
  console.log(`[axe] page errors=${pageErrors.length}`);
  if (totalViolations > 0 || assertionFailures > 0 || pageErrors.length > 0) process.exitCode = 1;
}
