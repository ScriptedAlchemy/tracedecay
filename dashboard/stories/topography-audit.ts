/**
 * Interaction-state audit for the code-topography surfaces.
 *
 * `npm run visual:topography`
 *
 * `stories/audit.ts` captures each of the twelve workspaces in its INITIAL
 * render, which is the right shape for a shipped-state gallery and the wrong
 * shape for TRACE: the drill-in does not exist until a symbol is touched, and
 * the two things it is judged on — the atmosphere and the motion contract — are
 * only observable after a gesture. So this walks a scripted journey instead of a
 * route list, screenshots every step in both themes at 320/768/1440, and runs
 * axe on each one.
 *
 * The journey is the real navigation model, clicked rather than deep-linked:
 *
 *   spine          /code as it arrives — the connectivity spine
 *   trace          a hub card touched, which selects the symbol AND floods its
 *                  topography in one gesture (the hero state)
 *   trace-hover    the pointer resting on the field, so the hover bloom, whose
 *                  latency is scaled by degree, is in the frame
 *   trace-reduced  the same field with Motion pinned to Reduced — the static
 *                  composition, captured as a peer of the animated one because
 *                  it is a rendering mode and not a degradation
 *   spine-return   back out, proving the drill-in is reversible
 *
 * Two things beyond pictures are asserted here because nothing else can see
 * them:
 *
 *   the offline posture   every network request the page makes is recorded, and
 *                         any non-local origin is a failure. A font that loads
 *                         from a CDN passes every visual check and breaks the
 *                         product on an air-gapped install, so it is checked
 *                         mechanically rather than by reading the stylesheet.
 *   the type system       the resolved font family and numeric behaviour of the
 *                         display, body, mono and legend tiers, read off live
 *                         elements. Tabular figures are the load-bearing one:
 *                         digits must not change width between renders.
 *
 * Env:
 *   TOPO_BASE_URL   Use an already-running server (skips spawn).
 *   TOPO_PORT       Port to spawn the dev server on (default 5273).
 */
