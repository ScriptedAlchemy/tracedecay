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
 * Five things here exist specifically to stop the gate from passing itself:
 *
 *  1. NON-ZERO EXIT ON ANY FAILURE. Violations, page errors, and failed
 *     assertions all set `process.exitCode = 1`.
 *
 *  2. THE ENGINE PROVES IT CAN SEE A VIOLATION, EVERY RUN. A scenario may
 *     declare `expectViolations`: rule ids it deliberately seeds into the page.
 *     Those are excluded from the gate's violation total — they are planted, so
 *     counting them would hold the gate permanently red — and their ABSENCE is
 *     an assertion failure. That inverts the dangerous direction: a scan that
 *     silently stops reporting anything (wrong tags, a page that never
 *     rendered, an `analyze()` that resolves empty) now fails loudly instead of
 *     scoring every surface clean. A zero only means something once something
 *     known-bad was detected in the same run, through the same code path.
 *
 *  3. STILLNESS IS `data-motion`, NOT `animation-duration: 0s`. An animation
 *     with `both` fill and zero duration holds its from-state — `opacity: 0` —
 *     permanently, so a harness that shortens animations photographs invisible
 *     content, and Axe reports invisible content as clean. Stillness goes
 *     through the app's own switch (`data-motion="reduced"`) plus a
 *     belt-and-braces `animation: none`.
 *
 *  4. THE BUILT BUNDLE, NOT `rsbuild dev`. Lazy route compilation under the dev
 *     server can race and throw (`factory is undefined`), which renders the
 *     router's error boundary — accessible markup that screenshots happily and
 *     passes Axe. A crashed page must not score a clean bill of health.
 *
 *  5. ANY `pageerror` FAILS THE RUN. Same reason.
 *
 * WHAT IT SWEEPS. Plan 11 mandates 320x568, 390x844, 768x1024, 1024x768,
 * 1280x720 and 1440x900 CSS pixels, 200% and 400% zoom, reduced motion,
 * `prefers-contrast: more` and forced colors — and two measurements no axe rule
 * performs: no page-level horizontal scroll at 320 and 400% zoom, and touch
 * targets of at least 44x44 CSS pixels. `responsive.ts` holds the matrix and
 * both probes; see it for why zoom is emulated as a CSS viewport rather than
 * `deviceScaleFactor`, and for each touch-target exemption.
 *
 * The cross of all of that against every scenario is a run nobody keeps, so it
 * is deliberately split: every scenario pays for three showcase viewports in
 * two themes, and a representative scenario per audited route — preferring the
 * ones carrying a canary — pays for the rest of the matrix. Scenarios run
 * concurrently, which is what buys the extra coverage back.
 *
 * Env:
 *   AXE_PORT         Static-server port (default 5241).
 *   AXE_LABEL        Output subdirectory under `.axe-audit/` (default `current`).
 *   AXE_ONLY         Substring filter over scenario ids.
 *   AXE_CONCURRENCY  Scenarios in flight (default 3). `1` restores ordered logs.
 *   AXE_TRAP         `1` reverts to the pre-fix stillness, to demonstrate defect 2.
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
import {
  FORCED_COLORS_PROBE,
  HEADER_BOX_PROBE,
  REFLOW_PROBE,
  RESPONSIVE_MATRIX,
  SHOWCASE_VIEWPORTS,
  TOUCH_TARGET_PROBE,
  clippedContentFailures,
  combinationTag,
  headerBoxFailures,
  reflowFailures,
  touchTargetFailures,
  type Combination,
  type ForcedColorsOptOut,
  type HeaderBoxReport,
  type MediaMode,
  type ReflowReport,
  type Theme,
  type TouchTargetReport,
  type Viewport,
} from './responsive.ts';
import {
  reportRun,
  type PlanFailure,
  type ScenarioRecord,
  type Violation,
} from './axe-report.ts';

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
const THEMES: readonly Theme[] = ['light', 'dark'];
const PORT = Number(process.env['AXE_PORT'] ?? 5241);
/**
 * How many scenarios are driven at once.
 *
 * Scenarios were sequential when the set was fourteen. It is thirty-five now
 * and the sweep is wider — the Plan 11 viewport, zoom and media matrix on top
 * of the showcase sweep — so sequential execution puts the gate past the point
 * where anyone will keep it in CI. Each scenario already owns a private browser
 * context, private route overrides and a private react-query cache, so nothing
 * is shared but the static server and the accumulator this pool merges into.
 *
 * Three is chosen against the plan's own 4 vCPU runner: axe's analysis is
 * CPU-bound in the page, so a fourth worker mostly contends. Override with
 * `AXE_CONCURRENCY=1` to get the old strictly-ordered log back when debugging.
 */
