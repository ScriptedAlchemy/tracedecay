/**
 * Print the offending nodes for a surface's axe violations.
 *
 * `stories/audit.ts` records violation COUNTS in its manifest, which tells you
 * a surface regressed but not which element did it. This runs the same axe
 * pass against the same fixture-backed dev server and prints each violation's
 * failure summary and target, so a contrast or focus failure can be fixed
 * without guessing.
 *
 *   npx tsx stories/axe-detail.ts /agents light 1440
 */
import { spawn } from 'node:child_process';
import { chromium } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { installApiFixtures } from './fixtures/route.ts';

const route = process.argv[2] ?? '/brain';
const theme = process.argv[3] ?? 'dark';
const width = Number(process.argv[4] ?? '1440');
const port = Number(process.env['AXE_PORT'] ?? '5455');

const server = spawn('npx', ['rsbuild', 'dev', '--port', String(port)], {
  cwd: process.cwd(),
  stdio: 'ignore',
  env: { ...process.env, NODE_ENV: 'development' },
});

const base = `http://127.0.0.1:${port}`;
const deadline = Date.now() + 90_000;
while (Date.now() < deadline) {
  try {
    if ((await fetch(base)).ok) break;
  } catch {
    /* not up yet */
  }
  await new Promise((resolve) => setTimeout(resolve, 400));
}

const browser = await chromium.launch({ headless: true });
// axe-core/playwright refuses a page created straight off the browser, so the
// context is explicit here exactly as it is in `audit.ts`.
const context = await browser.newContext({ viewport: { width, height: 900 } });
const page = await context.newPage();
await installApiFixtures(page);
await page.goto(`${base}${route}`, { waitUntil: 'domcontentloaded' });
await page.evaluate((value) => {
  try {
    localStorage.setItem('td-theme', value);
  } catch {
    /* storage disabled */
  }
  document.documentElement.dataset['theme'] = value;
}, theme);
await page.waitForSelector('main#td-main', { timeout: 20_000 }).catch(() => {});
await page.waitForTimeout(3_000);

const results = await new AxeBuilder({ page }).analyze();
for (const violation of results.violations) {
  console.log(`\n== ${violation.id} (${violation.impact}) — ${violation.help}`);
  for (const node of violation.nodes) {
    console.log('   target:', node.target.join(' '));
    console.log('   summary:', (node.failureSummary ?? '').replace(/\n/g, ' | '));
    console.log('   html:', node.html.slice(0, 220));
  }
}
if (results.violations.length === 0) console.log('no violations');

await browser.close();
server.kill('SIGTERM');
process.exit(0);
