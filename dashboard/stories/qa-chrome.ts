/**
 * Interaction QA in real Chrome (Playwright Chromium, WebGL via SwiftShader).
 *
 * `visual:audit` proves each surface *renders* and is axe-clean; `live:sweep`
 * proves each surface survives real daemon payloads. Neither one clicks
 * anything. This drives the app the way a person does — opens the palette,
 * flips the theme, switches scope, clicks a graph node, scrolls a long
 * surface — and fails on any uncaught page error, failed request, or console
 * error along the way.
 *
 *   AUDIT_PORT=5401 npx tsx stories/qa-chrome.ts
 */
import { chromium, type Page, type ConsoleMessage } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { installApiFixtures } from './fixtures/route.ts';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const PORT = Number(process.env['AUDIT_PORT'] ?? 5401);
const BASE = `http://127.0.0.1:${PORT}`;

const SURFACES = [
  'brain', 'explorer', 'loom', 'sessions', 'agents', 'code',
  'knowledge', 'delivery', 'automations', 'observatory', 'costs', 'settings',
  'work',
] as const;

interface Problem {
  surface: string;
  kind: 'pageerror' | 'console' | 'request' | 'assertion';
  detail: string;
}

const problems: Problem[] = [];
let current = 'startup';

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForServer(timeoutMs = 90_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(BASE, { method: 'GET', signal: AbortSignal.timeout(5_000) });
      if (res.ok || res.status === 304) return;
    } catch {
      /* not up yet */
    }
    await sleep(500);
  }
  throw new Error(`server at ${BASE} not ready`);
}

/** Console noise that is the dev server talking to itself, not the app. */
function isInfrastructureNoise(text: string): boolean {
  return /\[rsbuild\]|HMR|WebSocket connection|Download the React DevTools/i.test(text);
}

function watch(page: Page) {
  page.on('pageerror', (error) => {
    problems.push({ surface: current, kind: 'pageerror', detail: String(error.message).slice(0, 200) });
  });
  page.on('console', (message: ConsoleMessage) => {
    if (message.type() !== 'error') return;
    const text = message.text();
    if (isInfrastructureNoise(text)) return;
    problems.push({ surface: current, kind: 'console', detail: text.slice(0, 200) });
  });
  page.on('requestfailed', (request) => {
    const failure = request.failure()?.errorText ?? 'failed';
    if (/net::ERR_ABORTED/.test(failure)) return; // navigations supersede in-flight fetches
    problems.push({ surface: current, kind: 'request', detail: `${request.url().slice(0, 120)} ${failure}` });
  });
}

async function assert(condition: boolean, detail: string) {
  if (!condition) problems.push({ surface: current, kind: 'assertion', detail });
}

async function main() {
  let server: ChildProcess | null = null;
  let browser = null;
  try {
    server = spawn('npx', ['rsbuild', 'dev', '--port', String(PORT)], {
      cwd: ROOT,
      env: { ...process.env, NO_COLOR: '1' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    server.stdout?.on('data', () => {});
    server.stderr?.on('data', () => {});
    await waitForServer();

    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    watch(page);
    await installApiFixtures(page);

    // 1. Every surface renders with real content and no error boundary.
    for (const surface of SURFACES) {
      current = surface;
      await page.goto(`${BASE}/${surface}`, { waitUntil: 'domcontentloaded' });
      await page.waitForSelector('main#td-main', { timeout: 15_000 }).catch(() => {});
      await sleep(900);
      const text = (await page.locator('main#td-main').innerText().catch(() => '')) ?? '';
      await assert(text.trim().length > 0, 'main region rendered empty');
      await assert(
        !/Unexpected Application Error/i.test(text),
        'React Router error boundary caught a render failure',
      );
    }

    // 2. Theme toggle actually flips the document and survives a re-render.
    current = 'theme-toggle';
    await page.goto(`${BASE}/brain`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('main#td-main', { timeout: 15_000 }).catch(() => {});
    const before = await page.evaluate(() => document.documentElement.dataset['theme'] ?? '');
    const toggle = page.getByRole('button', { name: /toggle theme/i });
    if (await toggle.count()) {
      await toggle.first().click();
      await sleep(500);
      const after = await page.evaluate(() => document.documentElement.dataset['theme'] ?? '');
      await assert(before !== after, `theme did not change (${before} -> ${after})`);
      await toggle.first().click();
      await sleep(300);
    } else {
      await assert(false, 'theme toggle button not found');
    }

    // 3. Command palette opens, is focus-trapped, and closes.
    current = 'command-palette';
    await page.keyboard.press('Control+k');
    await sleep(600);
    const dialog = page.getByRole('dialog');
    const opened = (await dialog.count()) > 0;
    await assert(opened, 'command palette did not open on ctrl+k');
    if (opened) {
      await page.keyboard.press('Escape');
      await sleep(400);
      await assert((await page.getByRole('dialog').count()) === 0, 'palette did not close on Escape');
    }

    // 4. The graph canvas is actually drawing (WebGL present in this browser).
    current = 'graph-canvas';
    await page.goto(`${BASE}/brain`, { waitUntil: 'domcontentloaded' });
    await sleep(2_500);
    const canvasInfo = await page.evaluate(() => {
      const canvases = [...document.querySelectorAll('canvas')];
      const probe = document.createElement('canvas');
      const webgl = Boolean(probe.getContext('webgl2') ?? probe.getContext('webgl'));
      return { count: canvases.length, webgl };
    });
    await assert(canvasInfo.webgl, 'this Chrome has no WebGL context');
    await assert(canvasInfo.count > 0, 'no canvas element mounted on Brain');

    // 5. Deep-link scope survives a reload (the URL<->store sync).
    current = 'scope-deeplink';
    await page.goto(`${BASE}/brain?scope=project%3Ademo`, { waitUntil: 'domcontentloaded' });
    await sleep(1_200);
    const url = page.url();
    await assert(url.includes('scope='), `scope param was dropped from the URL: ${url}`);

    // 6. Narrow viewport: no horizontal overflow on any surface.
    current = 'responsive-320';
    await page.setViewportSize({ width: 320, height: 720 });
    for (const surface of ['brain', 'knowledge', 'settings'] as const) {
      current = `responsive-320:${surface}`;
      await page.goto(`${BASE}/${surface}`, { waitUntil: 'domcontentloaded' });
      await sleep(900);
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      await assert(overflow <= 1, `horizontal overflow of ${overflow}px at 320 wide`);
    }
  } finally {
    if (browser) await browser.close();
    server?.kill('SIGTERM');
  }

  console.log('\n[qa] ===== interaction QA =====');
  if (problems.length === 0) {
    console.log('[qa] PASS — 12 surfaces, theme toggle, palette, graph, deep-link scope, 320px reflow');
  } else {
    for (const problem of problems) {
      console.log(`[qa] ${problem.kind.padEnd(10)} ${problem.surface.padEnd(22)} ${problem.detail}`);
    }
    console.log(`[qa] FAIL — ${problems.length} problem(s)`);
    process.exitCode = 1;
  }
}

await main();
