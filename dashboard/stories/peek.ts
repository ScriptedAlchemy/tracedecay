/**
 * Design-iteration shot against the fixture-backed dev server.
 *
 * `stories/shot.ts` points a browser at a URL and screenshots it, which needs a
 * daemon serving a BUILT bundle — no use while iterating on source. This spawns
 * `rsbuild dev` over the working tree, serves every `/api` call from the same
 * fixtures the audit uses, and shoots the surfaces named on the command line:
 *
 *   npx tsx stories/peek.ts out-dir /brain /brain?scope=tracedecay
 */
import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { chromium } from '@playwright/test';
import { installApiFixtures } from './fixtures/route.ts';

const outDir = process.argv[2] ?? 'peek';
const targets = process.argv.slice(3);
const port = Number(process.env['PEEK_PORT'] ?? 5299);
mkdirSync(outDir, { recursive: true });

const server = spawn('npx', ['rsbuild', 'dev', '--port', String(port)], {
  cwd: process.cwd(),
  stdio: 'ignore',
  env: { ...process.env, NODE_ENV: 'development' },
});

async function waitForServer(url: string, timeoutMs = 90_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      /* not up yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
  throw new Error(`dev server did not come up on ${url}`);
}

const base = `http://127.0.0.1:${port}`;
await waitForServer(base);

const browser = await chromium.launch({ headless: true });
for (const target of targets) {
  const [route, themeAndWidth] = target.split('|');
  const [rawTheme, rawWidth] = (themeAndWidth ?? '').split(',');
  const theme = rawTheme || 'dark';
  const width = Number(rawWidth || '1440');
  const page = await browser.newPage({ viewport: { width, height: 900 } });
  // With TRACEDECAY_DASHBOARD_API set, rsbuild proxies `/api` to that daemon
  // and the surface is shot against real data; otherwise fixtures are served.
  // PEEK_REGISTRY overrides just `/api/projects` with a payload captured from a
  // real daemon, so a composition can be reviewed against the actual registry
  // (dozens of repositories) without one running.
  if (!process.env['TRACEDECAY_DASHBOARD_API']) await installApiFixtures(page);
  // Registered AFTER the catch-all on purpose: Playwright runs the most
  // recently added matching handler first.
  const registryFile = process.env['PEEK_REGISTRY'];
  if (registryFile) {
    const body = readFileSync(registryFile, 'utf8');
    await page.route('**/api/projects', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body }),
    );
  }
  await page.goto(`${base}${route}`, { waitUntil: 'domcontentloaded' });
  await page.evaluate((t) => {
    try {
      localStorage.setItem('td-theme', t);
    } catch {
      /* storage disabled — the dataset alone still themes */
    }
    document.documentElement.dataset['theme'] = t;
  }, theme);
  await page.waitForSelector('main#td-main', { timeout: 20_000 }).catch(() => {});
  await page.waitForTimeout(3_000);
  const slug = route!.split('?')[0]!.replace(/[^a-z0-9]+/gi, '-').replace(/^-|-$/g, '');
  const scopeId = /scopeLabel=([^&]+)/.exec(route!)?.[1];
  const scoped = scopeId ? `-scoped-${decodeURIComponent(scopeId)}` : '';
  const name = `${slug || 'root'}${scoped}-${theme}-${width}.png`;
  await page.screenshot({ path: path.join(outDir, name) });
  console.log('wrote', path.join(outDir, name));
  await page.close();
}
await browser.close();
server.kill('SIGTERM');
process.exit(0);