const CONCURRENCY = Math.max(1, Number(process.env['AXE_CONCURRENCY'] ?? 3));
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
/**
 * Comma-separated substring filter over scenario ids, so one state can be
 * iterated on without paying for the whole matrix across all of them.
 *
 * A list rather than one substring because the ids of related scenarios do not
 * share a prefix — `AXE_ONLY=knowledge,delivery,loom` is otherwise three runs,
 * each paying for its own bundle build, which is enough friction to push
 * someone into running the whole gate to check one route.
 */
const ONLY = process.env['AXE_ONLY'] ?? '';
const ONLY_PARTS = ONLY.split(',')
  .map((part) => part.trim())
  .filter((part) => part !== '');

/**
 * A JSON body, or an explicit HTTP failure to simulate a broken read.
 *
 * `bodyFor` receives the intercepted request URL, which is what makes a
 * server-paginated surface auditable: a fixed body answers `offset=100` with
 * page one, so the pager appears to work while nothing moves, and a focus or
 * range assertion against it would be measuring the fixture rather than the
 * surface.
 */
type Override =
  | { status: number; body: unknown }
  | { status: number; bodyFor: (url: URL) => unknown };

function overrideBody(override: Override, url: string): unknown {
  return 'bodyFor' in override ? override.bodyFor(new URL(url)) : override.body;
}

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
  /**
   * Asserted on EVERY scan — each viewport, theme and media mode — rather than
   * once at 1440/light.
   *
   * `assert` answers "is the reading true", which does not change with width,
   * and paying for it thirty more times per scenario would buy nothing. This
   * answers a question that only has meaning per combination: is the sentence
   * still on screen and still readable at 320 CSS px, at 400% zoom, under
   * forced colors. A refusal a control depends on is exactly the kind of text
   * that survives a 1440 assertion and is clipped away at 320, and no axe rule
   * measures whether a specific sentence is reachable.
   *
   * Receives the combination tag so a failure names where it happened.
   */
  readonly assertEachScan?: (page: Page, tag: string) => Promise<void>;
  /**
   * Axe rule ids this scenario deliberately plants, via `drive`, to prove the
   * scan can still see a violation.
   *
   * Every listed id must appear in EVERY scan of this scenario or the run
   * fails, and none of them count toward the gate's violation total. Only a
   * scenario that seeds the markup itself may declare this — it is the one
   * place where a violation is the expected result, and it exists so that
   * `axe violations=0` on the other scenarios is a measurement rather than an
   * assumption.
   */
  readonly expectViolations?: readonly string[];
  /**
   * Also sweep this scenario through the Plan 11 viewport/zoom/media matrix.
   *
   * The full scenario set runs at the three showcase viewports in both themes;
   * crossing all thirty-five of them with six more viewports and three media
   * modes would be a six-hundred-scan run nobody would keep. The matrix runs
   * instead over a representative scenario per audited route, and the routes
   * that carry a canary are preferred for it: the seeded rules must be
   * reported at every one of those combinations too, so a clean 390x844 or
   * forced-colors scan is a measurement rather than a scan that quietly
   * stopped looking.
   */
  readonly matrix?: boolean;
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
 * What holds focus right now, as `tag:name`, or `body` when nothing does.
 *
 * `body` is the failure this exists to name. A control that disables itself in
 * response to its own activation — the last page of a pager — is removed from
 * the tab order while focused, and the browser drops focus to the document. A
 * keyboard user is then returned to the top of the page with no indication that
 * anything happened, which no axe rule can see because the markup is faultless
 * until the moment it is used.
 */
