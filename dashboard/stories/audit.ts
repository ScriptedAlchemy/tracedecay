/**
 * Visual-audit + accessibility harness (plan 11a R56-R59, plan 11 R64).
 *
 * `npm run visual:audit`
 *   Starts the dashboard (rsbuild dev) with every `/api` call served from the
 *   MSW-backed fixtures (`fixtures/route.ts`) — no daemon required — then walks
 *   the code-driven story registry and captures each surface across
 *   light+dark themes and 320/768/1440 widths, writing PNGs and a machine
 *   `manifest.json` to `audit-gallery/`. An axe accessibility scan runs per
 *   surface and its violations are recorded in the manifest.
 *
 * `npm run visual:audit -- --diff`
 *   Same capture, then pixelmatch every screenshot against the committed
 *   baseline of the same name in `audit-baselines/`, recording mismatched
 *   pixel counts (and writing diff PNGs) in the manifest.
 *
 * Env:
 *   AUDIT_BASE_URL   Audit an already-running server at this URL instead of
 *                    building and serving the bundle.
 *   AXE_PORT         Static-server port (default 5241).
 */
import { mkdirSync, rmSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { chromium, type Browser, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';
import type { Server } from 'node:http';
import { STORY_SURFACES } from './registry.ts';
import { installApiFixtures } from './fixtures/route.ts';
import { STILLNESS_INIT, startStaticServer } from '../e2e/axe-harness.ts';

const ROOT = process.cwd();
const GALLERY_DIR = path.join(ROOT, 'audit-gallery');
const DIFF_DIR = path.join(GALLERY_DIR, 'diffs');
const BASELINE_DIR = path.join(ROOT, 'audit-baselines');

const THEMES = ['light', 'dark'] as const;
const WIDTHS = [320, 768, 1440] as const;
const VIEWPORT_HEIGHT = 900;
const PORT = Number(process.env['AUDIT_PORT'] ?? 5173);
const DIFF_MODE = process.argv.slice(2).includes('--diff');

type Theme = (typeof THEMES)[number];
type Width = (typeof WIDTHS)[number];

interface AxeResult {
  violations: number;
  byImpact: Record<string, number>;
  ruleIds: string[];
}

interface DiffResult {
  baseline: string;
  status: 'match' | 'diff' | 'no-baseline' | 'size-mismatch';
  mismatchedPixels?: number;
  totalPixels?: number;
  ratio?: number;
  diffFile?: string;
}

interface ShotEntry {
  theme: Theme;
  width: Width;
  file: string;
  bytes: number;
  axe: AxeResult | null;
  diff?: DiffResult;
  error?: string;
}

interface SurfaceEntry {
  id: string;
  path: string;
  label: string;
  wired: boolean;
  shots: ShotEntry[];
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function waitForServer(baseURL: string, timeoutMs = 90_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    try {
      // Per-attempt timeout: a wedged server that accepts but never responds
      // must not hang the whole audit past the outer deadline.
      const res = await fetch(baseURL, { method: 'GET', signal: AbortSignal.timeout(5_000) });
      if (res.ok || res.status === 304) return;
    } catch (err) {
      lastErr = err;
    }
    await sleep(500);
  }
  throw new Error(`server at ${baseURL} not ready within ${timeoutMs}ms: ${String(lastErr)}`);
}


async function runAxe(page: Page): Promise<AxeResult> {
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa'])
    .analyze();
  const byImpact: Record<string, number> = {};
  const ruleIds: string[] = [];
  for (const v of results.violations) {
    const impact = v.impact ?? 'unknown';
    byImpact[impact] = (byImpact[impact] ?? 0) + 1;
    ruleIds.push(v.id);
  }
  return { violations: results.violations.length, byImpact, ruleIds };
}

function diffAgainstBaseline(file: string, shotBuf: Buffer): DiffResult {
  const baselinePath = path.join(BASELINE_DIR, file);
  if (!existsSync(baselinePath)) {
    return { baseline: file, status: 'no-baseline' };
  }
  const baseline = PNG.sync.read(readFileSync(baselinePath));
  const current = PNG.sync.read(shotBuf);
  if (baseline.width !== current.width || baseline.height !== current.height) {
    return {
      baseline: file,
      status: 'size-mismatch',
      totalPixels: current.width * current.height,
    };
  }
  const { width, height } = current;
  const diff = new PNG({ width, height });
  const mismatched = pixelmatch(baseline.data, current.data, diff.data, width, height, {
    threshold: 0.1,
  });
  const total = width * height;
  const result: DiffResult = {
    baseline: file,
    status: mismatched === 0 ? 'match' : 'diff',
    mismatchedPixels: mismatched,
    totalPixels: total,
    ratio: total === 0 ? 0 : mismatched / total,
  };
  if (mismatched > 0) {
    mkdirSync(DIFF_DIR, { recursive: true });
    const diffFile = `diff__${file}`;
    writeFileSync(path.join(DIFF_DIR, diffFile), PNG.sync.write(diff));
    result.diffFile = path.join('diffs', diffFile);
  }
  return result;
}

async function setTheme(page: Page, theme: Theme): Promise<void> {
  await page.evaluate((t) => {
    try {
      localStorage.setItem('td-theme', t);
    } catch {
      /* private mode / storage disabled — dataset alone still themes */
    }
    document.documentElement.dataset['theme'] = t;
  }, theme);
}

async function gotoSurface(page: Page, surfacePath: string): Promise<void> {
  // Faithful client-side navigation via the nav rail link (guaranteed present
  // for every registered surface). Falls back to a hard deep-link if the link
  // is not clickable in the current layout.
  const link = page.locator(`nav[aria-label="Workspaces"] a[href="${surfacePath}"]`);
  if ((await link.count()) > 0) {
    await link.first().click();
  } else {
    await page.goto(`http://localhost:${PORT}${surfacePath}`, { waitUntil: 'domcontentloaded' });
  }
  await page.waitForFunction((p) => location.pathname === p, surfacePath, { timeout: 15_000 });
  // Let react-query fixtures resolve and the layout settle.
  await page.waitForSelector('main#td-main', { timeout: 15_000 });
  await sleep(700);
}

async function main(): Promise<void> {
  rmSync(GALLERY_DIR, { recursive: true, force: true });
  mkdirSync(GALLERY_DIR, { recursive: true });

  // The BUILT bundle, not `rsbuild dev`. Lazy route compilation under the dev
  // server emits runtime errors the release build does not have (esbuild's
  // `__name` helper reaching the page among them), and a route that throws
  // still renders the router's accessible error boundary — which screenshots
  // happily and scores a clean axe pass. `AUDIT_BASE_URL` still wins, for
  // auditing an already-running server on purpose.
  const preset = process.env['AUDIT_BASE_URL'];
  let staticServer: Server | null = null;
  let baseURL: string;
  if (preset) {
    baseURL = preset;
  } else {
    const started = startStaticServer();
    staticServer = started.server;
    baseURL = started.baseURL;
  }

  console.log(`[audit] waiting for ${baseURL} ...`);
  await waitForServer(baseURL);
  console.log(`[audit] server ready; mode=${DIFF_MODE ? 'diff' : 'capture'}`);

  let browser: Browser | null = null;
  const surfaces: SurfaceEntry[] = [];
  const axeTotals = { violations: 0, byImpact: {} as Record<string, number> };
  const pageErrors: string[] = [];
  let screenshotCount = 0;

  try {
    // Chromium's bundled SwiftShader already gives headless runs conformant
    // WebGL (verified: ANGLE/SwiftShader), so the Sigma graph canvases render
    // for real here. Do not pin --use-angle: that would force software even on
    // a host that later has a GPU.
    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext({ deviceScaleFactor: 1 });
    const page = await context.newPage();
    // A crashed route renders the router's own accessible error boundary, which
    // screenshots happily and scores a clean axe pass. A page error therefore
    // fails the run rather than being invisible in the manifest.
    page.on('pageerror', (error) => {
      pageErrors.push(error.message);
      console.error(`[audit] PAGEERROR ${error.message}`);
    });
    await installApiFixtures(page);
    // Passed as source text, not a function: tsx compiles callbacks with
    // esbuild's `keepNames`, whose `__name` helper does not exist in the page.
    // As a function this threw `__name is not defined` on every run, so the
    // motion reset never applied and every capture was taken mid-animation —
    // silently, because nothing failed the run on a page error.
    await page.addInitScript({ content: STILLNESS_INIT });

    await page.goto(baseURL, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('nav[aria-label="Workspaces"]', { timeout: 30_000 });

    const surfaceMap = new Map<string, SurfaceEntry>();
    for (const s of STORY_SURFACES) {
      surfaceMap.set(s.id, { id: s.id, path: s.path, label: s.label, wired: s.wired, shots: [] });
    }

    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: VIEWPORT_HEIGHT });
      for (const theme of THEMES) {
        await setTheme(page, theme);
        for (const surface of STORY_SURFACES) {
          const file = `${surface.id}__${theme}__${width}.png`;
          const entry = surfaceMap.get(surface.id)!;
          try {
            await gotoSurface(page, surface.path);
            await setTheme(page, theme); // reassert after navigation
            const buf = await page.screenshot({
              path: path.join(GALLERY_DIR, file),
              fullPage: true,
            });
            screenshotCount += 1;
            const axe = await runAxe(page);
            axeTotals.violations += axe.violations;
            for (const [impact, n] of Object.entries(axe.byImpact)) {
              axeTotals.byImpact[impact] = (axeTotals.byImpact[impact] ?? 0) + n;
            }
            const shot: ShotEntry = { theme, width, file, bytes: buf.length, axe };
            if (DIFF_MODE) shot.diff = diffAgainstBaseline(file, buf as Buffer);
            entry.shots.push(shot);
            console.log(
              `[audit] ${file}  axe=${axe.violations}` +
                (shot.diff ? `  diff=${shot.diff.status}` : ''),
            );
          } catch (err) {
            entry.shots.push({ theme, width, file, bytes: 0, axe: null, error: String(err) });
            console.warn(`[audit] FAILED ${file}: ${String(err)}`);
          }
        }
      }
    }
    for (const s of STORY_SURFACES) surfaces.push(surfaceMap.get(s.id)!);
  } finally {
    if (browser) await browser.close();
    staticServer?.close();
  }

  const manifest = {
    generatedAt: new Date().toISOString(),
    baseURL,
    mode: DIFF_MODE ? 'diff' : 'capture',
    themes: THEMES,
    widths: WIDTHS,
    viewportHeight: VIEWPORT_HEIGHT,
    surfaceCount: STORY_SURFACES.length,
    screenshotCount,
    expectedScreenshotCount: STORY_SURFACES.length * THEMES.length * WIDTHS.length,
    axeSummary: axeTotals,
    surfaces,
  };
  writeFileSync(path.join(GALLERY_DIR, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  console.log('');
  console.log('[audit] ===== summary =====');
  console.log(
    `[audit] surfaces=${manifest.surfaceCount} themes=${THEMES.length} widths=${WIDTHS.length}`,
  );
  console.log(
    `[audit] screenshots=${screenshotCount}/${manifest.expectedScreenshotCount}  gallery=${path.relative(ROOT, GALLERY_DIR)}/`,
  );
  console.log(
    `[audit] axe violations=${axeTotals.violations} byImpact=${JSON.stringify(axeTotals.byImpact)}`,
  );
  // Hoisted out of the `DIFF_MODE` block so the gate below can consult it.
  // Outside diff mode no shot carries a `diff`, so this is empty and the gate
  // is unaffected.
  const diffs = surfaces.flatMap((s) => s.shots).filter((sh) => sh.diff?.status === 'diff');
  if (DIFF_MODE) {
    const noBaseline = surfaces
      .flatMap((s) => s.shots)
      .filter((sh) => sh.diff?.status === 'no-baseline');
    console.log(
      `[audit] diff: changed=${diffs.length} no-baseline=${noBaseline.length} (baselines in ${path.relative(ROOT, BASELINE_DIR)}/)`,
    );
    if (noBaseline.length > 0) {
      // Not a failure: a surface added since the baselines were written has
      // nothing to drift from. It is still an uncompared shot, so it is said
      // out loud rather than folded into the changed count.
      console.warn(
        `[audit] ${noBaseline.length} shot(s) had no baseline and were not compared`,
      );
    }
  }

  // THE GATE. This used to fail only on shots that could not be rendered, so a
  // run could report accessibility violations in its own summary and still exit
  // 0 — which is what every CI runner and every reviewer reads. Recording a
  // violation is not the same as failing on one.
  //
  // Pixel drift was left behind by that same fix: every baseline really is
  // compared with pixelmatch, the changed count really is printed, and then the
  // exit code ignored it — so a visual regression was measured, reported, and
  // waved through. It counts now, which is what `11a-dashboard-design.md` has
  // been claiming all along.
  const failed = surfaces.flatMap((s) => s.shots).filter((sh) => sh.error);
  if (failed.length > 0) {
    console.error(`[audit] ${failed.length} shot(s) failed to render`);
  }
  if (pageErrors.length > 0) {
    console.error(`[audit] ${pageErrors.length} page error(s)`);
  }
  if (diffs.length > 0) {
    console.error(`[audit] ${diffs.length} shot(s) drifted from their baseline`);
  }
  if (
    failed.length > 0 ||
    pageErrors.length > 0 ||
    axeTotals.violations > 0 ||
    diffs.length > 0
  ) {
    process.exitCode = 1;
  }
}

main().catch((err) => {
  console.error('[audit] fatal:', err);
  process.exit(1);
});