import { spawn } from 'node:child_process';
import { createReadStream, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import path from 'node:path';
import { chromium, type Browser, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from './fixtures/route.ts';

const ROOT = process.cwd();
const OUT_DIR = path.join(ROOT, 'topography-audit');
const DIST_DIR = path.join(ROOT, 'app-dist');

const THEMES = ['dark', 'light'] as const;
const WIDTHS = [320, 768, 1440] as const;
const VIEWPORT_HEIGHT = 900;
const PORT = Number(process.env['TOPO_PORT'] ?? 5273);

type Theme = (typeof THEMES)[number];

interface AxeResult {
  violations: number;
  byImpact: Record<string, number>;
  ruleIds: string[];
  nodes: string[];
}

interface ShotEntry {
  state: string;
  theme: Theme;
  width: number;
  file: string;
  bytes: number;
  axe: AxeResult | null;
  error?: string;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function waitForServer(baseURL: string, timeoutMs = 90_000): Promise<void> {
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
  throw new Error(`server at ${baseURL} not ready within ${timeoutMs}ms: ${String(lastErr)}`);
}

/**
 * Build the app, then serve the built output.
 *
 * Deliberately NOT `rsbuild dev`. The dev server compiles route chunks lazily,
 * and under a scripted walk that races: the Code route intermittently arrived
 * with `factory is undefined (GraphCanvas.tsx)` and rendered the router's error
 * boundary, which a screenshot-and-axe pass is perfectly happy to photograph and
 * call clean. Auditing the production bundle removes that class of false green
 * outright and has the better property anyway — the artifact under test is the
 * one `build.rs` embeds in the binary.
 */
function buildApp(): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn('npx', ['rsbuild', 'build'], {
      cwd: ROOT,
      env: { ...process.env, NO_COLOR: '1' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    child.stdout?.on('data', () => {});
    let stderr = '';
    child.stderr?.on('data', (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.on('close', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`rsbuild build exited ${String(code)}\n${stderr.slice(-4000)}`));
    });
  });
}

const CONTENT_TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.woff2': 'font/woff2',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

/**
 * Static host for `app-dist` with the SPA fallback the daemon also performs:
 * client-routed paths like `/code` are not files, so anything without an
 * extension is answered with the app shell.
 */
function startStaticServer(): { server: Server; baseURL: string } {
  const server = createServer((req, res) => {
    const requested = decodeURIComponent((req.url ?? '/').split('?')[0] ?? '/');
    let filePath = path.join(DIST_DIR, requested);
    // Contain the served tree: a traversal outside dist is answered by the shell
    // rather than reaching the filesystem.
    if (!filePath.startsWith(DIST_DIR) || !existsSync(filePath) || statSync(filePath).isDirectory()) {
      filePath = path.join(DIST_DIR, 'index.html');
    }
    res.writeHead(200, {
      'Content-Type': CONTENT_TYPES[path.extname(filePath)] ?? 'application/octet-stream',
      'Cache-Control': 'no-store',
    });
    createReadStream(filePath).pipe(res);
  });
  server.listen(PORT);
  return { server, baseURL: `http://localhost:${PORT}` };
}

async function runAxe(page: Page): Promise<AxeResult> {
  const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze();
  const byImpact: Record<string, number> = {};
  const ruleIds: string[] = [];
  const nodes: string[] = [];
  for (const v of results.violations) {
    const impact = v.impact ?? 'unknown';
    byImpact[impact] = (byImpact[impact] ?? 0) + 1;
    ruleIds.push(v.id);
    // The offending markup, so a violation is actionable from the manifest
    // alone rather than needing the run reproduced.
    for (const node of v.nodes.slice(0, 3)) nodes.push(`${v.id}: ${node.target.join(' ')}`);
  }
  return { violations: results.violations.length, byImpact, ruleIds, nodes };
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

async function setMotion(page: Page, preference: 'system' | 'reduced' | 'full'): Promise<void> {
  // Driven through the surface's own radio group when it is on screen, so this
  // exercises the control a reader actually uses rather than reaching past it
  // into storage.
  const radio = page.getByRole('radio', {
    name: preference === 'system' ? /^System/ : preference === 'reduced' ? 'Reduced' : 'Full',
  });
  if ((await radio.count()) > 0) {
    await radio.first().click();
    await sleep(250);
    return;
  }
  await page.evaluate((p) => {
    if (p === 'system') localStorage.removeItem('td.motion-preference');
    else localStorage.setItem('td.motion-preference', p);
  }, preference);
}

/** Navigate to a workspace through the nav rail, as a reader would. */
async function gotoSurface(page: Page, surfacePath: string): Promise<void> {
  const link = page.locator(`nav[aria-label="Workspaces"] a[href="${surfacePath}"]`);
  if ((await link.count()) > 0) await link.first().click();
  else await page.goto(`http://localhost:${PORT}${surfacePath}`, { waitUntil: 'domcontentloaded' });
  await page.waitForFunction((p) => location.pathname === p, surfacePath, { timeout: 15_000 });
  await page.waitForSelector('main#td-main', { timeout: 15_000 });
  await sleep(700);
}

/** The back-out control, which is the width-independent proof TRACE is open. */
function backToSpine(page: Page) {
  return page.getByRole('button', { name: /Back to spine/i });
}

/**
 * Leave TRACE if a previous step opened it.
 *
 * The drill-in is component state on a route that stays mounted, so navigating
 * to `/code` again does NOT return to the spine. Without this the first
 * iteration's TRACE stayed open for every later one: each "spine" capture
 * silently photographed the drill-in, and because the hub cards were then
 * off-screen, every subsequent open reported failure. Both symptoms had one
 * cause, and neither was visible from the axe or screenshot counts.
 */
async function ensureSpine(page: Page): Promise<void> {
  const back = backToSpine(page);
  if ((await back.count()) > 0) {
    await back.first().click();
    await sleep(600);
  }
}

/** Touch the top-ranked hub card, which selects the symbol and opens TRACE. */
async function openTrace(page: Page): Promise<boolean> {
  const card = page.locator('main#td-main button[aria-pressed]');
  // Waited for, not sampled: the ranked hubs arrive with the overview query, so
  // a fixed sleep raced them and reported "no cards" — indistinguishable from a
  // missing entry point.
  try {
    await card.first().waitFor({ state: 'visible', timeout: 20_000 });
  } catch {
    return false;
  }
  await card.first().click();
  try {
    // Keyed on the back-out control, NOT on the canvas. Below the field's
    // legibility floor the canvas is deliberately `hidden` and the symbol list
    // is the rendering — so waiting for a visible canvas declared the narrow
    // mode broken when it was in fact working as designed.
    await backToSpine(page).first().waitFor({ state: 'visible', timeout: 15_000 });
  } catch {
    return false;
  }
  // The field falls into its layout from a seeded displacement; wait for the
  // spring system to reach rest so the capture is the settled composition rather
  // than an arbitrary frame of the entrance.
  await sleep(1_800);
  return true;
}

/** Rest the pointer on the middle of the field, where the focus symbol sits. */
async function hoverField(page: Page): Promise<boolean> {
  const canvas = page.locator('[data-testid="trace-canvas"]');
  // `visible` matters here: at narrow widths the canvas is present but hidden,
  // and there is no field to hover.
  if (!(await canvas.first().isVisible().catch(() => false))) return false;
  const box = await canvas.first().boundingBox();
  if (!box) return false;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await sleep(1_000);
  return true;
}

/**
 * The typographic system as the browser actually resolved it.
 *
 * Read from live elements rather than from the stylesheet, because the question
 * is not what was written but what the cascade produced.
 */
async function readTypography(page: Page): Promise<unknown> {
  // Passed as a source string rather than a closure: esbuild (via tsx) rewrites
  // nested function declarations with a `__name` helper that does not exist in
  // the page realm, so a closure containing helpers throws `__name is not
  // defined` once serialised across the boundary.
  return page.evaluate(`(() => {
    const read = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return null;
      const style = getComputedStyle(element);
      return {
        selector,
        fontFamily: style.fontFamily,
        fontSize: style.fontSize,
        fontWeight: style.fontWeight,
        fontStretch: style.fontStretch,
        fontVariantNumeric: style.fontVariantNumeric,
        letterSpacing: style.letterSpacing,
      };
    };
    const root = getComputedStyle(document.documentElement);
    const probe = document.createElement('span');
    probe.style.position = 'absolute';
    probe.style.visibility = 'hidden';
    probe.style.fontSize = '12px';
    probe.style.fontVariantNumeric = 'tabular-nums';
    document.body.appendChild(probe);
    const tabular = {};
    for (const family of ['var(--font-sans)', 'var(--font-mono)', 'var(--font-display)']) {
      probe.style.fontFamily = family;
      const measured = [];
      for (const digits of ['1111111111', '0000000000', '8888888888', '4444444444']) {
        probe.textContent = digits;
        measured.push(Math.round(probe.getBoundingClientRect().width * 100) / 100);
      }
      measured.sort();
      tabular[family] = { widths: measured, uniform: measured[0] === measured[measured.length - 1] };
    }
    probe.remove();
    return {
      tokens: {
        sans: root.getPropertyValue('--font-sans').trim(),
        mono: root.getPropertyValue('--font-mono').trim(),
        display: root.getPropertyValue('--font-display').trim(),
        displayWidth: root.getPropertyValue('--display-width').trim(),
      },
      loadedFaces: Array.from(document.fonts).map((f) => f.family + ' ' + f.weight + ' ' + f.status),
      tiers: [
        read('body'),
        read('h1'),
        read('h2'),
        read('.td-legend'),
        read('.td-title'),
        read('.td-value'),
        read('.td-display'),
        read('[data-cell="numeric"]'),
      ].filter(Boolean),
      tabularProof: tabular,
    };
  })()`);
}

async function main(): Promise<void> {
  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });

  const preset = process.env['TOPO_BASE_URL'];
  let server: { server: Server; baseURL: string } | null = null;
  let baseURL: string;
  if (preset) {
    baseURL = preset;
  } else {
    console.log('[topo] building app-dist ...');
    await buildApp();
    server = startStaticServer();
    baseURL = server.baseURL;
  }

  console.log(`[topo] waiting for ${baseURL} ...`);
  await waitForServer(baseURL);
  console.log('[topo] server ready');

  let browser: Browser | null = null;
  const shots: ShotEntry[] = [];
  const axeTotals = { violations: 0, byImpact: {} as Record<string, number> };
  /** Every request origin the page reached for, so a CDN cannot hide. */
  const externalRequests = new Set<string>();
  /**
   * Uncaught page errors. A surface that throws still screenshots and still
   * passes axe — the router's error boundary is accessible markup — so a clean
   * gate over a crashed route is the exact false green this run has to refuse.
   */
  const pageErrors: string[] = [];
  let typography: unknown = null;
  let motionProbe: unknown = null;

  try {
    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext({ deviceScaleFactor: 1 });
    const page = await context.newPage();

    page.on('request', (request) => {
      const url = request.url();
      if (/^https?:\/\//.test(url) && !url.startsWith(baseURL)) externalRequests.add(url);
    });
    page.on('pageerror', (error) => pageErrors.push(String(error).slice(0, 400)));

    await installApiFixtures(page);
    // Animation is neutralised for CAPTURE only, so a screenshot is a stable
    // artifact instead of a race against an entrance. The motion contract is
    // covered by unit tests over the real loop, not by these stills.
    // Init script as a source string for the same `__name` reason as
    // `readTypography`: a closure with a nested helper is rewritten by esbuild
    // into something the page realm cannot evaluate, and an init script that
    // throws does so silently unless `pageerror` is being watched — which is how
    // this reset came to look installed while never running.
    // `animation: none`, NOT `animation-duration: 0s`. The entrance primitives
    // fill `both`, and a zero-duration animation with `both` fill holds its
    // from-state forever — which is `opacity: 0`. Collapsing the duration
    // therefore does not still the surface, it erases it: every staggered region
    // stayed invisible, Playwright reported the hub cards as not visible, and the
    // screenshots were of blank panels that axe was happy to pass. `tokens.css`
    // documents this exact trap as the reason stillness removes the animation
    // rather than shortening it, and a screenshot harness has to obey it too.
    await page.addInitScript({
      content: `(function () {
        var inject = function () {
          var style = document.createElement('style');
          style.id = 'topo-motion-reset';
          style.textContent = '*,*::before,*::after{animation:none!important;transition-duration:0s!important;transition-delay:0s!important;scroll-behavior:auto!important;}';
          document.head.appendChild(style);
        };
        if (document.head) inject();
        else document.addEventListener('DOMContentLoaded', inject);
      })()`,
    });

    await page.goto(baseURL, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('nav[aria-label="Workspaces"]', { timeout: 30_000 });

    const capture = async (state: string, theme: Theme, width: number): Promise<void> => {
      const file = `${state}__${theme}__${width}.png`;
      try {
        const buf = await page.screenshot({ path: path.join(OUT_DIR, file), fullPage: true });
        const axe = await runAxe(page);
        axeTotals.violations += axe.violations;
        for (const [impact, n] of Object.entries(axe.byImpact)) {
          axeTotals.byImpact[impact] = (axeTotals.byImpact[impact] ?? 0) + n;
        }
        shots.push({ state, theme, width, file, bytes: buf.length, axe });
        console.log(`[topo] ${file}  axe=${axe.violations}`);
      } catch (err) {
        shots.push({ state, theme, width, file, bytes: 0, axe: null, error: String(err) });
        console.warn(`[topo] FAILED ${file}: ${String(err)}`);
      }
    };

    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: VIEWPORT_HEIGHT });
      for (const theme of THEMES) {
        await setTheme(page, theme);

        await gotoSurface(page, '/code');
        await setTheme(page, theme);
        await ensureSpine(page);
        await capture('spine', theme, width);

        const opened = await openTrace(page);
        if (!opened) {
          console.warn(`[topo] TRACE did not open at ${theme}/${width}`);
          continue;
        }
        await setMotion(page, 'full');
        await capture('trace', theme, width);

        if (await hoverField(page)) await capture('trace-hover', theme, width);

        await setMotion(page, 'reduced');
        await sleep(400);
        await capture('trace-reduced', theme, width);
        await setMotion(page, 'system');

        // Reversibility: the drill-in has to hand the spine back.
        const back = backToSpine(page);
        if ((await back.count()) > 0) {
          await back.first().click();
          await sleep(700);
          await capture('spine-return', theme, width);
        }
      }
    }

    // Read the type system once, at the showcase width in dark, with the
    // richest surface on screen.
    await page.setViewportSize({ width: 1440, height: VIEWPORT_HEIGHT });
    await setTheme(page, 'dark');
    await gotoSurface(page, '/code');
    typography = await readTypography(page);

    // Does the app's own control actually reach the stylesheet?
    await openTrace(page);
    await setMotion(page, 'reduced');
    const reducedState = await page.evaluate(() => ({
      motionAttribute: document.documentElement.dataset['motion'] ?? null,
      enter: getComputedStyle(document.documentElement).getPropertyValue('--anim-enter').trim(),
      signal: getComputedStyle(document.documentElement).getPropertyValue('--anim-signal').trim(),
      panel: getComputedStyle(document.documentElement).getPropertyValue('--dur-panel').trim(),
    }));
    await setMotion(page, 'full');
    const fullState = await page.evaluate(() => ({
      motionAttribute: document.documentElement.dataset['motion'] ?? null,
      enter: getComputedStyle(document.documentElement).getPropertyValue('--anim-enter').trim(),
      signal: getComputedStyle(document.documentElement).getPropertyValue('--anim-signal').trim(),
      panel: getComputedStyle(document.documentElement).getPropertyValue('--dur-panel').trim(),
    }));
    motionProbe = { reduced: reducedState, full: fullState };
  } finally {
    if (browser) await browser.close();
    server?.server.close();
  }

  const failed = shots.filter((s) => s.error);
  const manifest = {
    generatedAt: new Date().toISOString(),
    baseURL,
    themes: THEMES,
    widths: WIDTHS,
    viewportHeight: VIEWPORT_HEIGHT,
    screenshotCount: shots.filter((s) => !s.error).length,
    axeSummary: axeTotals,
    externalRequests: [...externalRequests],
    pageErrors: [...new Set(pageErrors)],
    typography,
    motionProbe,
    shots,
  };
  writeFileSync(path.join(OUT_DIR, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  console.log('');
  console.log('[topo] ===== summary =====');
  console.log(`[topo] screenshots=${manifest.screenshotCount}  out=${path.relative(ROOT, OUT_DIR)}/`);
  console.log(
    `[topo] axe violations=${axeTotals.violations} byImpact=${JSON.stringify(axeTotals.byImpact)}`,
  );
  console.log(
    `[topo] external requests=${externalRequests.size}` +
      (externalRequests.size > 0 ? ` ${[...externalRequests].join(', ')}` : ' (offline-clean)'),
  );
  console.log(`[topo] page errors=${manifest.pageErrors.length}`);
  for (const error of manifest.pageErrors) console.error(`[topo]   ${error}`);
  if (failed.length > 0) {
    console.error(`[topo] ${failed.length} state(s) failed to capture`);
    process.exitCode = 1;
  }
  if (manifest.pageErrors.length > 0) {
    console.error('[topo] a surface threw during the walk — the gate is not clean');
    process.exitCode = 1;
  }
  // A remote font or script is a product defect on an offline install, so it
  // fails the run rather than being noted in a manifest nobody reads.
  if (externalRequests.size > 0) {
    console.error('[topo] non-local requests detected — offline posture broken');
    process.exitCode = 1;
  }
  if (axeTotals.violations > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error('[topo] fatal:', err);
  process.exit(1);
});