export async function focusedElement(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement;
    if (el === null || el === document.body || el === document.documentElement) return 'body';
    const name = (el.getAttribute('aria-label') ?? el.textContent ?? '').trim().replace(/\s+/g, ' ');
    return `${el.tagName.toLowerCase()}${name === '' ? '' : `:${name.slice(0, 60)}`}`;
  });
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

/**
 * Rules that must be switched off in a given media mode because they measure
 * the wrong thing there — never because they are inconvenient.
 *
 * `color-contrast` under forced colors is the only entry, and
 * `FORCED_COLORS_PROBE` in `responsive.ts` carries the measurements that
 * justify it plus the direct check that replaces it. Every other rule, and
 * `color-contrast` in every other mode, runs unchanged.
 */
function disabledRules(media: MediaMode): string[] {
  return media === 'forced-colors' ? ['color-contrast'] : [];
}

export async function runAxe(page: Page, media: MediaMode = 'reduced-motion'): Promise<Violation[]> {
  const builder = new AxeBuilder({ page }).withTags([
    'wcag2a',
    'wcag2aa',
    'wcag21a',
    'wcag21aa',
  ]);
  const off = disabledRules(media);
  const results = await (off.length > 0 ? builder.disableRules(off) : builder).analyze();
  return results.violations.map((v) => ({
    id: v.id,
    impact: v.impact ?? 'unknown',
    help: v.help,
    nodes: v.nodes.map((n) => `${n.target.join(' ')} :: ${n.html.slice(0, 160)}`),
  }));
}

/**
 * Split a scan into planted violations and real ones, and fail if a planted
 * rule went unreported.
 *
 * A scenario with no `expectViolations` is unchanged: everything is real.
 */
function partitionViolations(
  violations: Violation[],
  expected: readonly string[] | undefined,
  tag: string,
): { real: Violation[]; seeded: Violation[] } {
  if (expected === undefined || expected.length === 0) {
    return { real: violations, seeded: [] };
  }
  const wanted = new Set(expected);
  const seeded = violations.filter((v) => wanted.has(v.id));
  const real = violations.filter((v) => !wanted.has(v.id));
  const found = new Set(seeded.map((v) => v.id));
  const missing = expected.filter((id) => !found.has(id));
  if (missing.length > 0) {
    throw new Error(
      `${tag}: the axe scan did not report the seeded violation(s) ${missing.join(', ')}. ` +
        `The engine is not detecting known-inaccessible markup, so every clean ` +
        `scan in this run is worthless. Reported instead: ` +
        `${violations.length === 0 ? '(nothing)' : violations.map((v) => v.id).join(', ')}`,
    );
  }
  return { real, seeded };
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

/**
 * The three media conditions the plan names, applied live.
 *
 * All three resolve through CSS media queries, so none of them needs a reload:
 * `prefers-contrast` and `forced-colors` were both confirmed to switch real
 * computed style in the pinned Chromium, not merely to flip `matchMedia`.
 * Every axis is set explicitly on every call, so leaving forced colors behind
 * on the next mode is not possible.
 */
async function applyMedia(page: Page, media: MediaMode): Promise<void> {
  await page.emulateMedia({
    reducedMotion: TRAP_MODE ? 'no-preference' : 'reduce',
    contrast: media === 'contrast-more' ? 'more' : 'no-preference',
    forcedColors: media === 'forced-colors' ? 'active' : 'none',
  });
}

/** Everything one scenario produced, so workers accumulate privately and the
 * caller merges once. Sharing the counters would make totals depend on
 * interleaving. */
interface ScenarioRun {
  record: ScenarioRecord;
  planFailures: PlanFailure[];
  pageErrors: string[];
  assertionFailures: number;
  log: string[];
}

/**
 * One page state: prove it rendered, photograph it, scan it, and take the two
 * measurements the plan names that no axe rule performs.
 */
async function captureAndScan(
  page: Page,
  scenario: Scenario,
  viewport: Viewport,
  theme: Theme,
  media: MediaMode,
  run: ScenarioRun,
): Promise<void> {
  const file = `${scenario.id}__${theme}__${viewport.id}__${media}.png`;
  const tag = `${scenario.id}/${theme}/${viewport.id}/${media}`;
  await assertActuallyVisible(page, tag);
  // Before the screenshot and the scan, so a failure names the state that was
  // photographed rather than describing one taken afterwards.
  if (scenario.assertEachScan !== undefined) await scenario.assertEachScan(page, tag);
  await page.screenshot({ path: path.join(OUT_DIR, file), fullPage: true });
  const scanned = await runAxe(page, media);
  const { real: violations, seeded } = partitionViolations(scanned, scenario.expectViolations, tag);
  const reflow = (await page.evaluate(REFLOW_PROBE)) as ReflowReport;
  const targets = (await page.evaluate(TOUCH_TARGET_PROBE)) as TouchTargetReport;
  const headerBox = (await page.evaluate(HEADER_BOX_PROBE)) as HeaderBoxReport;
  const optOuts =
    media === 'forced-colors'
      ? ((await page.evaluate(FORCED_COLORS_PROBE)) as ForcedColorsOptOut[])
      : [];

  // Reflow is measured at every size and recorded at every size, but only
  // gated where the plan gates it: "at 320 pixels and 400% zoom there is no
  // page-level horizontal scroll". A wide layout that scrolls sideways is a
  // different and lesser complaint, and folding it in here would let the
  // gate's meaning drift away from the sentence it implements.
  const failures: PlanFailure[] = [
    ...(viewport.reflowGated ? reflowFailures(reflow, tag) : []).map(
      (detail): PlanFailure => ({
        scenario: scenario.id,
        route: scenario.route,
        tag,
        check: 'horizontal-scroll',
        detail,
      }),
    ),
    ...(viewport.reflowGated ? clippedContentFailures(reflow, tag) : []).map(
      (detail): PlanFailure => ({
        scenario: scenario.id,
        route: scenario.route,
        tag,
        check: 'clipped-content',
        detail,
      }),
    ),
    ...touchTargetFailures(targets, tag).map(
      (detail): PlanFailure => ({
        scenario: scenario.id,
        route: scenario.route,
        tag,
        check: 'touch-target',
        detail,
      }),
    ),
    // Not gated on `viewport.reflowGated`: a child rendering outside its own
    // container is a defect at 1440 as much as at 320, and this is the one
    // measurement that catches it when the shell clips rather than scrolls.
    ...headerBoxFailures(headerBox, tag).map(
      (detail): PlanFailure => ({
        scenario: scenario.id,
        route: scenario.route,
        tag,
        check: 'header-overflow',
        detail,
      }),
    ),
  ];
  run.planFailures.push(...failures);
  run.record.shots.push({
    theme,
    viewport: viewport.id,
    width: viewport.width,
    height: viewport.height,
    zoom: viewport.zoom,
    media,
    file,
    violations,
    reflow,
    targets,
    headerBox,
    ...(optOuts.length ? { forcedColorOptOuts: optOuts } : {}),
    ...(disabledRules(media).length ? { disabledRules: disabledRules(media) } : {}),
    ...(seeded.length ? { seeded } : {}),
  });
  run.log.push(
    `[axe] ${file}  axe=${violations.length}` +
      (violations.length ? ` (${violations.map((v) => v.id).join(', ')})` : '') +
      `  reflow=${reflow.scrollWidth}/${reflow.clientWidth}` +
      `  targets=${targets.undersized.length}/${targets.examined} under 44px` +
      `  header=${headerBox.offenders.length}/${headerBox.examined} outside box` +
      (seeded.length ? `  seeded-detected=${seeded.map((v) => v.id).join(', ')}` : ''),
  );
  for (const v of violations) {
    run.log.push(`             - ${v.id} [${v.impact}] x${v.nodes.length} — ${v.help}`);
    for (const n of v.nodes.slice(0, 3)) run.log.push(`                 ${n}`);
  }
  for (const f of failures) run.log.push(`             ! ${f.check}: ${f.detail}`);
}

/**
 * Run `fn` over `items` with at most `limit` in flight, preserving input order
 * in the results.
 *
 * Deliberately not a library: the pool is six lines and the alternative is a
 * dependency in a gate whose whole argument is that it does not trust anything
 * it did not measure.
 */
async function mapWithConcurrency<T, R>(
  items: readonly T[],
  limit: number,
  fn: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  const worker = async (): Promise<void> => {
    for (let i = next++; i < items.length; i = next++) {
      results[i] = await fn(items[i]!, i);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}

export async function runHarness(scenarios: readonly Scenario[]): Promise<void> {
  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });
  const { baseURL, server } = startStaticServer();

  let browser: Browser | null = null;
  const records: ScenarioRecord[] = [];
  const planFailures: PlanFailure[] = [];
  let assertionFailures = 0;
  const pageErrors: string[] = [];

  const selected =
    ONLY_PARTS.length === 0
      ? [...scenarios]
      : scenarios.filter((s) => ONLY_PARTS.some((part) => s.id.includes(part)));
  // A full gate run with nothing planted in it cannot distinguish "accessible"
  // from "the scan reported nothing". `AXE_ONLY` is the developer's iteration
  // flag, so it may narrow past the canary — loudly, and never in CI, which
  // runs the whole set.
  const canaries = selected.filter((s) => (s.expectViolations?.length ?? 0) > 0);
  if (canaries.length === 0) {
    const message =
      'no scenario in this run seeds a known violation, so a clean scan proves nothing about the engine';
    if (ONLY === '') {
      console.error(`[axe] ${message}`);
      assertionFailures += 1;
    } else {
      console.warn(`[axe] WARNING (AXE_ONLY=${ONLY}): ${message}`);
    }
  }

  // The showcase sweep every scenario pays for, and the matrix a
  // representative subset adds on top of it. Built once so the plan is
  // printed before the run rather than inferred from the log afterwards.
  const showcase: Combination[] = SHOWCASE_VIEWPORTS.flatMap((viewport) =>
    THEMES.map((theme): Combination => ({ viewport, theme, media: 'reduced-motion' })),
  );
  const matrixScenarios = selected.filter((s) => s.matrix === true);
  console.log(
    `[axe] plan matrix: ${selected.length} scenarios x ${showcase.length} showcase combinations` +
      ` + ${matrixScenarios.length} matrix scenarios x ${RESPONSIVE_MATRIX.length} more` +
      ` = ${selected.length * showcase.length + matrixScenarios.length * RESPONSIVE_MATRIX.length}` +
      ` scans at concurrency ${CONCURRENCY}`,
  );

  try {
    browser = await chromium.launch({ headless: true });
    const live = browser;
    const runs = await mapWithConcurrency(selected, CONCURRENCY, async (scenario) => {
      const run: ScenarioRun = {
        record: {
          id: scenario.id,
          route: scenario.route,
          proves: scenario.proves,
          assertion: 'passed',
          matrix: scenario.matrix === true,
          shots: [],
        },
        planFailures: [],
        pageErrors: [],
        assertionFailures: 0,
        log: [],
      };
      // One fresh context per scenario: the route overrides and the react-query
      // cache must not leak between states. `reducedMotion` drives the app's
      // media path; the init script drives its attribute path.
      const context = await live.newContext({
        deviceScaleFactor: 1,
        ...(TRAP_MODE ? {} : { reducedMotion: 'reduce' as const }),
      });
      const page = await context.newPage();
      page.on('pageerror', (err) => {
        const message = `${scenario.id}: ${err.message}`;
        run.pageErrors.push(message);
        run.log.push(`[axe] PAGEERROR ${message}`);
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
            body: JSON.stringify(overrideBody(override, route.request().url())),
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

      /** Land on the route in the state under test, from scratch. */
      const arrive = async (viewport: Viewport, theme: Theme): Promise<void> => {
        await page.setViewportSize({ width: viewport.width, height: viewport.height });
        await page.goto(`${baseURL}${scenario.route}`, { waitUntil: 'domcontentloaded' });
        await setTheme(page, theme);
        await page.waitForSelector('main#td-main', { timeout: 20_000 });
        await sleep(900);
        if (scenario.drive !== undefined) await scenario.drive(page);
      };

      for (const { viewport, theme } of showcase) {
        const tag = `${scenario.id}/${theme}/${viewport.id}`;
        try {
          await applyMedia(page, 'reduced-motion');
          await arrive(viewport, theme);
          await captureAndScan(page, scenario, viewport, theme, 'reduced-motion', run);
          // Assert the rendered state once, at the showcase viewport.
          if (viewport.id === '1440x900' && theme === 'light' && scenario.assert) {
            await scenario.assert(page);
          }
        } catch (err) {
          const message = String(err);
          run.record.assertion = message;
          run.assertionFailures += 1;
          run.log.push(`[axe] FAILED ${tag}: ${message}`);
        }
      }

      // The rest of the plan matrix, over one representative scenario per
      // audited route. Grouped by (theme, media) and resized within a group:
      // every mode here resolves through CSS, so the state under test survives
      // a resize, and re-driving thirty times per scenario would spend the
      // whole budget re-reaching states the group already holds. If a resize
      // ever did lose the driven state, the seeded-violation check fails the
      // run rather than scoring the emptied page clean.
      if (scenario.matrix === true) {
        const groups = new Map<string, Combination[]>();
        for (const c of RESPONSIVE_MATRIX) {
          const key = `${c.theme}|${c.media}`;
          groups.set(key, [...(groups.get(key) ?? []), c]);
        }
        for (const group of groups.values()) {
          const head = group[0]!;
          try {
            await applyMedia(page, head.media);
            await arrive(head.viewport, head.theme);
          } catch (err) {
            const message = String(err);
            run.record.assertion = message;
            run.assertionFailures += 1;
            run.log.push(`[axe] FAILED ${scenario.id}/${combinationTag(head)}: ${message}`);
            continue;
          }
          for (const c of group) {
            try {
              await page.setViewportSize({ width: c.viewport.width, height: c.viewport.height });
              // Reflow, media-query re-evaluation and any resize-driven
              // re-render need to settle before anything is measured.
              await sleep(450);
              await captureAndScan(page, scenario, c.viewport, c.theme, c.media, run);
            } catch (err) {
              const message = String(err);
              run.record.assertion = message;
              run.assertionFailures += 1;
              run.log.push(`[axe] FAILED ${scenario.id}/${combinationTag(c)}: ${message}`);
            }
          }
        }
        await applyMedia(page, 'reduced-motion');
      }

      await context.close();
      // Flushed here rather than after the pool drains: a five-minute CI step
      // that prints nothing until it is over is a step nobody can tell apart
      // from a hang. Buffering to the end of the SCENARIO is what keeps a
      // scenario's lines contiguous under concurrency; buffering to the end of
      // the RUN would just be silence.
      console.log(run.log.join('\n'));
      return run;
    });

    for (const run of runs) {
      records.push(run.record);
      planFailures.push(...run.planFailures);
      pageErrors.push(...run.pageErrors);
      assertionFailures += run.assertionFailures;
    }

    // The health dot is 8px square, which is the right size on screen and
    // useless as evidence. Re-shoot just the rail row that carries it at 6x so
    // the three states can actually be compared side by side.
    for (const scenario of scenarios.filter((s) => s.id.startsWith('navrail-'))) {
      const context = await browser.newContext({
        deviceScaleFactor: 6,
        reducedMotion: 'reduce',
        viewport: { width: 1440, height: 900 },
      });
      const page = await context.newPage();
      await installApiFixtures(page);
      for (const [pathname, override] of Object.entries(scenario.overrides)) {
        await page.route(`**${pathname}*`, async (route) => {
          await route.fulfill({
            status: override.status,
            contentType: 'application/json',
            body: JSON.stringify(overrideBody(override, route.request().url())),
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

  const failed = reportRun(
    {
      label: LABEL,
      servedFrom: DIST,
      records,
      planFailures,
      pageErrors,
      assertionFailures,
      themes: THEMES,
    },
    path.join(OUT_DIR, 'findings.json'),
  );
  if (failed) process.exitCode = 1;
}
